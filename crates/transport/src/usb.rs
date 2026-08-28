//! USB CDC-ACM transport.
//!
//! Channel identity comes from the USB **interface string descriptor**
//! (`balena-mcu-mgmt` / `balena-mcu-log`), never from the interface number:
//! `ID_PATH` is interface-suffixed, so the two CDC channels of one composite
//! device get different `ID_PATH`s and their numbering is not contractual.

use crate::Channel;
use anyhow::{Context, Result};
use serialport::SerialPort;
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

/// `mcumgr-toolkit` grants `ConfigurableTimeout` through a blanket impl over
/// `AsMut<dyn SerialPort>`, so exposing the inner port is all that is needed to
/// hand this channel straight to `MCUmgrClient::new_from_serial`.
impl AsMut<dyn serialport::SerialPort> for SerialChannel {
    fn as_mut(&mut self) -> &mut (dyn serialport::SerialPort + 'static) {
        self.port.as_mut()
    }
}

impl SerialChannel {
    /// Wrap an already-open port — used by the test harness to drive a pty pair
    /// and by native_sim, where the "port" is a `/dev/pts/N`.
    pub fn from_port(port: Box<dyn serialport::SerialPort>, name: impl Into<String>) -> Self {
        Self { port, name: name.into() }
    }

    /// A connected pty pair, for tests: `.0` stands in for the host side, `.1`
    /// for the device side.
    pub fn pty_pair() -> Result<(Self, Self)> {
        let (host, device) = serialport::TTYPort::pair().context("failed to allocate a pty pair")?;
        let host_name = host.name().unwrap_or_else(|| "pty-host".into());
        let dev_name = device.name().unwrap_or_else(|| "pty-device".into());
        Ok((
            Self::from_port(Box::new(host), host_name),
            Self::from_port(Box::new(device), dev_name),
        ))
    }
}
