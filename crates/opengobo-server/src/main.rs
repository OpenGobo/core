use clap::Parser;
use opengobo_protocol::{
    ClientPacket, DEFAULT_DISCOVERY_PORT, DEFAULT_SERVER_PORT, DiscoveryPacket, ServerPacket,
    UserId,
};
use opengobo_server::{CoreCoordinator, ServerError};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;
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

    let mut coordinator = CoreCoordinator::open(&cli.show)?;
    let quic_config = coordinator.build_quic_server_config()?;
    let hello = coordinator.server_hello();
    let server_id = Uuid::new_v4().to_string();

    info!(
        show_title = %hello.show_title,
        show_file = %cli.show.display(),
        quic_port = cli.port,
        discovery_port = cli.discovery_port,
        protocol_version = hello.protocol_version,
        "OpenGobo core initialized (QUIC / TLS 1.3)"
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
    spawn_quic_server(Arc::clone(&shared), quic_config, cli.port)?;

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

fn spawn_quic_server(
    coordinator: Arc<Mutex<CoreCoordinator>>,
    server_config: quinn::ServerConfig,
    port: u16,
) -> Result<(), ServerError> {
    let bind_addr = SocketAddr::from((Ipv4Addr::UNSPECIFIED, port));
    let endpoint = quinn::Endpoint::server(server_config, bind_addr)
        .map_err(|e| ServerError::Tls(e.to_string()))?;

    info!(port, "QUIC / TLS 1.3 server endpoint listening");

    tokio::spawn(async move {
        while let Some(incoming) = endpoint.accept().await {
            let coord = Arc::clone(&coordinator);
            tokio::spawn(async move {
                match incoming.await {
                    Ok(conn) => handle_quic_connection(coord, conn).await,
                    Err(err) => warn!("QUIC handshake failed: {err}"),
                }
            });
        }
    });

    Ok(())
}

async fn handle_quic_connection(
    coordinator: Arc<Mutex<CoreCoordinator>>,
    conn: quinn::Connection,
) {
    let mut authenticated_user: Option<UserId> = None;

    while let Ok((mut send, mut recv)) = conn.accept_bi().await {
        let hello_pkt = {
            let coord = coordinator.lock().await;
            ServerPacket::Hello(coord.server_hello())
        };
        if write_quic_packet(&mut send, &hello_pkt).await.is_err() {
            break;
        }

        let Ok(payload) = recv.read_to_end(65_536).await else {
            break;
        };
        let trimmed = payload.trim_ascii();
        if !trimmed.is_empty()
            && let Ok(client_pkt) = serde_json::from_slice::<ClientPacket>(trimmed)
        {
            let (resp, _delta) = {
                let mut coord = coordinator.lock().await;
                coord.handle_client_packet(&mut authenticated_user, client_pkt)
            };
            let _ = write_quic_packet(&mut send, &resp).await;
        }

        let _ = send.finish();
    }
}

async fn write_quic_packet(
    send: &mut quinn::SendStream,
    packet: &ServerPacket,
) -> Result<(), String> {
    let mut json = serde_json::to_vec(packet).map_err(|e| e.to_string())?;
    json.push(b'\n');
    send.write_all(&json).await.map_err(|e| e.to_string())
}
