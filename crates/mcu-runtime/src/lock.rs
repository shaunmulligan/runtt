//! Exclusive occupancy: one service = one MCU.
//!
//! The lock is an `flock` held by the *open file description*, not the process,
//! which is what makes this work across the fork: `create` acquires it, clears
//! `FD_CLOEXEC`, and the proxy inherits the descriptor. `create` then exits and
//! the lock survives, held by the long-lived proxy for the container's lifetime
//! and released by the kernel when it dies — no stale lockfiles to clean up.

use anyhow::{bail, Context, Result};
use std::os::fd::{AsRawFd, IntoRawFd, RawFd};
use std::path::Path;

/// Environment variable carrying the inherited lock fd to the proxy.
pub const LOCK_FD_ENV: &str = "MCU_RUNTIME_LOCK_FD";

/// Turn a placement label into a filesystem-safe lock filename.
fn lock_name(target: &str) -> String {
    let safe: String = target
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '_' })
        .collect();
    format!("occupancy-{safe}.lock")
}

/// Acquire the occupancy lock for `target`, returning an fd that must be kept
/// open (and inherited) for as long as the claim should hold.
pub fn acquire(root: &Path, target: &str) -> Result<RawFd> {
    std::fs::create_dir_all(root)
        .with_context(|| format!("failed to create {}", root.display()))?;
    let path = root.join(lock_name(target));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&path)
        .with_context(|| format!("failed to open lock file {}", path.display()))?;

    // Non-blocking: a port already claimed must fail fast with a clear message,
    // not hang the engine waiting for a slot that may never free.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
            bail!(
                "target {target} is already claimed by another service \
                 (exclusive occupancy: one service = one MCU)"
            );
        }
        return Err(err).context("flock failed");
    }

    let fd = file.into_raw_fd();
    clear_cloexec(fd)?;
    Ok(fd)
}

/// Clear `FD_CLOEXEC` so the descriptor — and therefore the lock — survives the
/// exec into the proxy.
fn clear_cloexec(fd: RawFd) -> Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error()).context("F_GETFD failed");
    }
    let rc = unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) };
    if rc < 0 {
        return Err(std::io::Error::last_os_error()).context("F_SETFD failed");
    }
    Ok(())
}
