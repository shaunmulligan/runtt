//! Diagnostic: capture the exact bytes mcumgr-toolkit puts on the wire.
use base64::Engine;
use std::io::Read;
use std::time::Duration;
use runtt_transport::usb::SerialChannel;

fn capture(op: &'static str) -> Option<Vec<u8>> {
    let (host, mut device) = SerialChannel::pty_pair().unwrap();
    let _ = device.as_mut().set_timeout(Duration::from_millis(1500));
    std::thread::spawn(move || {
        use runtt_smp::SmpClient;
        let mut c = runtt_smp::ToolkitClient::new(host, Duration::from_millis(600)).unwrap();
        match op {
            "echo" => {
                let _ = c.echo("hi");
            }
            "list" => {
                let _ = c.image_list();
            }
            "reset" => {
                let _ = c.reset();
            }
            "setstate" => {
                let _ = c.set_state(&[0u8; 32], true);
            }
            _ => {}
        }
    });
    std::thread::sleep(Duration::from_millis(250));
    let mut buf = vec![0u8; 4096];
    let n = device.read(&mut buf).unwrap_or(0);
    if n < 3 {
        return None;
    }
    let line: Vec<u8> = buf[..n]
        .iter()
        .copied()
        .filter(|b| *b != b'\n')
        .skip(2)
        .collect();
    base64::engine::general_purpose::STANDARD.decode(&line).ok()
}

#[test]
#[ignore]
fn compare_header_byte0_across_ops() {
    for op in ["echo", "list", "reset", "setstate"] {
        match capture(op) {
            Some(d) if d.len() >= 10 => {
                let h = &d[2..10];
                eprintln!(
                    "{op:9} byte0={:#04x} ({:08b})  group={:5} cmd={}  paylen={}",
                    h[0],
                    h[0],
                    u16::from_be_bytes([h[4], h[5]]),
                    h[7],
                    u16::from_be_bytes([h[2], h[3]])
                );
            }
            _ => eprintln!("{op:9} <no data>"),
        }
    }
}
