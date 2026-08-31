//! Live check against whatever is plugged in. Ignored by default; run with
//! `cargo test -p runtt-transport --test live_probe -- --ignored --nocapture`.
use runtt_transport::{resolve, Target};

#[test]
#[ignore]
fn describe_what_is_attached() {
    for tty in std::fs::read_dir("/sys/class/tty").unwrap() {
        let name = tty.unwrap().file_name().to_string_lossy().to_string();
        if !name.starts_with("ttyACM") && !name.starts_with("ttyUSB") {
            continue;
        }
        let dev = format!("/sys/class/tty/{name}/device");
        let real = std::fs::canonicalize(&dev).ok();
        let iface = std::fs::read_to_string(format!("{dev}/interface")).ok();
        eprintln!(
            "{name}: iface={:?} usb_iface_dir={:?}",
            iface.as_deref().map(str::trim),
            real.as_ref()
                .and_then(|p| p.file_name())
                .map(|s| s.to_string_lossy().to_string())
        );
    }
    for label in ["usb:3-6", "tty:ttyACM0"] {
        match Target::parse(label).map(|t| resolve::resolve(&t)) {
            Ok(Ok(r)) => eprintln!("{label} -> OK {r:?}"),
            Ok(Err(e)) => eprintln!("{label} -> ERR {e}"),
            Err(e) => eprintln!("{label} -> PARSE ERR {e}"),
        }
    }
}
