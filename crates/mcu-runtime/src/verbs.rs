//! The five OCI verbs.
//!
//! Lifecycle, and the one detail that makes restart policies work:
//!
//! 1. `create` forks the resident proxy, writes its PID to `--pid-file`, exits 0.
//!    Because the parent exits, the proxy reparents to the shim (which set
//!    `PR_SET_CHILD_SUBREAPER`) and *that PID is the container*.
//! 2. `start` signals the proxy (SIGUSR1); the proxy then does the real work.
//! 3. The proxy exits non-zero on detach or heartbeat loss → the shim reaps it →
//!    `TaskExit` → the restart policy fires.
//!
//! The proxy must be spawned in `create`, not `start`, or there is a window in
//! which the shim has no PID to track and `state` cannot report one.

use crate::{annotations, lock, state, trace};
use anyhow::{bail, Context, Result};
use oci_spec::runtime::{ContainerState, Spec};
use serde_json::json;
use std::path::{Path, PathBuf};

pub struct Ctx {
    pub root: PathBuf,
}

pub fn create(
    ctx: &Ctx,
    id: &str,
    bundle: &Path,
    pid_file: Option<&Path>,
) -> Result<()> {
    state::validate_container_id(id)?;

    let spec_path = bundle.join("config.json");
    // Parse via serde directly rather than Spec::load: oci-spec collapses parse
    // failures to "serde failed", which tells a user nothing about which field
    // of a 300-line config.json is wrong.
    let spec_raw = std::fs::read_to_string(&spec_path)
        .with_context(|| format!("failed to read {}", spec_path.display()))?;
    let spec: Spec = serde_json::from_str(&spec_raw)
        .with_context(|| format!("failed to parse {}", spec_path.display()))?;

    // The phase-0 diagnostic: record exactly what we were handed, so what
    // reaches the annotations map is settled by observation.
    trace::record(
        "create.spec",
        json!({
            "container_id": id,
            "bundle": bundle.display().to_string(),
            "annotations": spec.annotations(),
            "process_args": spec.process().as_ref().and_then(|p| p.args().clone()),
            "root_path": spec.root().as_ref().map(|r| r.path().display().to_string()),
            "spec": serde_json::to_value(&spec).ok(),
        }),
    );

    let target = spec
        .annotations()
        .as_ref()
        .and_then(|a| a.get(annotations::SPEC_TARGET))
        .cloned()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "missing required annotation {}: the firmware service must declare \
                 which MCU it targets, e.g. {}=usb:3-6",
                annotations::SPEC_TARGET,
                annotations::SPEC_TARGET
            )
        })?;

    // Parse now so a mislabelled service fails immediately with a clear message
    // rather than timing out against a device that was never going to answer.
    let parsed = transport::Target::parse(&target)?;
    if let transport::Target::Can { .. } = parsed {
        bail!("can: targets are not implemented this cycle (named production follow-on)");
    }

    let firmware = locate_firmware(&spec, bundle)?;

    // Default on: reflashing an image the device already runs is a pointless
    // write cycle and a pointless reboot. Opt out with the annotation set to
    // "false" when you want to force a rewrite (e.g. suspected flash corruption).
    let skip_if_same = spec
        .annotations()
        .as_ref()
        .and_then(|a| a.get(annotations::SPEC_SKIP_IF_SAME))
        .map(|v| !matches!(v.trim().to_ascii_lowercase().as_str(), "false" | "0" | "no"))
        .unwrap_or(true);

    // Claim the port before spawning anything, so a conflict is reported as a
    // create failure rather than a mysteriously dying container.
    let lock_fd = lock::acquire(&ctx.root, &target)?;

    let pid = crate::proxy::spawn(id, &target, &firmware, skip_if_same, lock_fd)
        .context("failed to spawn the resident proxy process")?;

    // From here on, any failure must not leave an orphaned proxy holding the port.
    let mut guard = SpawnGuard { pid: Some(pid) };

    let mut st = state::new_state(id, bundle);
    st.set_status(ContainerState::Created);
    st.set_pid(Some(pid));
    state::set_annotation(&mut st, annotations::STATE_TARGET, target.clone());
    state::set_annotation(
        &mut st,
        annotations::STATE_FIRMWARE_PATH,
        firmware.display().to_string(),
    );
    state::write(&ctx.root, &st)?;

    if let Some(p) = pid_file {
        std::fs::write(p, format!("{pid}"))
            .with_context(|| format!("failed to write pid file {}", p.display()))?;
    }

    guard.pid = None;
    tracing::info!(container = id, target = %target, pid, "created");
    Ok(())
}

/// Kills the proxy if `create` fails after spawning it.
struct SpawnGuard {
    pid: Option<i32>,
}

impl Drop for SpawnGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.pid {
            let _ = crate::proxy::signal(pid, libc::SIGKILL);
        }
    }
}

