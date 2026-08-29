//! Ask a device whether it speaks SMP at all.
//!
//!     cargo run -p smp-client --example ping -- /dev/balena-mcu/probe-uart
//!
//! Useful when the thing on the other end might be a bootloader in recovery,
//! an application, or nothing.
use smp_client::{SmpClient, ToolkitClient};
use std::time::Duration;
use transport::usb::SerialChannel;

fn main() {
    let dev = std::env::args().nth(1).expect("usage: ping <device>");
    let ch = match SerialChannel::open(&dev, 115_200, Duration::from_secs(3)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("open {dev}: {e:#}");
            std::process::exit(1);
        }
    };
    let mut c = ToolkitClient::new(ch, Duration::from_secs(3)).expect("client");

    match c.echo("balena") {
        Ok(r) => println!("  echo -> {r:?}"),
        Err(e) => println!("  echo failed: {e:#}"),
    }
    match c.image_list() {
        Ok(slots) if slots.is_empty() => println!("  image list -> no images"),
        Ok(slots) => {
            for s in slots {
                // bootable and permanent are printed because they are the
                // fields that distinguish "staged and waiting" from "MCUboot
                // tried this and rejected it" -- the exact question a failed
                // swap raises, and one this tool could not previously answer.
                println!(
                    "  slot {} active={} confirmed={} pending={} bootable={} permanent={} v{} hash={}",
                    s.slot,
                    s.active,
                    s.confirmed,
                    s.pending,
                    s.bootable,
                    s.permanent,
                    s.version,
                    s.hash
                        .map(|h| h.iter().map(|b| format!("{b:02x}")).collect::<String>())
                        .unwrap_or_else(|| "<none>".into())
                );
            }
        }
        Err(e) => println!("  image list failed: {e:#}"),
    }
    match c.describe() {
        Ok(d) => println!("  describe -> {d:?}"),
        Err(e) => println!("  describe unsupported (expected from MCUboot): {}", e),
    }

    if std::env::args().any(|a| a == "--reset") {
        match c.reset() {
            Ok(()) => println!("  reset sent"),
            Err(e) => println!("  reset failed: {e:#}"),
        }
    }
}
