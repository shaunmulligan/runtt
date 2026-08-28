//! End-to-end: the real runtime binary deploying to a mock device.
//!
//! The test process holds the pty master and runs the SMP mock on a thread; the
//! runtime is spawned as a genuine subprocess and opens the pty slave, so this
//! exercises the actual binary, the actual OCI verbs and the actual SMP stack —
//! not an in-process approximation.

use serialport::SerialPort;
use smp_mock::faults::Fault;
use smp_mock::server::Server;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const RUNTIME: &str = env!("CARGO_BIN_EXE_mcu-runtime");

struct Rig {
    _mock: std::thread::JoinHandle<()>,
    slave_path: String,
    root: PathBuf,
    bundle: PathBuf,
    id: String,
}

/// Stand up a mock on a pty and write an OCI bundle pointing at it.
fn rig(name: &str, fault: Fault, firmware: &[u8]) -> Rig {
    let (mut master, slave) = serialport::TTYPort::pair().expect("pty pair");
    let slave_path = slave.name().expect("pty slave name");
    master
        .set_timeout(Duration::from_millis(50))
        .expect("set timeout");

    let mock = std::thread::spawn(move || {
        // Hold the slave open for the whole test: dropping it tears down the
        // pty before the runtime can open it.
        let _keepalive = slave;
        let mut srv = Server::new(master, fault);
        let _ = srv.serve();
    });

    let base = std::env::temp_dir().join(format!("mcu-e2e-{name}"));
    let _ = std::fs::remove_dir_all(&base);
    let bundle = base.join("bundle");
    std::fs::create_dir_all(bundle.join("rootfs")).unwrap();
    std::fs::write(bundle.join("rootfs/app.signed.bin"), firmware).unwrap();
    std::fs::write(
        bundle.join("config.json"),
        spec_json(&format!("tty:{slave_path}")),
    )
    .unwrap();

    Rig {
        _mock: mock,
        slave_path,
        root: base.join("state"),
        bundle,
        id: format!("e2e{name}"),
    }
}

/// A minimal but valid OCI spec for a firmware service.
fn spec_json(target: &str) -> String {
    format!(
        r#"{{
  "ociVersion": "1.2.0",
  "process": {{
    "user": {{ "uid": 0, "gid": 0 }},
    "args": ["app.signed.bin"],
    "cwd": "/",
    "terminal": false
  }},
  "root": {{ "path": "rootfs", "readonly": true }},
  "annotations": {{ "io.balena.mcu.target": "{target}" }}
}}"#
    )
}

impl Rig {
    fn verb(&self, args: &[&str]) -> std::process::Output {
        Command::new(RUNTIME)
            .arg("--root")
            .arg(&self.root)
            .args(args)
            .output()
            .expect("failed to run mcu-runtime")
    }

    /// `create` is spawned rather than waited on, because the proxy it forks
    /// inherits stdio: capturing output would block until the proxy exits.
    fn create(&self) -> PathBuf {
        let log = self.root.with_extension("out");
        std::fs::create_dir_all(&self.root).unwrap();
        let out = std::fs::File::create(&log).unwrap();
        let err = out.try_clone().unwrap();
        let status = Command::new(RUNTIME)
            .arg("--root")
            .arg(&self.root)
            .args(["create", "--bundle"])
            .arg(&self.bundle)
            .arg(&self.id)
            .stdout(out)
            .stderr(err)
            .status()
            .expect("create");
        assert!(status.success(), "create failed; see {}", log.display());
        log
    }

    fn state(&self) -> String {
        let o = self.verb(&["state", &self.id]);
        String::from_utf8_lossy(&o.stdout).to_string()
    }

