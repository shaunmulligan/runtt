//! The transport seam.
//!
//! Everything above this boundary speaks SMP; everything below moves bytes. The
//! PoC ships USB (CDC-ACM) and bare serial; CAN (SMP-over-ISO-TP) is the named
//! production follow-on and must be addable without touching SMP logic.

use anyhow::Result;
use std::io::{Read, Write};

pub mod resolve;
pub mod usb;

/// A target's placement label, transport-prefixed from day one so that
/// `can:` and `tty:` slot in later without breaking existing labels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// `usb:<port-path>` — e.g. `usb:3-6`, a kernel USB port path.
    Usb { port_path: String },
    /// `tty:<device>` — e.g. `tty:ttyS3`, a bare serial port.
    Tty { device: String },
    /// `can:<iface>/<node-id>` — not implemented this cycle; parsed so that a
    /// mislabelled service fails with a clear message rather than a timeout.
    Can { iface: String, node_id: String },
}

impl Target {
    /// Parse a placement label. Unknown prefixes are an error, not a guess.
    pub fn parse(label: &str) -> Result<Self> {
        let (prefix, rest) = label
            .split_once(':')
            .ok_or_else(|| anyhow::anyhow!(
                "placement label {label:?} has no transport prefix; expected e.g. `usb:3-6`"
            ))?;
        match prefix {
            "usb" => Ok(Target::Usb { port_path: rest.to_string() }),
            "tty" => Ok(Target::Tty { device: rest.to_string() }),
            "can" => {
                let (iface, node_id) = rest.split_once('/').ok_or_else(|| {
                    anyhow::anyhow!("can target {rest:?} must be `can:<iface>/<node-id>`")
                })?;
                Ok(Target::Can { iface: iface.to_string(), node_id: node_id.to_string() })
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
            Target::Usb { port_path } => write!(f, "usb:{port_path}"),
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