/// The firmware image is the container's entrypoint, resolved inside the rootfs.
/// Same convention as remoteproc-runtime: `FROM scratch` + `ADD app.signed.bin /`
/// + `ENTRYPOINT ["app.signed.bin"]`.
fn locate_firmware(spec: &Spec, bundle: &Path) -> Result<PathBuf> {
    let args = spec
        .process()
        .as_ref()
        .and_then(|p| p.args().clone())
        .unwrap_or_default();
    if args.len() != 1 {
        bail!(
            "expected exactly one entrypoint argument naming the firmware image, got {:?}. \
             A firmware image should be `FROM scratch` with ENTRYPOINT [\"app.signed.bin\"]",
            args
        );
    }
    let rootfs = spec
        .root()
        .as_ref()
        .map(|r| r.path().to_path_buf())
        .ok_or_else(|| anyhow::anyhow!("spec has no root path"))?;
    let rootfs = if rootfs.is_absolute() { rootfs } else { bundle.join(rootfs) };

    let path = rootfs.join(&args[0]);
    if !path.exists() {
        bail!(
            "firmware image {} does not exist in the container rootfs",
            path.display()
        );
    }
    Ok(path)
}

pub fn start(ctx: &Ctx, id: &str) -> Result<()> {
    let mut st = state::read(&ctx.root, id)?;
    if *st.status() != ContainerState::Created {
        bail!(
            "cannot start container {id}: status is {}, expected created",
            st.status()
        );
    }
    let pid = st
        .pid()
        .ok_or_else(|| anyhow::anyhow!("state for {id} has no pid"))?;

    // Release the proxy from its pre-start wait. It does the flashing itself
    // rather than having `start` do it: flashing takes tens of seconds, `start`
    // must return promptly, and the proxy owns the serial port for the whole
    // container lifetime anyway (logs + heartbeat), so the port is never handed
    // between processes.
    crate::proxy::signal(pid, libc::SIGUSR1)
        .with_context(|| format!("failed to signal proxy {pid}"))?;

    st.set_status(ContainerState::Running);
    state::write(&ctx.root, &st)?;
    tracing::info!(container = id, pid, "started");
    Ok(())
}

pub fn print_state(ctx: &Ctx, id: &str) -> Result<()> {
    let mut st = state::read(&ctx.root, id)?;

    // `state` is invoked as a fresh process and has no memory, so reconcile the
    // recorded status against whether the proxy is actually alive.
    if *st.status() == ContainerState::Running {
        if let Some(pid) = st.pid() {
            if !crate::proxy::is_alive(*pid) {
                st.set_status(ContainerState::Stopped);
            }
        }
    }

    let json = serde_json::to_string(&st).context("failed to serialise state")?;
    println!("{json}");
    Ok(())
}

pub fn kill(ctx: &Ctx, id: &str, signal: &str, _all: bool) -> Result<()> {
    let mut st = state::read(&ctx.root, id)?;
    let sig = parse_signal(signal)?;

    if let Some(pid) = st.pid() {
        if crate::proxy::is_alive(*pid) {
            crate::proxy::signal(*pid, sig)
                .with_context(|| format!("failed to send {signal} to proxy {pid}"))?;
        }
    }

    if sig == libc::SIGKILL || sig == libc::SIGTERM || sig == libc::SIGINT {
        st.set_status(ContainerState::Stopped);
        state::write(&ctx.root, &st)?;
    }
    Ok(())
}

pub fn delete(ctx: &Ctx, id: &str, force: bool) -> Result<()> {
    let st = match state::read(&ctx.root, id) {
        Ok(s) => s,
        // Deleting something already gone is success, not an error — containerd
        // retries delete on its cleanup path.
        Err(_) if force => return Ok(()),
        Err(e) => return Err(e),
    };

    let running = *st.status() == ContainerState::Running
        && st.pid().map(crate::proxy::is_alive).unwrap_or(false);

    if running && !force {
        bail!("cannot delete running container {id} (use --force)");
    }
    if running {
        if let Some(pid) = st.pid() {
            let _ = crate::proxy::signal(*pid, libc::SIGKILL);
            // Wait for it to actually go. `delete` must not return while the
            // proxy still holds the serial port open exclusively: on a restart
            // policy the engine immediately creates the replacement, which
            // would then fail to open the device with EBUSY. Signalling is
            // asynchronous; releasing the resource is what we are promising.
            crate::proxy::await_exit(*pid, std::time::Duration::from_secs(5));
        }
    }

    // The occupancy lock needs no explicit release: it is held by the proxy's
    // inherited file description and the kernel drops it when the proxy dies.
    state::remove(&ctx.root, id)?;
    tracing::info!(container = id, "deleted");
    Ok(())
}

fn parse_signal(input: &str) -> Result<i32> {
    let s = input.trim().to_ascii_uppercase();
    let s = s.strip_prefix("SIG").unwrap_or(&s);
    Ok(match s {
        "KILL" | "9" => libc::SIGKILL,
        "TERM" | "15" => libc::SIGTERM,
        "INT" | "2" => libc::SIGINT,
        "USR1" | "10" => libc::SIGUSR1,
        "HUP" | "1" => libc::SIGHUP,
        "QUIT" | "3" => libc::SIGQUIT,
        other => bail!("unsupported signal {other:?} (supported: TERM, KILL, INT, USR1, HUP, QUIT)"),
    })
}
