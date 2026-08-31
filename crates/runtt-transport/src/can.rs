//! SMP over ISO-TP on a CAN bus.
//!
//! CAN frames carry 8 bytes (64 on CAN-FD) and an SMP packet runs to a kilobyte,
//! so something has to segment. Rather than invent that, this rides **ISO-TP**
//! (ISO 15765-2), which both ends already implement: Linux has the `can-isotp`
//! module, mainline since 5.10, and Zephyr has `subsys/canbus/isotp`. The kernel
//! does the segmentation, flow control and reassembly, and hands us whole
//! messages.
//!
//! That makes this a *datagram* transport, so it carries **raw** SMP frames --
//! an 8-byte header followed by CBOR, with none of the base64-and-CRC console
//! framing the serial transport needs. It is the same shape as `mcumgr-toolkit`'s
//! UDP transport, and deliberately modelled on it.
//!
//! ## Addressing
//!
//! A target is written `can:<iface>/<node-id>`, e.g. `can:vcan0/0x42`. The node
//! id is the address the **host sends to**; the device replies on `node-id + 1`.
//! Fixing the reply id by convention rather than configuring both halves keeps a
//! placement label to one number, which is what the OCI annotation carries.

use anyhow::{Context, Result, bail};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::time::Duration;

/// `AF_CAN` / `PF_CAN`, from `linux/socket.h`. Not in the `libc` crate's
/// constants for every target, so it is stated here.
const AF_CAN: libc::c_int = 29;
/// `CAN_ISOTP`, the protocol number from `linux/can.h`.
const CAN_ISOTP: libc::c_int = 6;

/// `struct sockaddr_can`, in the ISO-TP (`tp`) form of its address union.
///
/// Declared by hand because `libc` does not expose it. The layout is
/// `u16` family, two bytes of padding to align the interface index, `i32`
/// index, then the `tp` union arm of two CAN ids.
#[repr(C)]
#[derive(Default)]
struct SockAddrCanIsoTp {
    can_family: u16,
    _pad: u16,
    can_ifindex: i32,
    rx_id: u32,
    tx_id: u32,
}

/// One ISO-TP endpoint on a CAN interface.
pub struct IsoTpChannel {
    fd: OwnedFd,
    /// Reused across sends so a deploy does not allocate per frame.
    send_buffer: Vec<u8>,
    name: String,
}

impl IsoTpChannel {
    /// Open an ISO-TP socket on `iface`, talking to `node_id`.
    ///
    /// The device is expected to receive on `node_id` and reply on
    /// `node_id + 1`; see the module docs for why that is a convention rather
    /// than a setting.
    pub fn open(iface: &str, node_id: u32, timeout: Duration) -> Result<Self> {
        let ifindex = if_nametoindex(iface)
            .with_context(|| format!("no such CAN interface {iface:?}"))?;

        // SAFETY: a plain socket(2) with constants from linux/can.h.
        let fd = unsafe { libc::socket(AF_CAN, libc::SOCK_DGRAM, CAN_ISOTP) };
        if fd < 0 {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::EPROTONOSUPPORT) {
                bail!(
                    "the kernel has no ISO-TP support: {e}. Load it with \
                     `sudo modprobe can-isotp` (mainline since Linux 5.10)."
                );
            }
            return Err(anyhow::Error::new(e).context("failed to open an ISO-TP socket"));
        }
        // SAFETY: fd is a fresh, valid descriptor we have just checked.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };

        let addr = SockAddrCanIsoTp {
            can_family: AF_CAN as u16,
            _pad: 0,
            can_ifindex: ifindex,
            // The device receives on node_id, so that is where we transmit;
            // it answers one id higher.
            rx_id: node_id + 1,
            tx_id: node_id,
        };
        // SAFETY: addr is a correctly shaped sockaddr_can living for the call.
        let rc = unsafe {
            libc::bind(
                fd.as_raw_fd(),
                (&addr as *const SockAddrCanIsoTp).cast(),
                std::mem::size_of::<SockAddrCanIsoTp>() as libc::socklen_t,
            )
        };
        if rc < 0 {
            return Err(anyhow::Error::new(std::io::Error::last_os_error())
                .context(format!("failed to bind ISO-TP to {iface} node {node_id:#x}")));
        }

        let ch = Self {
            fd,
            send_buffer: Vec::new(),
            name: format!("can:{iface}/{node_id:#x}"),
        };
        ch.set_read_timeout(timeout)?;
        Ok(ch)
    }

    pub fn set_read_timeout(&self, timeout: Duration) -> Result<()> {
        let tv = libc::timeval {
            tv_sec: timeout.as_secs() as libc::time_t,
            tv_usec: timeout.subsec_micros() as libc::suseconds_t,
        };
        // SAFETY: tv is a valid timeval for the lifetime of the call.
        let rc = unsafe {
            libc::setsockopt(
                self.fd.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_RCVTIMEO,
                (&tv as *const libc::timeval).cast(),
                std::mem::size_of::<libc::timeval>() as libc::socklen_t,
            )
        };
        if rc < 0 {
            return Err(anyhow::Error::new(std::io::Error::last_os_error())
                .context("failed to set the ISO-TP receive timeout"));
        }
        Ok(())
    }

    pub fn describe(&self) -> String {
        self.name.clone()
    }

    fn raw(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}

