//! The raw-CAN log reader, against a virtual bus.
//!
//! Sends its own frames rather than shelling out to `cansend`, so it needs
//! can-utils nowhere and runs wherever a vcan interface exists.
//!
//! Skips when there is no bus: that means `modprobe vcan` was never run, which
//! is a setup gap rather than a regression. `scripts/native-sim-can-e2e.sh` is
//! the gate that fails loudly.
//!
//! **Every test here must use a well-separated identifier range.** `vcan0` is a
//! shared bus and cargo runs these concurrently, so two tests picking nearby ids
//! see each other's frames interleaved mid-line. That is not a flake to retry;
//! it is the bus behaving exactly as a bus does.

use std::io::{BufRead, BufReader};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::time::Duration;
use runtt_transport::can::CanLogReader;

const AF_CAN: libc::c_int = 29;
const CAN_RAW: libc::c_int = 1;

#[repr(C)]
#[derive(Default)]
struct SockAddrCanRaw {
    can_family: u16,
    _pad: u16,
    can_ifindex: i32,
    _unused: [u32; 2],
}

#[repr(C)]
#[derive(Default)]
struct CanFrame {
    can_id: u32,
    len: u8,
    _pad: u8,
    _res0: u8,
    _len8_dlc: u8,
    data: [u8; 8],
}

fn have_bus(iface: &str) -> bool {
    std::path::Path::new("/sys/class/net").join(iface).is_dir()
}

/// A raw CAN sender, standing in for the device's log backend.
fn sender(iface: &str) -> OwnedFd {
    let cname = std::ffi::CString::new(iface).unwrap();
    let ifindex = unsafe { libc::if_nametoindex(cname.as_ptr()) } as i32;
    assert!(ifindex > 0, "no interface {iface}");
    let fd = unsafe { libc::socket(AF_CAN, libc::SOCK_RAW, CAN_RAW) };
    assert!(fd >= 0, "socket: {}", std::io::Error::last_os_error());
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let addr = SockAddrCanRaw {
        can_family: AF_CAN as u16,
        _pad: 0,
        can_ifindex: ifindex,
        _unused: [0; 2],
    };
    let rc = unsafe {
        libc::bind(
            fd.as_raw_fd(),
            (&addr as *const SockAddrCanRaw).cast(),
            std::mem::size_of::<SockAddrCanRaw>() as libc::socklen_t,
        )
    };
    assert!(rc >= 0, "bind: {}", std::io::Error::last_os_error());
    fd
}

fn send_text(fd: &OwnedFd, id: u32, text: &str) {
    for chunk in text.as_bytes().chunks(8) {
        let mut frame = CanFrame {
            can_id: id,
            len: chunk.len() as u8,
            ..Default::default()
        };
        frame.data[..chunk.len()].copy_from_slice(chunk);
        let n = unsafe {
            libc::write(
                fd.as_raw_fd(),
                (&frame as *const CanFrame).cast(),
                std::mem::size_of::<CanFrame>(),
            )
        };
        assert!(n > 0, "write: {}", std::io::Error::last_os_error());
    }
}

#[test]
fn log_frames_reassemble_into_lines() {
    let iface = "vcan0";
    if !have_bus(iface) {
        eprintln!("skipping: no {iface}; run `sudo modprobe vcan && sudo ip link add dev {iface} type vcan && sudo ip link set {iface} up`");
        return;
    }
    const LOG_ID: u32 = 0x100;

    let reader = CanLogReader::open(iface, LOG_ID, Duration::from_secs(5)).expect("open reader");
    let tx = sender(iface);

    // Deliberately not aligned to the 8-byte frame size: a line must reassemble
    // across frames, and a frame must be allowed to straddle two lines.
    let sent = "alive, tick 0\nalive, tick 1\nthis line is comfortably longer than eight bytes\n";
    let expected: Vec<&str> = sent.trim_end().split('\n').collect();

    let handle = std::thread::spawn(move || {
        let mut out = Vec::new();
        for line in BufReader::new(reader).lines() {
            match line {
                Ok(l) => {
                    out.push(l);
                    if out.len() == 3 {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        out
    });

    // Give the reader a moment to reach its first read before anything is sent;
    // a raw CAN socket has no backlog for frames that predate it.
    std::thread::sleep(Duration::from_millis(200));
    send_text(&tx, LOG_ID, sent);

    let got = handle.join().expect("reader thread");
    assert_eq!(got, expected);
}

#[test]
fn frames_on_other_ids_are_filtered_out() {
    let iface = "vcan0";
    if !have_bus(iface) {
        eprintln!("skipping: no {iface}");
        return;
    }
    const LOG_ID: u32 = 0x200;

    let reader = CanLogReader::open(iface, LOG_ID, Duration::from_millis(700)).expect("open");
    let tx = sender(iface);

    let handle = std::thread::spawn(move || {
        let mut out = Vec::new();
        for line in BufReader::new(reader).lines().map_while(Result::ok) {
            out.push(line);
            if out.len() == 1 {
                break;
            }
        }
        out
    });

    std::thread::sleep(Duration::from_millis(200));
    // The management ids either side must not leak into the log stream: the
    // kernel filter is what keeps a busy bus from reaching userspace at all.
    send_text(&tx, LOG_ID - 2, "management traffic\n");
    send_text(&tx, LOG_ID - 1, "a device reply\n");
    send_text(&tx, LOG_ID, "the only line that counts\n");

    let got = handle.join().expect("reader thread");
    assert_eq!(got, vec!["the only line that counts"]);
}