    fn cleanup(&self) {
        let out = self.verb(&["delete", "--force", &self.id]);
        assert!(
            out.status.success(),
            "delete --force should succeed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Wait for `needle` to appear in the container's output.
    fn wait_for(&self, log: &Path, needle: &str, within: Duration) -> String {
        let deadline = Instant::now() + within;
        loop {
            let text = std::fs::read_to_string(log).unwrap_or_default();
            if text.contains(needle) {
                return text;
            }
            if Instant::now() > deadline {
                panic!("timed out waiting for {needle:?}. Output so far:\n{text}");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

#[test]
fn deploys_firmware_and_stays_resident() {
    let firmware: Vec<u8> = (0..3072u32).map(|i| (i % 251) as u8).collect();
    let rig = rig("happy", Fault::None, &firmware);
    let log = rig.create();

    // Created, not started: nothing should have been flashed yet.
    assert!(
        rig.state().contains("\"status\":\"created\""),
        "{}",
        rig.state()
    );

    let out = rig.verb(&["start", &rig.id]);
    assert!(out.status.success(), "start failed: {out:?}");

    // The whole safety-critical ordering, observed from the container's own log.
    let text = rig.wait_for(&log, "image confirmed", Duration::from_secs(20));
    let staged = text.find("marked test").expect("should mark test");
    let confirmed = text.find("image confirmed").expect("should confirm");
    assert!(
        staged < confirmed,
        "must mark test BEFORE confirming, so an image that cannot speak SMP \
         can never be confirmed. Got:\n{text}"
    );

    assert!(
        rig.state().contains("\"status\":\"running\""),
        "{}",
        rig.state()
    );
    rig.cleanup();
}

#[test]
fn a_digest_mismatch_fails_the_container() {
    let rig = rig("badhash", Fault::BadHash, &vec![0xAAu8; 2048]);
    let log = rig.create();
    let out = rig.verb(&["start", &rig.id]);
    assert!(
        out.status.success(),
        "start failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let text = rig.wait_for(&log, "mcu-runtime:", Duration::from_secs(25));
    assert!(
        text.contains("upload") || text.to_lowercase().contains("hash"),
        "the failure should name the cause:\n{text}"
    );
    // And the proxy must be gone, so the engine sees a non-zero exit.
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if rig.state().contains("\"status\":\"stopped\"") {
            rig.cleanup();
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    panic!("container should have stopped; state: {}", rig.state());
}

#[test]
fn a_missing_target_annotation_is_refused_at_create() {
    let (_m, slave) = serialport::TTYPort::pair().unwrap();
    let _keep = slave;
    let base = std::env::temp_dir().join("mcu-e2e-noannot");
    let _ = std::fs::remove_dir_all(&base);
    let bundle = base.join("bundle");
    std::fs::create_dir_all(bundle.join("rootfs")).unwrap();
    std::fs::write(bundle.join("rootfs/app.signed.bin"), b"x").unwrap();
    // A spec with no io.balena.mcu.target at all.
    std::fs::write(
        bundle.join("config.json"),
        r#"{"ociVersion":"1.2.0",
            "process":{"user":{"uid":0,"gid":0},"args":["app.signed.bin"],"cwd":"/","terminal":false},
            "root":{"path":"rootfs","readonly":true},
            "annotations":{}}"#,
    )
    .unwrap();

    let out = Command::new(RUNTIME)
        .arg("--root")
        .arg(base.join("state"))
        .args(["create", "--bundle"])
        .arg(&bundle)
        .arg("noannot")
        .output()
        .unwrap();
    assert!(!out.status.success(), "create should fail without a target");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("io.balena.mcu.target"),
        "the error should name the missing annotation, got: {err}"
    );
}

#[test]
fn a_second_service_cannot_claim_the_same_target() {
    let firmware = vec![0x5Au8; 1024];
    let rig = rig("occupancy", Fault::None, &firmware);
    let _log = rig.create();

    // A different container id, same target.
    let out = Command::new(RUNTIME)
        .arg("--root")
        .arg(&rig.root)
        .args(["create", "--bundle"])
        .arg(&rig.bundle)
        .arg("secondclaim")
        .output()
        .unwrap();
    assert!(!out.status.success(), "the second claim must be refused");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("already claimed"),
        "expected an occupancy error, got: {err}"
    );
    rig.cleanup();
}

#[test]
fn a_second_deploy_of_the_same_image_skips_the_upload() {
    let firmware: Vec<u8> = (0..2048u32).map(|i| (i % 97) as u8).collect();
    let rig = rig("skipsame", Fault::None, &firmware);

    // First deploy: uploads, stages, confirms.
    let log = rig.create();
    assert!(rig.verb(&["start", &rig.id]).status.success());
    let first = rig.wait_for(&log, "image confirmed", Duration::from_secs(20));
    assert!(
        first.contains("uploading"),
        "first deploy should upload:\n{first}"
    );
    rig.cleanup();

    // Second deploy against the same device. This also covers reacquisition:
    // the device must be openable again once the previous container exited. It
    // was not, originally — see the TIOCEXCL note on SerialChannel.
    //
    // The digest
    // is already running and confirmed, so there is nothing to do — reflashing
    // would be a wasted write cycle and an unnecessary reboot.
    let log2 = rig.create();
    assert!(rig.verb(&["start", &rig.id]).status.success());
    let second = rig.wait_for(&log2, "nothing to do", Duration::from_secs(20));
    assert!(
        !second.contains("uploading"),
        "second deploy must not re-upload:\n{second}"
    );
    assert!(
        !second.contains("resetting"),
        "second deploy must not reboot the board:\n{second}"
    );
    rig.cleanup();
}

#[test]
fn force_reflash_overrides_the_skip() {
    let firmware = vec![0x3Cu8; 1536];
    let rig = rig("forceflash", Fault::None, &firmware);

    let log = rig.create();
    assert!(rig.verb(&["start", &rig.id]).status.success());
    rig.wait_for(&log, "image confirmed", Duration::from_secs(20));
    rig.cleanup();

    // Same image, but the spec opts out of skipping.
    std::fs::write(
        rig.bundle.join("config.json"),
        spec_json_with(
            &format!("tty:{}", rig.slave_path),
            r#","io.balena.mcu.skip-if-same-hash":"false""#,
        ),
    )
    .unwrap();

    // DIAGNOSTIC
    eprintln!(
        "DIAG pty {} exists={}",
        rig.slave_path,
        Path::new(&rig.slave_path).exists()
    );
    for p in std::fs::read_dir("/proc").unwrap().flatten() {
        let fd = p.path().join("fd");
        if let Ok(entries) = std::fs::read_dir(&fd) {
            for e in entries.flatten() {
                if let Ok(t) = std::fs::read_link(e.path()) {
                    if t.to_string_lossy() == rig.slave_path {
                        let cmd =
                            std::fs::read_to_string(p.path().join("cmdline")).unwrap_or_default();
                        eprintln!(
                            "DIAG holder pid={:?} cmd={}",
                            p.file_name(),
                            cmd.replace('\0', " ")
                        );
                    }
                }
            }
        }
    }
    let log2 = rig.create();
    assert!(rig.verb(&["start", &rig.id]).status.success());
    let out = rig.wait_for(&log2, "uploading", Duration::from_secs(20));
    assert!(
        out.contains("uploading"),
        "opting out must force a re-upload:\n{out}"
    );
    rig.cleanup();
}

/// Like `spec_json`, with extra annotation entries spliced in.
fn spec_json_with(target: &str, extra_annotations: &str) -> String {
    format!(
        r#"{{
  "ociVersion": "1.2.0",
  "process": {{
    "user": {{ "uid": 0, "gid": 0 }},
    "args": ["app.signed.bin"],
    "cwd": "/",
    "terminal": false
  }},
  "root": {{ "path": "rootfs", "readonly": true }},
  "annotations": {{ "io.balena.mcu.target": "{target}"{extra_annotations} }}
}}"#
    )
}