/// `if_nametoindex(3)`, returning a real error rather than 0.
fn if_nametoindex(name: &str) -> Result<i32> {
    let c = std::ffi::CString::new(name).context("interface name contains a NUL")?;
    // SAFETY: c is a valid NUL-terminated string for the duration of the call.
    let idx = unsafe { libc::if_nametoindex(c.as_ptr()) };
    if idx == 0 {
        return Err(anyhow::Error::new(std::io::Error::last_os_error())
            .context(format!("failed to look up interface {name:?}")));
    }
    Ok(idx as i32)
}

impl std::io::Read for IsoTpChannel {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        // SAFETY: buf is a valid, writable slice of the length we pass.
        let n = unsafe { libc::read(self.raw(), buf.as_mut_ptr().cast(), buf.len()) };
        if n < 0 {
            let e = std::io::Error::last_os_error();
            // SO_RCVTIMEO expiry surfaces as EAGAIN; the SMP stack's retry logic
            // keys off TimedOut, so normalise it as the UDP transport does.
            if e.kind() == std::io::ErrorKind::WouldBlock {
                return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, e));
            }
            return Err(e);
        }
        Ok(n as usize)
    }
}

impl std::io::Write for IsoTpChannel {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        // SAFETY: buf is a valid slice of the length we pass.
        let n = unsafe { libc::write(self.raw(), buf.as_ptr().cast(), buf.len()) };
        if n < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(n as usize)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        // ISO-TP writes are whole messages handed to the kernel; nothing buffers
        // on our side.
        Ok(())
    }
}

