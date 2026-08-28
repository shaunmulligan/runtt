//! Turning a placement label into device paths.
//!
//! Reads sysfs directly rather than linking libudev. The two things we need —
//! a port path and an interface string descriptor — are both plain files under
//! `/sys/class/tty/<dev>/device/`, so the native dependency buys nothing.
//!
//! Channel identity comes from the **interface string descriptor**, never from
//! the interface number: `ID_PATH` is interface-suffixed, so the numbering of a
//! composite device's channels is not contractual. A customer may also ship
//! their own VID, which is why we do not match on VID/PID either.

use crate::usb::{IFACE_LOG, IFACE_MGMT};
use crate::Target;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

/// The device paths for one target's channels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub mgmt: PathBuf,
    /// `None` on single-channel targets (ESP32-C3 class, or bring-up over a
    /// debug probe's UART bridge), where logs share the management channel.
    pub log: Option<PathBuf>,
}

/// One candidate tty discovered in sysfs.
#[derive(Debug, Clone)]
struct Candidate {
    dev: PathBuf,
    /// Kernel USB port path, e.g. `3-6` or `1-1.2`.
    port_path: Option<String>,
    /// USB interface string descriptor, if the device supplies one.
    interface: Option<String>,
}

pub fn resolve(target: &Target) -> Result<Resolved> {
    match target {
        Target::Tty { device } => {
            // An absolute path is taken literally, which is what makes a
            // native_sim pty or a mock addressable without special-casing.
            let path = if device.starts_with('/') {
                PathBuf::from(device)
            } else {
                Path::new("/dev").join(device)
            };
            if !path.exists() {
                bail!("{} does not exist", path.display());
            }
            Ok(Resolved {
                mgmt: path,
                log: None,
            })
        }
        Target::Usb { port_path } => resolve_usb(port_path),
        Target::Can { .. } => bail!("can: transport is not implemented this cycle"),
    }
}

fn resolve_usb(port_path: &str) -> Result<Resolved> {
    // Prefer the udev-maintained tree if it is present: it is the contract-keyed
    // inventory, and its existence means the rules are installed and working.
    if let Some(r) = from_udev_tree(port_path)? {
        return Ok(r);
    }

    let candidates = scan_ttys().context("failed to scan sysfs for tty devices")?;
    let matching: Vec<&Candidate> = candidates
        .iter()
        .filter(|c| c.port_path.as_deref() == Some(port_path))
        .collect();

    if matching.is_empty() {
        let seen: Vec<String> = candidates
            .iter()
            .filter_map(|c| c.port_path.clone())
            .collect();
        bail!(
            "no contract device at usb:{port_path}. Ports currently present: {}. \
             A board without our firmware contract shows up as exactly this error, \
             which is the correct legible symptom.",
            if seen.is_empty() {
                "none".to_string()
            } else {
                seen.join(", ")
            }
        );
    }

    let mgmt = matching
        .iter()
        .find(|c| c.interface.as_deref() == Some(IFACE_MGMT))
        .map(|c| c.dev.clone());
    let log = matching
        .iter()
        .find(|c| c.interface.as_deref() == Some(IFACE_LOG))
        .map(|c| c.dev.clone());

    match mgmt {
        Some(mgmt) => Ok(Resolved { mgmt, log }),
        // Exactly one tty on the port and no interface strings: a single-channel
        // target, or a board whose descriptors we cannot read. Use it, because
        // refusing here would block bring-up over a plain UART.
        None if matching.len() == 1 => Ok(Resolved {
            mgmt: matching[0].dev.clone(),
            log: None,
        }),
        None => bail!(
            "found {} tty devices at usb:{port_path} but none advertising the \
             {IFACE_MGMT} interface descriptor; is the firmware contract present?",
            matching.len()
        ),
    }
}

