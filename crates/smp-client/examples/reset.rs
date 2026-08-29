//! Send `os reset` and nothing else.
//!
//! The tightest isolation available for a board that stops responding around a
//! reset: no image_list, no set_state, no upload. If a board wedges under this,
//! the reset path alone is responsible and nothing about the deploy sequence
//! needs to be involved to explain it.
//!
//!   cargo run -p smp-client --example reset -- /dev/balena-mcu/<tag>-mgmt

use smp_client::{SmpClient, ToolkitClient};
use std::time::Duration;
use transport::usb::SerialChannel;

fn main() {
    let dev = std::env::args().nth(1).expect("usage: reset <device>");
    let ch = SerialChannel::open(&dev, 115_200, Duration::from_secs(3)).expect("open");
    let mut c = ToolkitClient::new(ch, Duration::from_secs(3)).expect("client");

    match c.echo("balena") {
        Ok(r) => println!("  echo -> {r:?} (device is alive)"),
        Err(e) => {
            println!("  echo failed before we even reset: {e:#}");
            std::process::exit(1);
        }
    }
    match c.reset() {
        Ok(()) => println!("  reset -> accepted"),
        Err(e) => println!("  reset FAILED: {e:#}"),
    }
}
