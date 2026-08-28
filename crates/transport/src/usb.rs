//! USB CDC-ACM transport.
//!
//! Channel identity comes from the USB **interface string descriptor**
//! (`balena-mcu-mgmt` / `balena-mcu-log`), never from the interface number:
//! `ID_PATH` is interface-suffixed, so the two CDC channels of one composite
//! device get different `ID_PATH`s and their numbering is not contractual.

use crate::Channel;
use anyhow::{Context, Result};
use std::io::{Read, Write};
use std::time::Duration;

/// Interface string descriptors from the wire contract. See docs/WIRE_CONTRACT.md.
pub const IFACE_MGMT: &str = "balena-mcu-mgmt";
pub const IFACE_LOG: &str = "balena-mcu-log";

/// A serial port opened exclusively (`TIOCEXCL`, which `serialport` sets by
/// default) so that a stray ModemManager probe cannot interleave with an
/// in-flight SMP upload.
pub struct SerialChannel {
    port: Box<dyn serialport::SerialPort>,
    name: String,
}

impl SerialChannel {
    pub fn open(path: &str, baud: u32, timeout: Duration) -> Result<Self> {
        let port = serialport::new(path, baud)
            .timeout(timeout)
            .open()
            .with_context(|| format!("failed to open serial port {path}"))?;
        Ok(Self { port, name: path.to_string() })
    }
}

impl Read for SerialChannel {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.port.read(buf)
    }
}

impl Write for SerialChannel {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.port.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.port.flush()
    }
}

impl Channel for SerialChannel {
    fn describe(&self) -> String {
        self.name.clone()
    }
}
