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

/// How far above the node id the device's console frames sit. Must match
/// `CONFIG_RUNTT_CAN_NODE_ID + 2` in `firmware/runtt/src/can_log.c`.
pub const LOG_ID_OFFSET: u32 = 2;
use crate::{Target, UsbSelector};
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

/// Where one target's channels actually are.
///
/// Not a path pair, because CAN targets are not path-shaped: they are addressed
/// by interface and node id, and no character device exists for them. Making
/// this an enum is what keeps `flash.rs` from having to pretend otherwise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    /// One or two character devices — USB CDC-ACM, or a bare UART.
    Serial {
        mgmt: PathBuf,
        /// `None` on single-channel targets (ESP32-C3 class, or bring-up over a
        /// debug probe's UART bridge), where logs share the management channel.
        log: Option<PathBuf>,
    },
    /// A SocketCAN interface and an ISO-TP node id. The host sends on `node_id`
    /// and the device replies on `node_id + 1`; see `transport::can`.
    Can {
        iface: String,
        node_id: u32,
        /// CAN carries no log channel of its own. A `tty:` log target on the
        /// spec puts one here — a board managed over the bus whose console
        /// still comes back over a wire is a real and useful arrangement.
        log: Option<PathBuf>,
    },
}

/// Where a target's console output comes from.
///
/// Not just a path, because a CAN target's console arrives as raw frames on the
/// bus rather than from any character device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogSource {
    /// A character device carrying a plain byte stream.
    Serial(PathBuf),
    /// Raw CAN frames on `id`. See `runtt_transport::can::CanLogReader`.
    Can { iface: String, id: u32 },
}

impl Resolved {
    /// The log channel, if this target has one at all.
    ///
    /// A CAN target has one by default -- the device's console goes out as raw
    /// frames on `node_id + 2` -- but an explicit `tty:` log target overrides it,
    /// for a board managed over the bus whose console comes back over a wire.
    pub fn log_source(&self) -> Option<LogSource> {
        match self {
            Resolved::Serial { log, .. } => log.clone().map(LogSource::Serial),
            Resolved::Can { log: Some(p), .. } => Some(LogSource::Serial(p.clone())),
            Resolved::Can {
                iface,
                node_id,
                log: None,
            } => Some(LogSource::Can {
                iface: iface.clone(),
                id: node_id + LOG_ID_OFFSET,
            }),
        }
    }

    /// The explicitly-configured log *path*, where there is one.
    pub fn log_path(&self) -> Option<&Path> {
        match self {
            Resolved::Serial { log, .. } | Resolved::Can { log, .. } => log.as_deref(),
        }
    }

    /// Attach an explicitly-specified log channel, overriding whatever the
    /// transport worked out for itself.
    pub fn set_log(&mut self, path: PathBuf) {
        match self {
            Resolved::Serial { log, .. } | Resolved::Can { log, .. } => *log = Some(path),
        }
    }

    /// A key identifying the physical board, for the occupancy claim.
    ///
    /// CANONICALISED, which is the whole point. Two labels can name one device
    /// through a symlink -- `tty:/dev/ttyACM1` and
    /// `tty:/dev/runtt/<tag>-mgmt` are the same board -- and comparing the
    /// literal paths would let both be claimed at once. Resolving the link is
    /// what makes the claim about hardware rather than about spelling.
    ///
    /// Falls back to the unresolved path if canonicalisation fails, which is
    /// worse than nothing only if it also would have failed to open.
    pub fn lock_key(&self) -> String {
        match self {
            Resolved::Serial { mgmt, .. } => std::fs::canonicalize(mgmt)
                .unwrap_or_else(|_| mgmt.clone())
                .display()
                .to_string(),
            Resolved::Can { iface, node_id, .. } => format!("can:{iface}/{node_id:#x}"),
        }
    }

    /// How many channels this target presents, for the contract check against
    /// what the device says it has.
    pub fn channels(&self) -> u32 {
        if self.log_source().is_some() {
            2
        } else {
            1
        }
    }
}

impl std::fmt::Display for Resolved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Resolved::Serial { mgmt, .. } => write!(f, "{}", mgmt.display()),
            Resolved::Can { iface, node_id, .. } => write!(f, "can:{iface}/{node_id:#x}"),
        }
    }
}

