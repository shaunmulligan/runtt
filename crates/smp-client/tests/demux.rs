//! The single-channel demux: application logs and SMP traffic on one link.

use smp_client::demux::{is_smp_line, LogDemux, MARKER_CONT, MARKER_START};
use std::io::{Read, Write};
use std::sync::mpsc;
use std::time::Duration;
use transport::usb::SerialChannel;

#[test]
fn classifies_lines_by_their_framing_marker() {
    let mut start = MARKER_START.to_vec();
    start.extend_from_slice(b"AAECAwQ=");
    assert!(is_smp_line(&start), "0x06 0x09 opens an SMP packet");

    let mut cont = MARKER_CONT.to_vec();
    cont.extend_from_slice(b"BQYHCA==");
    assert!(is_smp_line(&cont), "0x04 0x14 continues one");

    assert!(!is_smp_line(b"[00:00:01.000,000] <inf> app: alive, tick 3"));
    assert!(!is_smp_line(b""), "an empty line is not a frame");
    // The marker must lead, not merely appear.
    assert!(!is_smp_line(b"log: \x06\x09 embedded"));
}

/// Feed a link that carries both kinds of traffic and assert each half lands
/// where it should: log lines out to the sink, SMP bytes through to the client.
#[test]
fn routes_log_lines_to_the_sink_and_smp_bytes_to_the_reader() {
    let (host, mut device) = SerialChannel::pty_pair().expect("pty pair");
    let (tx, logs) = mpsc::channel();

    let mut demux = LogDemux::with_sink(host, Duration::from_secs(2), move |line| {
        let _ = tx.send(String::from_utf8_lossy(line).into_owned());
    })
    .expect("demux");

    // Interleaved exactly as a real single-channel device emits: the app is
    // logging while a management exchange is in flight.
    let mut wire = Vec::new();
    wire.extend_from_slice(b"<inf> app: booting\r\n");
    wire.extend_from_slice(&MARKER_START);
    wire.extend_from_slice(b"AAECAwQ=\n");
    wire.extend_from_slice(b"<inf> app: alive, tick 1\n");
    wire.extend_from_slice(&MARKER_CONT);
    wire.extend_from_slice(b"BQYHCA==\n");
    device.write_all(&wire).expect("write");
    device.flush().expect("flush");

    // The SMP client must see the frame lines, and nothing else.
    let mut got = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut buf = [0u8; 64];
    while got.len() < 21 && std::time::Instant::now() < deadline {
        match demux.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => got.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
            Err(e) => panic!("read failed: {e}"),
        }
    }

    let mut expected = MARKER_START.to_vec();
    expected.extend_from_slice(b"AAECAwQ=\n");
    expected.extend_from_slice(&MARKER_CONT);
    expected.extend_from_slice(b"BQYHCA==\n");
    assert_eq!(got, expected, "the client must see only SMP bytes");

    let first = logs.recv_timeout(Duration::from_secs(2)).expect("log line");
    let second = logs.recv_timeout(Duration::from_secs(2)).expect("log line");
    assert_eq!(first, "<inf> app: booting", "CRLF must be trimmed");
    assert_eq!(second, "<inf> app: alive, tick 1");
}

/// A quiet link must time out rather than block forever, so the SMP client's
/// own retry and probe logic still works.
#[test]
fn a_silent_link_times_out_rather_than_hanging() {
    let (host, _device) = SerialChannel::pty_pair().expect("pty pair");
    let mut demux = LogDemux::with_sink(host, Duration::from_millis(200), |_| {}).expect("demux");

    let started = std::time::Instant::now();
    let err = demux.read(&mut [0u8; 8]).expect_err("must not block");
    assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "returned after {:?}",
        started.elapsed()
    );
}

/// Log output alone must never be mistaken for a frame, however much of it
/// there is — otherwise a chatty application would corrupt the SMP stream.
#[test]
fn heavy_log_traffic_never_leaks_into_the_smp_stream() {
    let (host, mut device) = SerialChannel::pty_pair().expect("pty pair");
    let mut demux = LogDemux::with_sink(host, Duration::from_millis(300), |_| {}).expect("demux");

    for i in 0..200 {
        writeln!(device, "<inf> app: tick {i}").expect("write");
    }
    device.flush().expect("flush");

    let err = demux
        .read(&mut [0u8; 64])
        .expect_err("no SMP frame was sent, so the read must time out");
    assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
}
