use clap::Parser;
use opengobo_protocol::{
    ClientPacket, DEFAULT_DISCOVERY_PORT, DEFAULT_SERVER_PORT, DiscoveryPacket, ServerPacket,
    UserId,
};
use opengobo_server::{CoreCoordinator, ServerError};
use std::net::{Ipv4Addr, SocketAddrV4};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::Mutex;
use tokio::time;
use tracing::{info, warn};
use uuid::Uuid;

const MULTICAST_GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 71, 66);

#[derive(Debug, Parser)]
#[command(
    name = "opengobo-core",
    version,
    about = "OpenGobo real-time lighting engine and show server"
)]
struct Cli {
    #[arg(short, long, default_value = "default.gobo")]
    show: PathBuf,

    #[arg(short, long, default_value_t = DEFAULT_SERVER_PORT)]
    port: u16,

    #[arg(long, default_value_t = DEFAULT_DISCOVERY_PORT)]
    discovery_port: u16,

    #[arg(long, default_value_t = 44)]
    tick_hz: u32,

    #[arg(long)]
    check_only: bool,
}

#[tokio::main]
async fn main() -> Result<(), ServerError> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    let coordinator = CoreCoordinator::open(&cli.show)?;
    let hello = coordinator.server_hello();
    let server_id = Uuid::new_v4().to_string();

    info!(
        show_title = %hello.show_title,
        show_file = %cli.show.display(),
        port = cli.port,
        discovery_port = cli.discovery_port,
        protocol_version = hello.protocol_version,
        "OpenGobo core initialized"
    );

    if let Some(pin) = coordinator.setup_pin() {
        info!(
            setup_pin = %pin,
            "First-Time Setup Mode active: enter this Setup-PIN in OpenGobo Client to create the initial Admin user."
        );
    }

    if cli.check_only {
        let universes = coordinator.engine().render_universes();
        info!(
            active_universes = universes.len(),
            "Completed verification tick (--check-only)"
        );
        return Ok(());
    }

    let shared = Arc::new(Mutex::new(coordinator));

    spawn_discovery_responder(Arc::clone(&shared), server_id, cli.port, cli.discovery_port).await;
    spawn_client_listener(Arc::clone(&shared), cli.port).await?;

    let hz = cli.tick_hz.clamp(1, 120);
    let mut ticker = time::interval(Duration::from_micros(1_000_000 / u64::from(hz)));

    info!(tick_hz = hz, "Starting real-time DMX mixer loop");

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let coord = shared.lock().await;
                let _universes = coord.engine().render_universes();
            }
            _ = tokio::signal::ctrl_c() => {
                info!("Shutting down OpenGobo core");
                break;
            }
        }
    }

    Ok(())
}

async fn spawn_discovery_responder(
    coordinator: Arc<Mutex<CoreCoordinator>>,
    server_id: String,
    server_port: u16,
    discovery_port: u16,
) {
    let bind_addr = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, discovery_port);
    let socket = match UdpSocket::bind(bind_addr).await {
        Ok(s) => s,
        Err(err) => {
            warn!("Could not bind UDP discovery socket on port {discovery_port}: {err}");
            return;
        }
    };
    let _ = socket.join_multicast_v4(MULTICAST_GROUP, Ipv4Addr::UNSPECIFIED);

    info!(discovery_port, "UDP auto-discovery responder active");

    tokio::spawn(async move {
        let mut buf = [0u8; 2048];
        while let Ok((len, peer)) = socket.recv_from(&mut buf).await {
            let Ok(DiscoveryPacket::DiscoverRequest) = serde_json::from_slice(&buf[..len]) else {
                continue;
            };
            let hello = coordinator.lock().await.server_hello();
            let reply = DiscoveryPacket::ServerAnnounce {
                server_id: server_id.clone(),
                server_port,
                hello,
            };
            if let Ok(bytes) = serde_json::to_vec(&reply) {
                let _ = socket.send_to(&bytes, peer).await;
            }
        }
    });
}

async fn spawn_client_listener(
    coordinator: Arc<Mutex<CoreCoordinator>>,
    port: u16,
) -> Result<(), ServerError> {
    let listener = TcpListener::bind(("0.0.0.0", port))
        .await
        .map_err(opengobo_storage::StorageError::Io)?;

    info!(port, "Client server listener active");

    tokio::spawn(async move {
        while let Ok((stream, peer)) = listener.accept().await {
            let coord = Arc::clone(&coordinator);
            tokio::spawn(async move {
                if let Err(err) = handle_client_stream(coord, stream).await {
                    warn!(%peer, "Client connection closed: {err}");
                }
            });
        }
    });

    Ok(())
}

async fn handle_client_stream(
    coordinator: Arc<Mutex<CoreCoordinator>>,
    stream: TcpStream,
) -> std::io::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    let mut authenticated_user: Option<UserId> = None;

    let hello_pkt = {
        let coord = coordinator.lock().await;
        ServerPacket::Hello(coord.server_hello())
    };
    send_packet(&mut writer, &hello_pkt).await?;

    while let Some(line) = lines.next_line().await? {
        let Ok(client_pkt) = serde_json::from_str::<ClientPacket>(&line) else {
            continue;
        };
        let (resp, _delta) = {
            let mut coord = coordinator.lock().await;
            coord.handle_client_packet(&mut authenticated_user, client_pkt)
        };
        send_packet(&mut writer, &resp).await?;
    }

    Ok(())
}

async fn send_packet(
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    packet: &ServerPacket,
) -> std::io::Result<()> {
    if let Ok(mut json) = serde_json::to_string(packet) {
        json.push('\n');
        writer.write_all(json.as_bytes()).await?;
        writer.flush().await?;
    }
    Ok(())
}