/// One candidate tty discovered in sysfs.
#[derive(Debug, Clone)]
struct Candidate {
    dev: PathBuf,
    /// Kernel USB port path, e.g. `3-6` or `1-1.2`.
    port_path: Option<String>,
    /// USB interface string descriptor, if the device supplies one.
    interface: Option<String>,
    /// The device's USB serial string descriptor. On our firmware this is the
    /// provisioned board serial, or a hardware-derived one when unprovisioned.
    serial: Option<String>,
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
            Ok(Resolved::Serial {
                mgmt: path,
                log: None,
            })
        }
        Target::Usb { selector } => match selector {
            UsbSelector::PortPath(p) => resolve_usb(p),
            UsbSelector::Serial(serial) => resolve_usb_serial(serial),
        },
        Target::Can { iface, node_id } => {
            let node_id = parse_node_id(node_id)?;
            // Check the interface exists here rather than letting `bind()` fail
            // later with an errno. A typo'd interface name is the likeliest
            // mistake and deserves to say so.
            if !Path::new("/sys/class/net").join(iface).is_dir() {
                bail!(
                    "no network interface named {iface:?}. For a virtual bus: \
                     `sudo modprobe vcan can-isotp && sudo ip link add dev {iface} type vcan \
                     && sudo ip link set {iface} up`"
                );
            }
            Ok(Resolved::Can {
                iface: iface.clone(),
                node_id,
                log: None,
            })
        }
    }
}

