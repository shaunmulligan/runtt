//! The resident proxy: the process the engine treats as the container.
//!
//! Two phases, borrowed from remoteproc-runtime:
//!   1. block until `start` sends SIGUSR1 (or SIGTERM/SIGINT → clean exit 0);
//!   2. do the work, and keep doing it until told to stop or until the device
//!      stops answering — in which case **exit non-zero**, which is the whole
//!      mechanism by which restart policies fire.
//!
//! Where we diverge from remoteproc: it explicitly gives up stdio because
//! "firmware has no standard I/O channels". Piping MCU logs to container stdout
//! is our headline feature, so this process holds the inherited stdio for the
//! container's lifetime and does real work throughout, rather than idling.

use crate::{lock, trace};
use anyhow::{Context, Result};
use serde_json::json;
use std::os::fd::RawFd;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::sync::atomic::{AtomicI32, Ordering};

/// Hidden subcommand name for the re-exec.
pub const PROXY_SUBCOMMAND: &str = "proxy";

static SIGNAL_RECEIVED: AtomicI32 = AtomicI32::new(0);

extern "C" fn handle_signal(sig: i32) {
    SIGNAL_RECEIVED.store(sig, Ordering::SeqCst);
}

fn install_handlers() -> Result<()> {
    for sig in [libc::SIGUSR1, libc::SIGTERM, libc::SIGINT] {
        // SA_RESTART deliberately omitted: we want blocking reads to be
        // interrupted so shutdown is prompt.
        let rc = unsafe { libc::signal(sig, handle_signal as *const () as libc::sighandler_t) };
        if rc == libc::SIG_ERR {
            return Err(std::io::Error::last_os_error())
                .context("failed to install signal handler");
        }
    }
    Ok(())
}

/// Re-exec ourselves as the proxy, returning its PID.
pub fn spawn(
    id: &str,
    target: &str,
    firmware: &Path,
    skip_if_same: bool,
    lock_fd: RawFd,
) -> Result<i32> {
    let exe = std::env::current_exe().context("failed to resolve own executable path")?;

    let mut cmd = std::process::Command::new(exe);
    cmd.arg(PROXY_SUBCOMMAND)
        .arg("--container-id")
        .arg(id)
        .arg("--target")
        .arg(target)
        .arg("--firmware")
        .arg(firmware);
    if !skip_if_same {
        cmd.arg("--force-reflash");
    }

    // A deliberately minimal environment. The engine's environment carries
    // containerd auth tokens and TTRPC addresses; a long-lived process has no
    // business holding them, and anything it later execs must not inherit them.
    cmd.env_clear();
    cmd.env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin");
    cmd.env(lock::LOCK_FD_ENV, lock_fd.to_string());
    if let Ok(v) = std::env::var("MCU_RUNTIME_TRACE") {
        cmd.env("MCU_RUNTIME_TRACE", v);
    }
    if let Ok(v) = std::env::var("RUST_LOG") {
        cmd.env("RUST_LOG", v);
    }

    // The proxy inherits our stdio, which containerd has already wired to the
    // container's log pipes (process.terminal: false). Whatever it writes to
    // fd 1 is what `docker logs` shows.
    unsafe {
        cmd.pre_exec(move || {
            // Own process group, so a signal to the runtime's group does not
            // take the container down with it.
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let child = cmd.spawn().context("failed to spawn proxy")?;
    Ok(child.id() as i32)
}

pub fn signal(pid: i32, sig: i32) -> Result<()> {
    let rc = unsafe { libc::kill(pid, sig) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("failed to signal pid {pid}"));
    }
    Ok(())
}

/// Signal 0 probes for existence without delivering anything.
pub fn is_alive(pid: i32) -> bool {
    pid > 0 && unsafe { libc::kill(pid, 0) } == 0
}

/// Block until `pid` is gone, or `timeout` elapses.
///
/// Returns whether it actually exited. We cannot `waitpid` here — the proxy is
/// not our child, it was reparented to the engine's shim — so polling is the
/// available mechanism.
pub fn await_exit(pid: i32, timeout: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if !is_alive(pid) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let still_running = is_alive(pid);
    if still_running {
        tracing::warn!(pid, "proxy did not exit within the timeout after SIGKILL");
    }
    !still_running
}

/// The proxy's own main loop.
pub fn run(container_id: &str, target: &str, firmware: &Path, skip_if_same: bool) -> Result<i32> {
    install_handlers()?;
    trace::record(
        "proxy.waiting",
        json!({ "container_id": container_id, "target": target,
                "firmware": firmware.display().to_string() }),
    );

    // Phase 1 — wait for `start`.
    loop {
        match SIGNAL_RECEIVED.swap(0, Ordering::SeqCst) {
            0 => unsafe {
                libc::pause();
            },
            libc::SIGUSR1 => break,
            libc::SIGTERM | libc::SIGINT => {
                // Killed before ever starting: a clean exit, not a failure.
                return Ok(0);
            }
            _ => {}
        }
    }

    trace::record("proxy.starting", json!({ "container_id": container_id }));

    // Phase 2 — deploy, then stay resident.
    let parsed = transport::Target::parse(target)?;
    let resolved = transport::resolve::resolve(&parsed)
        .with_context(|| format!("could not resolve target {target}"))?;
    tracing::info!(
        mgmt = %resolved.mgmt.display(),
        log = ?resolved.log.as_ref().map(|p| p.display().to_string()),
        "resolved target"
    );
    if resolved.log.is_none() {
        // Not a failure: single-channel targets and probe-UART bring-up both
        // look like this. Say so, because silence here is confusing.
        println!("mcu: single channel; application logs share the management link");
    }

    let deploy = crate::flash::Deploy {
        target,
        firmware,
        resolved: resolved.clone(),
        skip_if_same,
    };
    let client = deploy.run()?;

    let stop = || SIGNAL_RECEIVED.load(Ordering::SeqCst) != 0;
    crate::flash::stay_resident(client, resolved.log.as_deref(), &stop)?;
    Ok(0)
}
