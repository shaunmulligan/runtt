//! Watch a device's console arriving over CAN.
//!
//!     cargo run -p runtt-transport --example can-logs -- vcan0 0x42
//!
//! Takes the node id from the placement label and listens on `node_id + 2`,
//! the same derivation the runtime makes, so what this prints is exactly what
//! would reach the container's stdout.
use std::io::{BufRead, BufReader};
use std::time::Duration;
use runtt_transport::can::CanLogReader;

fn main() {
    let mut args = std::env::args().skip(1);
    let iface = args.next().unwrap_or_else(|| "vcan0".into());
    let node = args.next().unwrap_or_else(|| "0x42".into());
    let node_id = u32::from_str_radix(node.trim_start_matches("0x"), 16)
        .unwrap_or_else(|_| panic!("node id should be hex, e.g. 0x42; got {node:?}"));

    let reader = match CanLogReader::open(&iface, node_id + 2, Duration::from_secs(3600)) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e:#}");
            std::process::exit(1);
        }
    };
    eprintln!("listening: {}", reader.describe());
    for line in BufReader::new(reader).lines() {
        match line {
            Ok(l) => println!("{l}"),
            Err(e) => {
                eprintln!("log channel closed: {e}");
                break;
            }
        }
    }
}
