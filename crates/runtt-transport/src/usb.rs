//! USB CDC-ACM transport.
//!
//! Channel identity comes from the USB **interface string descriptor**
//! (`runtt-mgmt` / `runtt-log`), never from the interface number:
//! `ID_PATH` is interface-suffixed, so the two CDC channels of one composite
//! device get different `ID_PATH`s and their numbering is not contractual.

use crate::Channel;
use anyhow::{Context, Result};
use serialport::SerialPort;
use std::io::{Read, Write};
use std::time::Duration;

/// Interface string descriptors from the wire contract. See docs/WIRE_CONTRACT.md.
pub const IFACE_MGMT: &str = "runtt-mgmt";
pub const IFACE_LOG: &str = "runtt-log";

/// A serial port on one channel of a target.
///
/// **`TIOCEXCL` is deliberately disabled.** `serialport` sets it by default, but
/// it is a flag on the *terminal*, not on the file descriptor: it is only
/// cleared once every fd to that tty closes. So if anything else holds the
/// device open — native_sim holding its own pty, a log pump on a shared link,
/// the mock — then a process that exits leaves the flag set and the next open
/// fails with `EBUSY`. On a restart-policy cycle that turns one crash into a
/// permanently unstartable service.
///
/// We lose nothing by dropping it, because exclusivity is provided properly
/// elsewhere: our own `flock`-based occupancy lock is what guarantees one
/// service per MCU, and `ID_MM_DEVICE_IGNORE=1` in the udev rules is what keeps
/// ModemManager from probing mid-upload. `TIOCEXCL` was protecting against
/// neither, while adding a sticky failure mode.
pub struct SerialChannel {
    port: Box<dyn serialport::SerialPort>,
    name: String,
}

impl SerialChannel {
    pub fn open(path: &str, baud: u32, timeout: Duration) -> Result<Self> {
        // open_native() rather than open(), so we get a concrete TTYPort and can
        // turn off TIOCEXCL before anyone depends on it. See the type docs.
        let mut port = serialport::new(path, baud)
            .timeout(timeout)
            .open_native()
            .with_context(|| format!("failed to open serial port {path}"))?;
        port.set_exclusive(false)
            .with_context(|| format!("failed to clear TIOCEXCL on {path}"))?;
        Ok(Self {
            port: Box::new(port),
            name: path.to_string(),
        })
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
        Self {
            port,
            name: name.into(),
        }
    }

    /// Duplicate the underlying port, so one handle can read while another
    /// writes.
    ///
    /// Needed by the single-channel log demux: one thread must own the read
    /// side continuously (otherwise log output sits in the kernel buffer until
    /// the next SMP operation drains it), while the SMP client still needs to
    /// write requests. `serialport` keeps its timeout per handle rather than on
    /// the tty, so the two halves can be timed independently.
    pub fn try_clone(&self) -> Result<Self> {
        let port = self
            .port
            .try_clone()
            .with_context(|| format!("failed to duplicate {}", self.name))?;
        Ok(Self {
            port,
            name: self.name.clone(),
        })
    }

    /// Set this handle's read timeout. Affects only this handle.
    pub fn set_timeout(&mut self, timeout: Duration) -> Result<()> {
        self.port
            .set_timeout(timeout)
            .with_context(|| format!("failed to set timeout on {}", self.name))
    }

    /// A connected pty pair, for tests: `.0` stands in for the host side, `.1`
    /// for the device side.
    pub fn pty_pair() -> Result<(Self, Self)> {
        let (mut host, mut device) =
            serialport::TTYPort::pair().context("failed to allocate a pty pair")?;
        // Same reasoning as `open`: never leave a sticky TIOCEXCL behind.
        host.set_exclusive(false).ok();
        device.set_exclusive(false).ok();
        let host_name = host.name().unwrap_or_else(|| "pty-host".into());
        let dev_name = device.name().unwrap_or_else(|| "pty-device".into());
        Ok((
            Self::from_port(Box::new(host), host_name),
            Self::from_port(Box::new(device), dev_name),
        ))
    }
}
