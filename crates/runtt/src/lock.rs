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
pub const LOCK_FD_ENV: &str = "RUNTT_LOCK_FD";

/// Make a string safe to use as a filename.
fn sanitise(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Turn a placement label into a filesystem-safe lock filename.
fn lock_name(target: &str) -> String {
    format!("occupancy-{}.lock", sanitise(target))
}

/// Acquire the occupancy lock for a RESOLVED board, returning an fd that must be
/// kept open for as long as the claim should hold.
///
/// WHY THIS EXISTS SEPARATELY FROM THE LABEL LOCK. `acquire` keys on the
/// placement label, which was sufficient while a board had exactly one way to be
/// named. It no longer does: `usb:3-4` and `usb:feather-01` can be the same
/// physical board, and two containers using the two forms would take two
/// different label locks and both proceed -- two runtimes flashing one MCU, which
/// is the exact thing occupancy is meant to prevent.
///
/// A label cannot be compared for board-identity without resolving it, so this
/// claim is taken in the proxy once the target is resolved. `Resolved`'s Display
/// is the canonical key: the management device path for a serial target, and
/// `can:<iface>/<id>` for a CAN one.
pub fn acquire_resolved(root: &Path, resolved: &str) -> Result<RawFd> {
    acquire_named(root, &format!("device-{}", sanitise(resolved)))
}

/// Acquire the occupancy lock for `target`, returning an fd that must be kept
/// open (and inherited) for as long as the claim should hold.
pub fn acquire(root: &Path, target: &str) -> Result<RawFd> {
    acquire_named(root, &lock_name(target)).map_err(|e| {
        // Keep the label in the message: it is what the operator wrote.
        if e.to_string().contains("already claimed") {
            anyhow::anyhow!(
                "target {target} is already claimed by another service \
                 (exclusive occupancy: one service = one MCU)"
            )
        } else {
            e
        }
    })
}

/// The shared body: open a named lock file under `root` and flock it.
fn acquire_named(root: &Path, name: &str) -> Result<RawFd> {
    std::fs::create_dir_all(root)
        .with_context(|| format!("failed to create {}", root.display()))?;
    let path = root.join(name);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        // Explicitly do not truncate: the file's contents are irrelevant (the
        // lock lives in the kernel, not the file), and another holder may have
        // it open.
        .truncate(false)
        .open(&path)
        .with_context(|| format!("failed to open lock file {}", path.display()))?;

    // Non-blocking: a board already claimed must fail fast with a clear message,
    // not hang the engine waiting for a slot that may never free.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
            bail!(
                "already claimed by another service \
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The defect this guards: a board has more than one valid name, so keying
    /// occupancy on the label text let two services claim one MCU. `usb:3-4` and
    /// `usb:feather-01` can be the same hardware; so can two paths where one is a
    /// symlink to the other. The device claim is keyed on the resolved board, so
    /// the second attempt must be refused.
    #[test]
    fn one_device_cannot_be_claimed_twice_under_two_names() {
        let root = std::env::temp_dir().join("runtt-lock-alias-test");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        // Both names reduce to the same key, which is the point.
        let key = "/dev/ttyACM_fake_for_test";

        let first = acquire_resolved(&root, key).expect("first claim should succeed");
        let second = acquire_resolved(&root, key);
        assert!(
            second.is_err(),
            "the second claim on one device must be refused"
        );
        assert!(
            second.unwrap_err().to_string().contains("already claimed"),
            "the refusal should say why"
        );

        // Releasing lets the next claimant in, so a restarted service is not
        // locked out by its own predecessor.
        // SAFETY: fd came from acquire_resolved and is not used again.
        unsafe { libc::close(first) };
        acquire_resolved(&root, key).expect("claim should succeed once released");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn different_devices_do_not_contend() {
        let root = std::env::temp_dir().join("runtt-lock-distinct-test");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let a = acquire_resolved(&root, "/dev/ttyACM0").expect("board A");
        let b = acquire_resolved(&root, "/dev/ttyACM1").expect("board B");
        // Two MCUs on one host is the normal case, not an edge case.
        assert!(a != b);

        // SAFETY: both fds came from acquire_resolved and are not used again.
        unsafe {
            libc::close(a);
            libc::close(b);
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