impl IsoTpChannel {
    /// Send one raw SMP frame: header and body in a single ISO-TP message.
    pub fn send_frame(&mut self, header: &[u8], data: &[u8]) -> std::io::Result<()> {
        use std::io::Write;
        self.send_buffer.clear();
        self.send_buffer.extend_from_slice(header);
        self.send_buffer.extend_from_slice(data);
        let buf = std::mem::take(&mut self.send_buffer);
        let r = self.write_all(&buf);
        self.send_buffer = buf;
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sockaddr_can_matches_the_kernel_layout() {
        // u16 family + 2 pad + i32 ifindex + two u32 ids. If this drifts, bind()
        // silently addresses the wrong thing rather than failing.
        assert_eq!(std::mem::size_of::<SockAddrCanIsoTp>(), 16);
        assert_eq!(std::mem::align_of::<SockAddrCanIsoTp>(), 4);
    }

    #[test]
    fn a_missing_interface_is_a_clear_error() {
        let err = if_nametoindex("definitely-not-an-interface").unwrap_err();
        assert!(
            format!("{err:#}").contains("definitely-not-an-interface"),
            "the error should name the interface: {err:#}"
        );
    }
}

// ---------------------------------------------------------------------------
// Logs
// ---------------------------------------------------------------------------

/// `CAN_RAW`, the protocol number from `linux/can.h`.
const CAN_RAW: libc::c_int = 1;
/// `SOL_CAN_BASE + CAN_RAW`, the socket option level for raw CAN.
const SOL_CAN_RAW: libc::c_int = 100 + CAN_RAW;
/// `CAN_RAW_FILTER`, from `linux/can/raw.h`.
const CAN_RAW_FILTER: libc::c_int = 1;
/// `CAN_SFF_MASK` — match the full 11-bit standard identifier.
const CAN_SFF_MASK: u32 = 0x0000_07ff;

/// `struct sockaddr_can` in its plain form. Raw sockets bind by interface index
/// and ignore the address union, so the two id words are present for layout only.
#[repr(C)]
#[derive(Default)]
struct SockAddrCanRaw {
    can_family: u16,
    _pad: u16,
    can_ifindex: i32,
    _unused: [u32; 2],
}

/// `struct can_filter`, from `linux/can.h`.
#[repr(C)]
struct CanFilter {
    can_id: u32,
    can_mask: u32,
}

/// `struct can_frame`, from `linux/can.h`. Sixteen bytes: the id, a length and
/// three reserved bytes, then eight of payload aligned to eight.
#[repr(C)]
#[derive(Default)]
struct CanFrame {
    can_id: u32,
    len: u8,
    _pad: u8,
    _res0: u8,
    _len8_dlc: u8,
    data: [u8; 8],
}

/// The device's console output, arriving as raw CAN frames.
///
/// **Raw frames rather than ISO-TP, deliberately.** ISO-TP's flow control means
/// the sender waits for the receiver to say go, so a device logging to an ISO-TP
/// socket with nobody listening would block — and a blocking log backend
/// deadlocks boot. Raw frames are fire-and-forget: the device drops what it
/// cannot send, and losing log lines is enormously better than not booting.
///
/// Lossy under backpressure is therefore the *design*, not a defect. A dropped
/// frame shows up as a mangled line, which is honest about what happened.
///
/// Ordering is safe without a sequence number: CAN delivers frames of one
/// identifier from one sender in order, and this id has exactly one sender.
///
/// The log id also sits *above* the two management ids, which on CAN means lower
/// arbitration priority — so a firmware upload in progress wins the bus against
/// chatty logs rather than being slowed by them.
pub struct CanLogReader {
    fd: OwnedFd,
    /// Payload left over when the caller's buffer was smaller than a frame.
    pending: Vec<u8>,
    /// Read cursor into `pending`.
    at: usize,
    name: String,
}

impl CanLogReader {
    /// Listen for log frames on `iface` carrying identifier `id`.
    pub fn open(iface: &str, id: u32, timeout: Duration) -> Result<Self> {
        let ifindex = if_nametoindex(iface)
            .with_context(|| format!("no such CAN interface {iface:?}"))?;

        // SAFETY: a plain socket(2) with constants from linux/can.h.
        let fd = unsafe { libc::socket(AF_CAN, libc::SOCK_RAW, CAN_RAW) };
        if fd < 0 {
            return Err(anyhow::Error::new(std::io::Error::last_os_error())
                .context("failed to open a raw CAN socket"));
        }
        // SAFETY: fd is a fresh, valid descriptor we have just checked.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };

        let addr = SockAddrCanRaw {
            can_family: AF_CAN as u16,
            _pad: 0,
            can_ifindex: ifindex,
            _unused: [0; 2],
        };
        // SAFETY: addr is a correctly shaped sockaddr_can living for the call.
        let rc = unsafe {
            libc::bind(
                fd.as_raw_fd(),
                (&addr as *const SockAddrCanRaw).cast(),
                std::mem::size_of::<SockAddrCanRaw>() as libc::socklen_t,
            )
        };
        if rc < 0 {
            return Err(anyhow::Error::new(std::io::Error::last_os_error())
                .context(format!("failed to bind a raw CAN socket to {iface}")));
        }

        // Filter in the kernel rather than in userspace: on a busy bus this
        // socket would otherwise wake for every management frame as well.
        //
        // AFTER bind, not before. A filter set on an unbound raw CAN socket does
        // not survive the bind, so the socket ends up with the default
        // accept-everything filter -- or, as observed here, silently matching
        // nothing. This ordering is load-bearing.
        let filter = CanFilter {
            can_id: id,
            can_mask: CAN_SFF_MASK,
        };
        // SAFETY: filter is a correctly shaped can_filter living for the call.
        let rc = unsafe {
            libc::setsockopt(
                fd.as_raw_fd(),
                SOL_CAN_RAW,
                CAN_RAW_FILTER,
                (&filter as *const CanFilter).cast(),
                std::mem::size_of::<CanFilter>() as libc::socklen_t,
            )
        };
        if rc < 0 {
            return Err(anyhow::Error::new(std::io::Error::last_os_error())
                .context(format!("failed to filter raw CAN for id {id:#x}")));
        }

        let r = Self {
            fd,
            pending: Vec::new(),
            at: 0,
            name: format!("can:{iface} log id {id:#x}"),
        };
        r.set_read_timeout(timeout)?;
        Ok(r)
    }

    fn set_read_timeout(&self, timeout: Duration) -> Result<()> {
        let tv = libc::timeval {
            tv_sec: timeout.as_secs() as libc::time_t,
            tv_usec: timeout.subsec_micros() as libc::suseconds_t,
        };
        // SAFETY: tv is a valid timeval for the lifetime of the call.
        let rc = unsafe {
            libc::setsockopt(
                self.fd.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_RCVTIMEO,
                (&tv as *const libc::timeval).cast(),
                std::mem::size_of::<libc::timeval>() as libc::socklen_t,
            )
        };
        if rc < 0 {
            return Err(anyhow::Error::new(std::io::Error::last_os_error())
                .context("failed to set the raw CAN receive timeout"));
        }
        Ok(())
    }

    pub fn describe(&self) -> String {
        self.name.clone()
    }
}

impl std::io::Read for CanLogReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        // Drain whatever a previous frame left over before reading another.
        if self.at == self.pending.len() {
            let mut frame = CanFrame::default();
            // SAFETY: reading at most sizeof(can_frame) into a live can_frame.
            let n = unsafe {
                libc::read(
                    self.fd.as_raw_fd(),
                    (&mut frame as *mut CanFrame).cast(),
                    std::mem::size_of::<CanFrame>(),
                )
            };
            if n < 0 {
                return Err(std::io::Error::last_os_error());
            }
            let len = (frame.len as usize).min(frame.data.len());
            self.pending.clear();
            self.pending.extend_from_slice(&frame.data[..len]);
            self.at = 0;
        }
        let n = (self.pending.len() - self.at).min(buf.len());
        buf[..n].copy_from_slice(&self.pending[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}
