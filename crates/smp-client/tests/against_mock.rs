//! The client, driven against our own SMP server over a real pty pair.
//!
//! This is the load-bearing test of phase 0. The framing in `smp-mock` and the
//! framing in `mcumgr-toolkit` were written independently from the same Zephyr
//! spec, so agreement between them is strong evidence both are correct — much
//! stronger than either one's self-consistency.

use smp_client::{SmpClient, ToolkitClient};
use smp_mock::faults::Fault;
use smp_mock::server::Server;
use std::time::Duration;
use transport::usb::SerialChannel;

/// Stand up a mock on one end of a pty pair and a client on the other.
fn rig(fault: Fault) -> (ToolkitClient, std::thread::JoinHandle<()>) {
    let (host, device) = SerialChannel::pty_pair().expect("pty pair");

    let server = std::thread::spawn(move || {
        let mut dev = device;
        // A short read timeout keeps the loop responsive without busy-waiting.
        let _ = dev.as_mut().set_timeout(Duration::from_millis(50));
        let mut srv = Server::new(dev, fault);
        // The pipe closing when the client drops is the normal way out.
        let _ = srv.serve();
    });

    let client = ToolkitClient::new(host, Duration::from_secs(3)).expect("client");
    (client, server)
}

#[test]
fn echo_round_trips_through_our_own_framing() {
    let (mut c, _srv) = rig(Fault::None);
    let got = c.echo("balena").expect("echo should succeed");
    assert_eq!(got, "balena", "the device must echo the payload verbatim");
}

#[test]
fn a_freshly_provisioned_device_reports_one_confirmed_active_slot() {
    let (mut c, _srv) = rig(Fault::None);
    let slots = c.image_list().expect("image list");
    assert_eq!(
        slots.len(),
        1,
        "provisioned board has slot 0 only: {slots:?}"
    );
    let s0 = &slots[0];
    assert_eq!(s0.slot, 0);
    assert!(s0.active, "slot 0 is running");
    assert!(s0.confirmed, "a provisioned image is confirmed");
    assert!(!s0.pending);
    assert!(s0.hash.is_some(), "the device must report a digest");
}

#[test]
fn the_full_happy_path_upload_test_reset_confirm() {
    let (mut c, _srv) = rig(Fault::None);
    let image: Vec<u8> = (0..3072u32).map(|i| (i % 251) as u8).collect();

    c.flash(&image, None).expect("upload should succeed");

    // The staged image must appear in slot 1 with the digest we computed.
    let expected = ToolkitClient::digest(&image);
    let slots = c.image_list().expect("image list");
    let staged = slots
        .iter()
        .find(|s| s.slot == 1)
        .unwrap_or_else(|| panic!("slot 1 should be populated: {slots:?}"));
    assert_eq!(
        staged.hash.as_deref(),
        Some(expected.as_slice()),
        "the device's digest must match ours"
    );

    // Mark test — deliberately NOT confirmed. This is the safety invariant:
    // confirmation must only be reachable after the new image proves itself.
    c.set_state(&expected, false).expect("mark test");
    let slots = c.image_list().unwrap();
    assert!(
        slots.iter().find(|s| s.slot == 1).unwrap().pending,
        "test-marking must set pending"
    );

    c.reset().expect("reset");

    // The new image is now running but unconfirmed.
    let slots = c.image_list().expect("image list after reset");
    let running = slots.iter().find(|s| s.slot == 0).unwrap();
    assert_eq!(
        running.hash.as_deref(),
        Some(expected.as_slice()),
        "new image should run"
    );
    assert!(!running.confirmed, "it must not confirm itself");

    // Only now, having spoken SMP, may it be confirmed.
    c.set_state(&expected, true).expect("confirm");
    let slots = c.image_list().unwrap();
    assert!(slots.iter().find(|s| s.slot == 0).unwrap().confirmed);
}

#[test]
fn fault_bad_hash_is_reported_as_an_upload_failure() {
    let (mut c, _srv) = rig(Fault::BadHash);
    let image = vec![0x42u8; 2048];
    let err = c
        .flash(&image, None)
        .expect_err("a digest mismatch must fail the upload");
    let msg = format!("{err:#}").to_lowercase();
    assert!(
        msg.contains("hash") || msg.contains("match") || msg.contains("upload"),
        "error should name the cause, got: {err:#}"
    );
}

#[test]
fn fault_disconnect_mid_upload_surfaces_as_an_error_not_a_hang() {
    let (mut c, _srv) = rig(Fault::DisconnectMidUpload { after_chunks: 1 });
    let image = vec![0x11u8; 8192];
    let err = c
        .flash(&image, None)
        .expect_err("a silent device must not look like success");
    assert!(!format!("{err:#}").is_empty());
}

#[test]
fn fault_timeout_on_echo_surfaces_as_an_error() {
    let (mut c, _srv) = rig(Fault::Timeout { group: 0, cmd: 0 });
    assert!(
        c.echo("hello").is_err(),
        "a withheld response must time out"
    );
}

#[test]
fn fault_restart_upload_is_survived_by_the_client() {
    // The server demands off:0 partway through. A correct client re-sends len
    // and sha and completes the transfer.
    let (mut c, _srv) = rig(Fault::RestartUpload { at_offset: 512 });
    let image = vec![0x77u8; 4096];
    let result = c.flash(&image, None);
    assert!(
        result.is_ok(),
        "the client should honour the server's authoritative offset: {result:?}"
    );
}

#[test]
fn progress_is_reported_monotonically_to_completion() {
    struct Rec {
        seen: Vec<(u64, u64)>,
    }
    impl smp_client::Progress for Rec {
        fn advance(&mut self, uploaded: u64, total: u64) {
            self.seen.push((uploaded, total));
        }
    }
    let (mut c, _srv) = rig(Fault::None);
    let image = vec![0x33u8; 4096];
    let mut rec = Rec { seen: Vec::new() };
    c.flash(&image, Some(&mut rec)).expect("upload");

    assert!(
        !rec.seen.is_empty(),
        "progress should be reported at least once"
    );
    let mut last = 0;
    for (done, total) in &rec.seen {
        assert!(
            *done >= last,
            "progress must not go backwards: {:?}",
            rec.seen
        );
        assert_eq!(*total, image.len() as u64, "total should be the image size");
        last = *done;
    }
    assert_eq!(last, image.len() as u64, "progress must reach the total");
}
