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
#[allow(clippy::too_many_arguments)]
pub fn spawn(
    root: &Path,
    id: &str,
    target: &str,
    firmware: &Path,
    skip_if_same: bool,
    log_target: Option<&str>,
    lock_fd: RawFd,
) -> Result<i32> {
    let exe = std::env::current_exe().context("failed to resolve own executable path")?;

    let mut cmd = std::process::Command::new(exe);
    // --root is a GLOBAL flag, so it has to precede the subcommand. Without it
    // the child falls back to the default state directory, which is /run/runtt
    // and not writable by an ordinary user -- the device claim then fails with a
    // permission error that looks nothing like the occupancy problem it is not.
    cmd.arg("--root")
        .arg(root)
        .arg(PROXY_SUBCOMMAND)
        .arg("--container-id")
        .arg(id)
        .arg("--target")
        .arg(target)
        .arg("--firmware")
        .arg(firmware);
    if !skip_if_same {
        cmd.arg("--force-reflash");
    }
    if let Some(lt) = log_target {
        cmd.arg("--log-target").arg(lt);
    }

    // A deliberately minimal environment. The engine's environment carries
    // containerd auth tokens and TTRPC addresses; a long-lived process has no
    // business holding them, and anything it later execs must not inherit them.
    cmd.env_clear();
    cmd.env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin");
    cmd.env(lock::LOCK_FD_ENV, lock_fd.to_string());
    if let Ok(v) = std::env::var("RUNTT_TRACE") {
        cmd.env("RUNTT_TRACE", v);
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
pub fn run(
    // Where occupancy locks live, so the resolved-device claim lands beside the
    // label-keyed one taken in `create`.
    root: &Path,
    container_id: &str,
    target: &str,
    firmware: &Path,
    skip_if_same: bool,
    log_target: Option<&str>,
) -> Result<i32> {
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
    let parsed = runtt_transport::Target::parse(target)?;
    let resolved = runtt_transport::resolve::resolve(&parsed)
        .with_context(|| format!("could not resolve target {target}"))?;
    tracing::info!(
        mgmt = %resolved,
        log = ?resolved.log_source(),
        "resolved target"
    );

    // Claim the BOARD, now that we know which board it is.
    //
    // The label-keyed claim taken in `create` cannot do this job on its own: a
    // board has more than one valid name -- `usb:3-4` and `usb:feather-01` may be
    // the same hardware -- so two services using the two forms would take two
    // different label locks and both proceed. Two runtimes flashing one MCU is
    // precisely what occupancy exists to stop, so the authoritative claim is the
    // one keyed on the resolved device. Held for the life of this process; the
    // kernel drops it if we die.
    let _device_claim = crate::lock::acquire_resolved(root, &resolved.lock_key())
        .with_context(|| format!("{resolved} is already in use"))?;

    // An explicit log target overrides whatever the transport could work out,
    // which is nothing at all for tty: and nothing at all for can:.
    let mut resolved = resolved;
    if let Some(lt) = log_target {
        let parsed = runtt_transport::Target::parse(lt)?;
        let r = runtt_transport::resolve::resolve(&parsed)
            .with_context(|| format!("could not resolve log target {lt}"))?;
        match r {
            runtt_transport::resolve::Resolved::Serial { mgmt, .. } => {
                tracing::info!(log = %mgmt.display(), "using the log channel given on the spec");
                resolved.set_log(mgmt);
            }
            // A CAN target has no character device to read, so it cannot serve
            // as somebody else's log channel.
            runtt_transport::resolve::Resolved::Can { .. } => {
                anyhow::bail!(
                    "log target {lt} is a CAN target; a log channel must be a serial device"
                )
            }
        }
    }

    if resolved.log_source().is_none() {
        // Not a failure: single-channel targets and probe-UART bring-up both
        // look like this. Say so, because silence here is confusing.
        println!(
            "mcu: single channel; application logs are demultiplexed from the management link"
        );
    }

    let deploy = crate::flash::Deploy {
        target,
        firmware,
        resolved: resolved.clone(),
        skip_if_same,
    };
    let client = deploy.run()?;

    let stop = || SIGNAL_RECEIVED.load(Ordering::SeqCst) != 0;
    crate::flash::stay_resident(client, resolved.log_source(), &stop)?;
    Ok(0)
}
