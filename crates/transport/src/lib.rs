//! The transport seam.
//!
//! Everything above this boundary speaks SMP; everything below moves bytes.
//! USB (CDC-ACM), bare serial, and CAN (SMP over ISO-TP) all arrive here as a
//! placement label and leave as something `flash.rs` can open, without the SMP
//! logic above knowing which one it got.

use anyhow::Result;
use std::io::{Read, Write};

pub mod can;
pub mod resolve;
pub mod usb;

/// A target's placement label, transport-prefixed from day one so that
/// `can:` and `tty:` slot in later without breaking existing labels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// `usb:<selector>` — either a kernel port path (`usb:3-6`) or a board
    /// serial (`usb:feather-01`). See [`UsbSelector`].
    Usb { selector: UsbSelector },
    /// `tty:<device>` — e.g. `tty:ttyS3`, a bare serial port.
    Tty { device: String },
    /// `can:<iface>/<node-id>` — SMP over ISO-TP on a SocketCAN interface.
    /// The node id is a standard 11-bit identifier; the device replies on
    /// `node_id + 1`. Kept as a string here and parsed in `resolve`, so that
    /// parsing a label never needs a live bus.
    Can { iface: String, node_id: String },
}

/// Which board a `usb:` label means.
///
/// Two forms under one prefix, and they are told apart by SHAPE rather than by
/// guessing. A kernel USB port path is strictly `<bus>-<port>[.<port>]*` — digits,
/// hyphens and dots, nothing else — so the two sets are disjoint and the
/// disambiguation is total. `scripts/make-identity.py` refuses to write a serial
/// that would look like a port path, which is what keeps them disjoint in fact
/// and not merely by convention.
///
/// They answer different questions and both are legitimate:
///
/// * A **port path** means "whatever board is in this physical position". Right
///   when boards are replaceable and position defines the role -- swap a failed
///   controller and the replacement inherits the job untouched.
/// * A **serial** means "this specific board, wherever it is plugged in". Right
///   when inventory is fixed, and the only form that makes a compose file
///   portable between machines, since a port path encodes one host's USB tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsbSelector {
    /// A kernel USB port path, e.g. `3-6` or `1-1.2`.
    PortPath(String),
    /// A board serial from its flash identity record, published as the USB
    /// serial string descriptor so it is readable without talking to the board.
    Serial(String),
}

impl UsbSelector {
    /// Classify by shape. Never fails: anything not port-path-shaped is a serial.
    pub fn parse(s: &str) -> Self {
        if is_port_path(s) {
            UsbSelector::PortPath(s.to_string())
        } else {
            UsbSelector::Serial(s.to_string())
        }
    }
}

impl std::fmt::Display for UsbSelector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UsbSelector::PortPath(s) | UsbSelector::Serial(s) => write!(f, "{s}"),
        }
    }
}

/// Does this look like a kernel USB port path (`3-6`, `1-1.2`, `3-4.1.2`)?
///
/// Shared with `make-identity.py`, which refuses serials of this shape. Keep the
/// two in step: if this loosens, a previously-valid serial could start being read
/// as a port path.
pub fn is_port_path(s: &str) -> bool {
    let Some((bus, ports)) = s.split_once('-') else {
        return false;
    };
    if bus.is_empty() || !bus.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    // At least one port number, then any number of dot-separated ones.
    !ports.is_empty()
        && ports
            .split('.')
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

impl Target {
    /// Parse a placement label. Unknown prefixes are an error, not a guess.
    pub fn parse(label: &str) -> Result<Self> {
        let (prefix, rest) = label.split_once(':').ok_or_else(|| {
            anyhow::anyhow!(
                "placement label {label:?} has no transport prefix; expected e.g. `usb:3-6`"
            )
        })?;
        match prefix {
            "usb" => Ok(Target::Usb {
                selector: UsbSelector::parse(rest),
            }),
            "tty" => Ok(Target::Tty {
                device: rest.to_string(),
            }),
            "can" => {
                let (iface, node_id) = rest.split_once('/').ok_or_else(|| {
                    anyhow::anyhow!("can target {rest:?} must be `can:<iface>/<node-id>`")
                })?;
                Ok(Target::Can {
                    iface: iface.to_string(),
                    node_id: node_id.to_string(),
                })
            }
            other => anyhow::bail!(
                "unknown transport prefix {other:?} in {label:?}; known: usb, tty, can"
            ),
        }
    }
}

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Target::Usb { selector } => write!(f, "usb:{selector}"),
            Target::Tty { device } => write!(f, "tty:{device}"),
            Target::Can { iface, node_id } => write!(f, "can:{iface}/{node_id}"),
        }
    }
}

/// A bidirectional byte pipe to one channel of a target.
///
/// `mcumgr-toolkit` accepts any `Read + Write`, so an implementor of this trait
/// can be handed to it directly — which is also how the pty-backed mock and
/// native_sim are driven in tests.
pub trait Channel: Read + Write + Send {
    /// Human-readable identity, for logs.
    fn describe(&self) -> String;
}

/// The two channels the wire contract requires. Single-channel targets
/// (ESP32-C3 class, or bring-up over a probe's UART bridge) report `log: None`
/// and multiplex logs over the management channel's console framing.
pub struct Channels {
    pub mgmt: Box<dyn Channel>,
    pub log: Option<Box<dyn Channel>>,
}
