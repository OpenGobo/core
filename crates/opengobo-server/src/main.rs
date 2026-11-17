use clap::Parser;
use opengobo_protocol::{
    ClientPacket, DEFAULT_DISCOVERY_PORT, DEFAULT_SERVER_PORT, DiscoveryPacket, RealtimeDatagram,
    ServerPacket, StateDelta, UserId,
};
use opengobo_server::{CoreCoordinator, ServerError};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::UdpSocket;
use tokio::sync::{Mutex, broadcast};
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
    let (delta_tx, _) = broadcast::channel::<StateDelta>(256);

    spawn_discovery_responder(Arc::clone(&shared), server_id, cli.port, cli.discovery_port).await;
    spawn_quic_server(Arc::clone(&shared), delta_tx, quic_config, cli.port)?;

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
    delta_tx: broadcast::Sender<StateDelta>,
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
            let tx = delta_tx.clone();
            tokio::spawn(async move {
                match incoming.await {
                    Ok(conn) => handle_quic_connection(coord, tx, conn).await,
                    Err(err) => warn!("QUIC handshake failed: {err}"),
                }
            });
        }
    });

    Ok(())
}

async fn handle_quic_connection(
    coordinator: Arc<Mutex<CoreCoordinator>>,
    delta_tx: broadcast::Sender<StateDelta>,
    conn: quinn::Connection,
) {
    while let Ok((send, recv)) = conn.accept_bi().await {
        let coord = Arc::clone(&coordinator);
        let tx = delta_tx.clone();
        let conn_clone = conn.clone();
        tokio::spawn(async move {
            handle_persistent_stream(coord, tx, conn_clone, send, recv).await;
        });
    }
}

async fn handle_persistent_stream(
    coordinator: Arc<Mutex<CoreCoordinator>>,
    delta_tx: broadcast::Sender<StateDelta>,
    conn: quinn::Connection,
    mut send: quinn::SendStream,
    recv: quinn::RecvStream,
) {
    let mut authenticated_user: Option<UserId> = None;
    let mut delta_rx = delta_tx.subscribe();
    let mut lines = BufReader::new(recv).lines();

    let hello_pkt = {
        let coord = coordinator.lock().await;
        ServerPacket::Hello(coord.server_hello())
    };
    if write_quic_packet(&mut send, &hello_pkt).await.is_err() {
        return;
    }

    loop {
        tokio::select! {
            line_res = lines.next_line() => {
                let Ok(Some(line)) = line_res else {
                    break;
                };
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let Ok(client_pkt) = serde_json::from_str::<ClientPacket>(trimmed) else {
                    continue;
                };
                let (resp, maybe_delta) = {
                    let mut coord = coordinator.lock().await;
                    coord.handle_client_packet(&mut authenticated_user, client_pkt)
                };
                if write_quic_packet(&mut send, &resp).await.is_err() {
                    break;
                }
                if let Some(delta) = maybe_delta {
                    let _ = delta_tx.send(delta);
                }
            }
            delta_res = delta_rx.recv() => {
                if let Ok(delta) = delta_res
                    && authenticated_user.is_some()
                    && write_quic_packet(&mut send, &ServerPacket::Delta(delta)).await.is_err()
                {
                    break;
                }
            }
            dgram_res = conn.read_datagram() => {
                if let Ok(bytes) = dgram_res
                    && let Ok(dgram) = serde_json::from_slice::<RealtimeDatagram>(&bytes)
                {
                    let maybe_delta = {
                        let mut coord = coordinator.lock().await;
                        coord.handle_realtime_datagram(authenticated_user.as_deref(), dgram)
                    };
                    if let Some(delta) = maybe_delta {
                        let _ = delta_tx.send(delta);
                    }
                }
            }
        }
    }

    let _ = send.finish();
}

async fn write_quic_packet(
    send: &mut quinn::SendStream,
    packet: &ServerPacket,
) -> Result<(), String> {
    let mut json = serde_json::to_vec(packet).map_err(|e| e.to_string())?;
    json.push(b'\n');
    send.write_all(&json).await.map_err(|e| e.to_string())
}