/// `/dev/balena-mcu/<ID_PATH_TAG>-mgmt` and `-log`, created by our udev rules.
fn from_udev_tree(port_path: &str) -> Result<Option<Resolved>> {
    let dir = Path::new("/dev/balena-mcu");
    if !dir.is_dir() {
        return Ok(None);
    }
    // ID_PATH_TAG replaces separators with underscores, so a port path of
    // `3-6` appears inside a tag like `pci-0000_00_14_0-usb-0_6_1_1`. Match on
    // the tty's real port path instead of trying to reverse the tag.
    let mut mgmt = None;
    let mut log = None;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        let resolved = std::fs::canonicalize(entry.path()).unwrap_or_else(|_| entry.path());
        let dev_name = resolved
            .file_name()
            .map(|s| s.to_string_lossy().to_string());
        let Some(dev_name) = dev_name else { continue };
        if tty_port_path(&dev_name).as_deref() != Some(port_path) {
            continue;
        }
        if name.ends_with("-mgmt") {
            mgmt = Some(resolved);
        } else if name.ends_with("-log") {
            log = Some(resolved);
        }
    }
    Ok(mgmt.map(|mgmt| Resolved { mgmt, log }))
}

fn scan_ttys() -> Result<Vec<Candidate>> {
    let mut out = Vec::new();
    let class = Path::new("/sys/class/tty");
    if !class.is_dir() {
        return Ok(out);
    }
    for entry in std::fs::read_dir(class)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        // Only USB CDC-ACM devices are plausible contract devices.
        if !name.starts_with("ttyACM") && !name.starts_with("ttyUSB") {
            continue;
        }
        let dev = Path::new("/dev").join(&name);
        if !dev.exists() {
            continue;
        }
        out.push(Candidate {
            port_path: tty_port_path(&name),
            interface: tty_interface(&name),
            dev,
        });
    }
    Ok(out)
}

/// Derive the kernel USB port path (e.g. `3-6`) for a tty.
///
/// `/sys/class/tty/ttyACM0/device` points at the USB *interface*
/// (`.../usb3/3-6/3-6:1.1`), whose parent directory name is the port path.
fn tty_port_path(tty: &str) -> Option<String> {
    let link = Path::new("/sys/class/tty").join(tty).join("device");
    let real = std::fs::canonicalize(link).ok()?;
    let iface = real.file_name()?.to_string_lossy().to_string();
    // `3-6:1.1` -> `3-6`
    let port = iface.split(':').next()?.to_string();
    if port.is_empty() {
        None
    } else {
        Some(port)
    }
}

/// Read the USB interface string descriptor, which is what the contract owns.
fn tty_interface(tty: &str) -> Option<String> {
    let path = Path::new("/sys/class/tty")
        .join(tty)
        .join("device")
        .join("interface");
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_transport_prefixes() {
        assert_eq!(
            Target::parse("usb:3-6").unwrap(),
            Target::Usb {
                port_path: "3-6".into()
            }
        );
        assert_eq!(
            Target::parse("tty:ttyS3").unwrap(),
            Target::Tty {
                device: "ttyS3".into()
            }
        );
        assert_eq!(
            Target::parse("can:vcan0/0x42").unwrap(),
            Target::Can {
                iface: "vcan0".into(),
                node_id: "0x42".into()
            }
        );
    }

    #[test]
    fn rejects_a_label_with_no_prefix() {
        // The whole point of prefixing from day one is that an unprefixed label
        // fails loudly instead of being guessed at.
        let err = Target::parse("1-1.2").unwrap_err().to_string();
        assert!(err.contains("no transport prefix"), "got: {err}");
    }

    #[test]
    fn rejects_an_unknown_prefix() {
        let err = Target::parse("spi:0").unwrap_err().to_string();
        assert!(err.contains("unknown transport prefix"), "got: {err}");
    }

    #[test]
    fn round_trips_through_display() {
        for label in ["usb:3-6", "tty:ttyS3", "can:vcan0/0x42"] {
            assert_eq!(Target::parse(label).unwrap().to_string(), label);
        }
    }

    #[test]
    fn an_absolute_tty_path_is_taken_literally() {
        // This is what lets a native_sim pty or the mock be addressed directly.
        let t = Target::parse("tty:/dev/null").unwrap();
        let r = resolve(&t).unwrap();
        assert_eq!(r.mgmt, PathBuf::from("/dev/null"));
        assert!(r.log.is_none());
    }

    #[test]
    fn a_missing_tty_is_a_clear_error() {
        let t = Target::parse("tty:/dev/definitely-not-here").unwrap();
        assert!(resolve(&t)
            .unwrap_err()
            .to_string()
            .contains("does not exist"));
    }

    #[test]
    fn can_targets_are_refused_with_a_reason() {
        let t = Target::parse("can:vcan0/0x42").unwrap();
        assert!(resolve(&t)
            .unwrap_err()
            .to_string()
            .contains("not implemented"));
    }
}
