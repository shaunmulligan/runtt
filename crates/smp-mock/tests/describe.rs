//! The `describe` command, against the mock.
//!
//! Also checks the negative case, which is the one that matters in the field: a
//! board without our module must produce a clear error rather than a timeout.

use smp_client::describe::GROUP_PERUSER;
use smp_client::ToolkitClient;
use smp_mock::faults::Fault;
use smp_mock::server::Server;
use std::time::Duration;
use transport::usb::SerialChannel;

fn rig(fault: Fault) -> (ToolkitClient, std::thread::JoinHandle<()>) {
    let (host, device) = SerialChannel::pty_pair().expect("pty pair");
    let server = std::thread::spawn(move || {
        let mut dev = device;
        let _ = dev.as_mut().set_timeout(Duration::from_millis(50));
        let mut srv = Server::new(dev, fault);
        let _ = srv.serve();
    });
    let client = ToolkitClient::new(host, Duration::from_secs(3)).expect("client");
    (client, server)
}

#[test]
fn describe_reports_the_contract() {
    let (c, _srv) = rig(Fault::None);
    let d = c.describe().expect("describe should succeed");

    assert_eq!(d.contract, "1.0.0", "contract version");
    assert_eq!(d.board, "smp-mock");
    assert_eq!(d.channels, 2, "the normal management + log split");
    assert!(!d.app_version.is_empty());
}

#[test]
fn describe_lives_in_the_per_user_group() {
    // 64 is MGMT_GROUP_ID_PERUSER. Below that the group ids belong to Zephyr,
    // so putting our command there would be squatting on someone else's number.
    assert_eq!(GROUP_PERUSER, 64);
}

#[test]
fn a_withheld_response_is_an_error_not_a_hang() {
    // A board whose firmware lacks the module answers ENOTSUP; a board that is
    // wedged answers nothing. Both must surface as errors, and the error should
    // point at the likely cause.
    let (mut c, _srv) = rig(Fault::Timeout {
        group: GROUP_PERUSER,
        cmd: 0,
    });
    let err = c
        .describe()
        .expect_err("a withheld response must not look like success");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("balena-mcu module"),
        "the error should name the likely cause, got: {msg}"
    );
}
