use clap::Parser;
use opengobo_server::{CoreCoordinator, ServerError};
use std::path::PathBuf;
use std::time::Duration;
use tokio::time;
use tracing::info;

#[derive(Debug, Parser)]
#[command(
    name = "opengobo-core",
    version,
    about = "OpenGobo real-time lighting engine and show server"
)]
struct Cli {
    #[arg(short, long, default_value = "default.gobo")]
    show: PathBuf,

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

    info!(
        show_title = %hello.show_title,
        show_file = %cli.show.display(),
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

    let hz = cli.tick_hz.clamp(1, 120);
    let mut ticker = time::interval(Duration::from_micros(1_000_000 / u64::from(hz)));

    info!(tick_hz = hz, "Starting real-time DMX mixer loop");

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let _universes = coordinator.engine().render_universes();
            }
            _ = tokio::signal::ctrl_c() => {
                info!("Shutting down OpenGobo core");
                break;
            }
        }
    }

    Ok(())
}