/// Find a board by the serial it publishes as its USB serial string descriptor.
///
/// DELIBERATELY NOT A PROBE. The tempting implementation opens every candidate
/// tty and asks it `describe` -- which means speaking SMP to boards that belong
/// to other containers, and an SMP frame landing mid-upload on somebody else's
/// board is precisely the corruption hazard `ID_MM_DEVICE_IGNORE` exists to
/// prevent. Identity has to be readable WITHOUT talking to the device, so the
/// firmware publishes it in the USB serial string descriptor and this is a
/// filesystem lookup like any other.
fn resolve_usb_serial(serial: &str) -> Result<Resolved> {
    let candidates = scan_ttys().context("failed to scan sysfs for tty devices")?;
    let matching: Vec<&Candidate> = candidates
        .iter()
        .filter(|c| c.serial.as_deref() == Some(serial))
        .collect();

    if matching.is_empty() {
        // Dedupe: a composite board contributes one candidate per CDC
        // interface, and listing the same serial twice reads like two boards.
        let mut seen: Vec<String> = candidates.iter().filter_map(|c| c.serial.clone()).collect();
        seen.sort();
        seen.dedup();
        bail!(
            "no board with serial {serial:?} is attached. Serials currently present: {}. \
             A board only publishes one once it has an identity record -- write one with \
             scripts/make-identity.py.",
            if seen.is_empty() {
                "none".to_string()
            } else {
                seen.join(", ")
            }
        );
    }

    // Two boards answering to one serial is a provisioning mistake, and picking
    // whichever sysfs happened to enumerate first would make deploys
    // non-deterministic -- the same label flashing a different board run to run.
    // A port path cannot have this problem, since it is unique by construction;
    // a serial is only as unique as whoever wrote it.
    let mut ports: Vec<&str> = matching
        .iter()
        .filter_map(|c| c.port_path.as_deref())
        .collect();
    ports.sort();
    ports.dedup();
    if ports.len() > 1 {
        bail!(
            "serial {serial:?} is claimed by more than one board, on USB ports {}. \
             Serials must be unique: re-provision one of them with \
             scripts/make-identity.py, or address them by port path instead.",
            ports.join(" and ")
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
        Some(mgmt) => Ok(Resolved::Serial { mgmt, log }),
        None if matching.len() == 1 => Ok(Resolved::Serial {
            mgmt: matching[0].dev.clone(),
            log: None,
        }),
        None => bail!(
            "found {} tty devices with serial {serial:?} but none advertising the \
             {IFACE_MGMT} interface descriptor; is the firmware contract present?",
            matching.len()
        ),
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
        Some(mgmt) => Ok(Resolved::Serial { mgmt, log }),
        // Exactly one tty on the port and no interface strings: a single-channel
        // target, or a board whose descriptors we cannot read. Use it, because
        // refusing here would block bring-up over a plain UART.
        None if matching.len() == 1 => Ok(Resolved::Serial {
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

/// `/dev/runtt/<ID_PATH_TAG>-mgmt` and `-log`, created by our udev rules.
fn from_udev_tree(port_path: &str) -> Result<Option<Resolved>> {
    let dir = Path::new("/dev/runtt");
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
    Ok(mgmt.map(|mgmt| Resolved::Serial { mgmt, log }))
}

/// Parse an ISO-TP node id, hex (`0x42`) or decimal (`66`).
///
/// **Standard 11-bit identifiers only.** The device builds its filters with
/// `.std_id` (`firmware/runtt/src/smp_can.c`) and its Kconfig declares
/// `range 0x0 0x7ff`; the host socket does not set `CAN_EFF_FLAG` either. Both
/// ends agree, and this is where a label that disagrees gets rejected — rather
/// than being silently masked into a different id on the wire, which would
/// present as an unexplained timeout.
///
/// A node owns THREE consecutive ids: requests on `node_id`, replies on
/// `node_id + 1`, and the device's console on `node_id + 2`. The console id is
/// reserved whether or not the firmware was built with CAN logging, so that
/// enabling it later cannot collide with a neighbour already on the bus. The top
/// usable host id is therefore `0x7fd`.
fn parse_node_id(s: &str) -> Result<u32> {
    let t = s.trim();
    let parsed = match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        Some(hex) => u32::from_str_radix(hex, 16),
        None => t.parse::<u32>(),
    };
    let id = parsed.map_err(|_| {
        anyhow::anyhow!("CAN node id {s:?} is not a number; expected e.g. `0x42` or `66`")
    })?;
    /// The largest standard CAN identifier.
    const MAX_STD_ID: u32 = 0x7ff;
    if id > MAX_STD_ID {
        bail!(
            "CAN node id {s:?} ({id:#x}) is larger than the maximum standard CAN \
             identifier ({MAX_STD_ID:#x}). The firmware filters on 11-bit ids; \
             extended ids are not part of the contract."
        );
    }
    if id + LOG_ID_OFFSET > MAX_STD_ID {
        bail!(
            "CAN node id {s:?} leaves no room for the two ids the device also \
             owns -- replies on {:#x} and its console on {:#x} -- and the 11-bit \
             maximum is {MAX_STD_ID:#x}",
            id + 1,
            id + LOG_ID_OFFSET
        );
    }
    Ok(id)
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
            serial: tty_serial(&name),
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

/// Read the USB serial string descriptor.
///
/// `/sys/class/tty/<tty>/device` is the USB *interface*; `serial` belongs to the
/// USB *device* one level up, so this reads the parent. Both CDC interfaces of
/// one composite board therefore report the same serial, which is exactly what
/// lets a single serial name a board rather than one of its channels.
fn tty_serial(tty: &str) -> Option<String> {
    let link = Path::new("/sys/class/tty").join(tty).join("device");
    let iface = std::fs::canonicalize(link).ok()?;
    let device = iface.parent()?;
    std::fs::read_to_string(device.join("serial"))
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
                selector: UsbSelector::PortPath("3-6".into())
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
        assert_eq!(
            r,
            Resolved::Serial {
                mgmt: PathBuf::from("/dev/null"),
                log: None
            }
        );
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
    fn usb_selectors_are_told_apart_by_shape() {
        // Port paths are strictly <bus>-<port>[.<port>]*, so the two forms are
        // disjoint sets rather than a guess.
        for path in ["3-6", "1-1.2", "3-4.1.2", "11-2"] {
            assert_eq!(
                UsbSelector::parse(path),
                UsbSelector::PortPath(path.into()),
                "{path} should read as a port path"
            );
        }
        for serial in [
            "feather-01",
            "arm-01",
            "3",
            "3-",
            "-4",
            "3-4a",
            "a-4",
            "3..4",
            "3-4.",
        ] {
            assert_eq!(
                UsbSelector::parse(serial),
                UsbSelector::Serial(serial.into()),
                "{serial} should read as a serial"
            );
        }
    }

    #[test]
    fn a_usb_label_round_trips_in_either_form() {
        for label in ["usb:3-6", "usb:feather-01"] {
            assert_eq!(Target::parse(label).unwrap().to_string(), label);
        }
    }

    #[test]
    fn node_ids_parse_in_hex_and_decimal() {
        assert_eq!(parse_node_id("0x42").unwrap(), 0x42);
        assert_eq!(parse_node_id("0X42").unwrap(), 0x42);
        assert_eq!(parse_node_id("66").unwrap(), 66);
        assert_eq!(parse_node_id(" 0x42 ").unwrap(), 0x42);
    }

    #[test]
    fn a_non_numeric_node_id_says_so() {
        let e = parse_node_id("frobnicate").unwrap_err().to_string();
        assert!(e.contains("not a number"), "{e}");
    }

    #[test]
    fn an_extended_can_id_is_refused() {
        // The firmware filters with .std_id and declares `range 0x0 0x7ff`, and
        // the host socket sets no CAN_EFF_FLAG. An 11-bit-overflowing id would
        // otherwise be masked on the wire and present as a bare timeout.
        let e = parse_node_id("0x800").unwrap_err().to_string();
        assert!(e.contains("maximum standard CAN identifier"), "{e}");
        assert_eq!(parse_node_id("0x7fd").unwrap(), 0x7fd);
    }

    #[test]
    fn a_node_id_with_no_room_for_the_reply_is_refused() {
        // A node owns three ids, so the top two standard ids cannot be used as
        // a host id however valid each looks on its own.
        for taken in ["0x7ff", "0x7fe"] {
            let e = parse_node_id(taken).unwrap_err().to_string();
            assert!(e.contains("no room for the two ids"), "{taken}: {e}");
        }
    }

    #[test]
    fn a_missing_can_interface_is_a_clear_error() {
        // Names an interface that cannot plausibly exist, so this is stable on
        // a machine that does have vcan0 configured.
        let t = Target::parse("can:definitelynotaniface/0x42").unwrap();
        let e = resolve(&t).unwrap_err().to_string();
        assert!(e.contains("no network interface named"), "{e}");
        // The remedy is in the message: this is the error a first-time user
        // hits before `modprobe vcan`, and it should not need a web search.
        assert!(e.contains("modprobe vcan"), "{e}");
    }

    #[test]
    fn channel_counting_is_uniform_across_transports() {
        let serial = Resolved::Serial {
            mgmt: PathBuf::from("/dev/ttyACM0"),
            log: None,
        };
        let can = Resolved::Can {
            iface: "vcan0".into(),
            node_id: 0x42,
            log: None,
        };
        assert_eq!(serial.channels(), 1);
        // A CAN target has a console channel of its own, two ids above its
        // management id, without anything being configured.
        assert_eq!(can.channels(), 2);
        assert_eq!(
            can.log_source(),
            Some(LogSource::Can {
                iface: "vcan0".into(),
                id: 0x44
            })
        );

        // An explicit serial log target overrides it: a board managed over the
        // bus whose console comes back over a wire is a real arrangement.
        let mut can = can;
        can.set_log(PathBuf::from("/dev/ttyACM1"));
        assert_eq!(can.channels(), 2);
        assert_eq!(
            can.log_source(),
            Some(LogSource::Serial(PathBuf::from("/dev/ttyACM1")))
        );
    }

    #[test]
    fn a_lock_key_resolves_symlinks_so_two_names_collapse_to_one() {
        // Two labels can name one board. If the occupancy claim compared literal
        // paths, `tty:/dev/ttyACM1` and a symlink to it would be claimed
        // independently -- two runtimes flashing one MCU.
        let dir = std::env::temp_dir().join("runtt-lockkey-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let real = dir.join("real-device");
        std::fs::write(&real, b"").unwrap();
        let link = dir.join("alias-device");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let direct = Resolved::Serial {
            mgmt: real.clone(),
            log: None,
        };
        let aliased = Resolved::Serial {
            mgmt: link.clone(),
            log: None,
        };
        assert_eq!(
            direct.lock_key(),
            aliased.lock_key(),
            "a symlinked device must produce the same occupancy key"
        );
        // ...while the Display stays the label the operator wrote.
        assert_ne!(direct.to_string(), aliased.to_string());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_can_lock_key_is_the_bus_address() {
        let can = Resolved::Can {
            iface: "vcan0".into(),
            node_id: 0x45,
            log: None,
        };
        // No path to canonicalise; the address IS the identity.
        assert_eq!(can.lock_key(), "can:vcan0/0x45");
    }

    #[test]
    fn a_can_target_displays_its_address_not_a_path() {
        let can = Resolved::Can {
            iface: "vcan0".into(),
            node_id: 0x42,
            log: None,
        };
        assert_eq!(can.to_string(), "can:vcan0/0x42");
    }
}
