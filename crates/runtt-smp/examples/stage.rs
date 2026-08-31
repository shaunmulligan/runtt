//! Mark an image for test and read the trailer straight back.
//!
//! Splits "the device accepted set_state" from "the pending flag is actually
//! there", which `image list` after a reboot cannot distinguish from a swap
//! that ran and cleared it. By default it does NOT reset: the point is to
//! observe the trailer while the device is still up.
//!
//!   cargo run -p runtt-smp --example stage -- <dev> <hex-digest> [reset]
//!
//! Pass `reset` to send os reset afterwards and nothing else -- useful for
//! isolating whether the reset itself is what wedges a board, separately from
//! the upload that normally precedes it.

use runtt_smp::{SmpClient, ToolkitClient};
use std::time::Duration;
use runtt_transport::usb::SerialChannel;

fn main() {
    let mut args = std::env::args().skip(1);
    let dev = args.next().expect("usage: stage <device> <hex-digest>");
    let hex = args.next().expect("usage: stage <device> <hex-digest>");
    let digest: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex digest"))
        .collect();

    let ch = SerialChannel::open(&dev, 115_200, Duration::from_secs(30)).expect("open");
    let mut c = ToolkitClient::new(ch, Duration::from_secs(30)).expect("client");
    c.tune_frame_size();

    // Optional: upload an image first, so the whole stage can be driven without
    // the runtime and, crucially, without it also sending a reset. That lets
    // the reset be issued separately -- over SWD -- so a debugger can watch
    // what the bootloader does with the staged image.
    if let Ok(path) = std::env::var("STAGE_UPLOAD") {
        let image = std::fs::read(&path).expect("read image");
        println!("  uploading {} bytes from {path}", image.len());
        c.flash(&image, None).expect("upload failed");
        println!("  upload complete");
    }

    let show = |c: &mut ToolkitClient, when: &str| match c.image_list() {
        Ok(slots) => {
            for s in slots {
                println!(
                    "  [{when}] slot {} active={} confirmed={} pending={} bootable={} v{}",
                    s.slot, s.active, s.confirmed, s.pending, s.bootable, s.version
                );
            }
        }
        Err(e) => println!("  [{when}] image list failed: {e:#}"),
    };

    show(&mut c, "before");
    match c.set_state(&digest, false) {
        Ok(()) => println!("  set_state(test) -> accepted"),
        Err(e) => {
            println!("  set_state(test) FAILED: {e:#}");
            std::process::exit(1);
        }
    }
    show(&mut c, "after");

    if args.next().as_deref() == Some("reset") {
        match c.reset() {
            Ok(()) => println!("  reset -> accepted; the device should reboot now"),
            Err(e) => println!("  reset FAILED: {e:#}"),
        }
    }
}
