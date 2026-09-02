//! Speak SMP to a device on a CAN bus, over ISO-TP.
//!
//!   cargo run -p runtt-smp --example can-ping -- vcan0 0x42
//!
//! The counterpart to `ping`, for the other transport. Needs the `can-isotp`
//! kernel module and an interface that is UP; see docs/ROADMAP.md.

use runtt_smp::{can::IsoTpTransport, SmpClient, ToolkitClient};
use runtt_transport::can::IsoTpChannel;
use std::time::Duration;

fn main() {
    let mut args = std::env::args().skip(1);
    let iface = args.next().unwrap_or_else(|| "vcan0".into());
    let node = args.next().unwrap_or_else(|| "0x42".into());
    let node_id = u32::from_str_radix(node.trim_start_matches("0x"), 16)
        .unwrap_or_else(|_| panic!("node id should be hex, e.g. 0x42; got {node:?}"));

    let ch = match IsoTpChannel::open(&iface, node_id, Duration::from_secs(3)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("open {iface} node {node_id:#x}: {e:#}");
            std::process::exit(1);
        }
    };
    println!("  {} ", ch.describe());

    let mut c = ToolkitClient::from_transport(IsoTpTransport::new(ch), Duration::from_secs(3))
        .expect("client");

    match c.echo("runtt") {
        Ok(r) => println!("  echo -> {r:?}"),
        Err(e) => println!("  echo failed: {e:#}"),
    }
    match c.image_list() {
        Ok(s) if s.is_empty() => println!("  image list -> no images"),
        Ok(s) => {
            for i in s {
                println!(
                    "  slot {} active={} confirmed={} v{}",
                    i.slot, i.active, i.confirmed, i.version
                );
            }
        }
        Err(e) => println!("  image list failed: {e:#}"),
    }
    match c.describe() {
        Ok(d) => println!("  describe -> {d:?}"),
        Err(e) => println!("  describe failed: {e:#}"),
    }
}
