use sacn::packet::UNIVERSE_CHANNEL_CAPACITY;
use sacn::source::SacnSource;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;
use tokio::time::{MissedTickBehavior, interval};

const UNIVERSE: u16 = 1;
const TICK_INTERVAL_MS: u64 = 25;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bind_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0);
    let mut source = SacnSource::with_ip("opengobo-core", bind_addr)?;

    source.set_multicast_loop_v4(true)?;
    source.register_universe(UNIVERSE)?;

    let mut dmx_buffer = [0u8; UNIVERSE_CHANNEL_CAPACITY];

    let mut ticker = interval(Duration::from_millis(TICK_INTERVAL_MS));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        ticker.tick().await;

        dmx_buffer[1] = dmx_buffer[1].wrapping_add(1);

        source.send(&[UNIVERSE], &dmx_buffer, None, None, None)?;
    }
}
