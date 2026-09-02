//! Split application log output from SMP traffic on a shared link.
//!
//! Two-channel targets keep the log channel separate and never come through
//! here. This is for the single-channel case — an ESP32-C3 class part, or
//! bring-up over a debug probe's UART bridge — where the application's console
//! output and the SMP management traffic share one byte pipe.
//!
//! That sharing is by design: the module deliberately selects MCUmgr's
//! **console** transport over the raw one, because the raw transport cannot
//! coexist with log output on a line. See `firmware/runtt/snippets/`.
//!
//! Without this, a single-channel container gets **no logs at all**.
//! `mcumgr-toolkit`'s receive path scans forward for the frame marker and
//! silently discards everything it steps over, so the application's output was
//! being dropped on the floor rather than reaching container stdio.
//!
//! ## Why a thread rather than filtering inline
//!
//! The obvious implementation filters inside `Read`, which only runs while the
//! SMP client is mid-operation. Between heartbeats nothing reads the port, so
//! log output would sit in the kernel buffer for up to a heartbeat interval and
//! then arrive in a burst — or overflow and be lost. Owning the read side on a
//! thread keeps logs live, which is the whole point of the feature.
//!
//! The SMP client therefore never touches the port for reading: it reads from a
//! queue this thread fills. Writes still go straight to a duplicate handle.

use anyhow::{Context, Result};
use mcumgr_toolkit::transport::serial::ConfigurableTimeout;
use runtt_transport::usb::SerialChannel;
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::time::Duration;

/// SMP console framing markers. The first line of a packet carries
/// `0x06 0x09`; continuations carry `0x04 0x14`. See `docs/WIRE_CONTRACT.md`.
///
/// Deliberately restated here rather than shared with `runtt-mock`: the mock is
/// the independent second opinion in the tests, and a single shared constant
/// would let one bug satisfy both sides.
pub const MARKER_START: [u8; 2] = [0x06, 0x09];
pub const MARKER_CONT: [u8; 2] = [0x04, 0x14];

/// How long the reader blocks before looping. Short enough to notice the device
/// going away promptly, long enough not to spin.
const POLL: Duration = Duration::from_millis(250);

/// Is this line an SMP frame rather than application output?
///
/// Classification is by prefix alone, which is sound because the two producers
/// are different subsystems: MCUmgr writes framed base64 with these markers,
/// and the logging backend writes text. A log line that happened to begin with
/// these two control bytes would be misrouted, which is both vanishingly
/// unlikely in text output and strictly better than today's behaviour of
/// discarding every log line.
pub fn is_smp_line(line: &[u8]) -> bool {
    line.starts_with(&MARKER_START) || line.starts_with(&MARKER_CONT)
}

/// A shared link, with log output peeled off and sent to `stdout`.
///
/// Handed to `ToolkitClient::new` in place of the raw channel, so the SMP
/// client sees only SMP bytes.
pub struct LogDemux {
    /// SMP lines, newline-terminated, from the reader thread.
    smp: mpsc::Receiver<Vec<u8>>,
    /// SMP bytes not yet handed to the caller.
    pending: VecDeque<u8>,
    /// Write side. The reader thread owns the read side.
    writer: SerialChannel,
    /// Applied to queue reads, so `probe_settings` still means what it says.
    timeout: Duration,
    /// Asks the reader to stop; see the `Drop` impl for why joining matters.
    stop: Arc<AtomicBool>,
    reader: Option<std::thread::JoinHandle<()>>,
}

/// Stop the reader and **wait for it**, so the port is closed before we return.
///
/// Signalling alone is not enough. `serialport` takes an `flock(LOCK_EX)` on
/// open, and the reader thread owns that handle — so a thread still winding
/// down holds the lock, and the next `open` of the same device fails with
/// "Unable to acquire exclusive lock on serial port". That is exactly the
/// reconnect after a deploy reset, i.e. every deploy. Joining makes the
/// close-before-reopen ordering a property of the type rather than a race.
impl Drop for LogDemux {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

impl LogDemux {
    /// Take over `channel`'s read side and start peeling off log lines.
    pub fn new(channel: SerialChannel, timeout: Duration) -> Result<Self> {
        Self::with_sink(channel, timeout, |line| {
            println!("{}", String::from_utf8_lossy(line));
        })
    }

    /// As `new`, but with the log destination injected — the seam the tests
    /// use to assert what was routed where.
    pub fn with_sink<S>(mut channel: SerialChannel, timeout: Duration, sink: S) -> Result<Self>
    where
        S: FnMut(&[u8]) + Send + 'static,
    {
        let writer = channel
            .try_clone()
            .context("failed to duplicate the management channel for writing")?;

        // The reader loops on its own cadence; the caller's timeout governs how
        // long the SMP client waits on the queue, which is a different thing.
        channel.set_timeout(POLL)?;

        let (tx, smp) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let reader_stop = Arc::clone(&stop);
        let reader = std::thread::spawn(move || pump(channel, &tx, sink, &reader_stop));

        Ok(Self {
            smp,
            pending: VecDeque::new(),
            writer,
            timeout,
            stop,
            reader: Some(reader),
        })
    }
}

/// Read the shared link forever, routing whole lines by their prefix.
///
/// Returns when the device goes away, which drops the sender and turns the
/// client's next read into EOF — reported upward as a failed heartbeat, which
/// is what makes the container exit non-zero. Also returns when asked to stop,
/// within one poll interval, so the port is released promptly for a reconnect.
fn pump<S>(mut channel: SerialChannel, tx: &mpsc::Sender<Vec<u8>>, mut sink: S, stop: &AtomicBool)
where
    S: FnMut(&[u8]),
{
    let mut line: Vec<u8> = Vec::with_capacity(128);
    let mut buf = [0u8; 512];

    while !stop.load(Ordering::SeqCst) {
        let n = match channel.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            // A quiet link is the normal case, not a failure.
            Err(e)
                if e.kind() == std::io::ErrorKind::TimedOut
                    || e.kind() == std::io::ErrorKind::WouldBlock =>
            {
                continue
            }
            Err(_) => break,
        };

        for &byte in &buf[..n] {
            if byte != b'\n' {
                line.push(byte);
                continue;
            }

            if is_smp_line(&line) {
                // Restore the terminator: the toolkit's frame reader looks for
                // it to close a chunk.
                line.push(b'\n');
                if tx.send(std::mem::take(&mut line)).is_err() {
                    return; // client gone
                }
                line = Vec::with_capacity(128);
            } else {
                // Zephyr's console backend emits CRLF; the SMP transport does
                // not, so this only ever trims log output.
                let end = line.strip_suffix(b"\r").unwrap_or(&line);
                if !end.is_empty() {
                    sink(end);
                }
                line.clear();
            }
        }
    }
}

impl Read for LogDemux {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.pending.is_empty() {
            match self.smp.recv_timeout(self.timeout) {
                Ok(line) => self.pending.extend(line),
                Err(RecvTimeoutError::Timeout) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "timed out waiting for an SMP frame on the shared link",
                    ))
                }
                // The reader stopped: the device is gone. Zero signals EOF,
                // which the toolkit turns into an error rather than a hang.
                Err(RecvTimeoutError::Disconnected) => return Ok(0),
            }
        }

        let n = buf.len().min(self.pending.len());
        for (slot, byte) in buf.iter_mut().zip(self.pending.drain(..n)) {
            *slot = byte;
        }
        Ok(n)
    }
}

impl Write for LogDemux {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.writer.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }
}

/// Applies to the queue rather than the port.
///
/// `mcumgr-toolkit` grants this through a blanket impl over
/// `AsMut<dyn SerialPort>`; we implement it directly instead, deliberately not
/// exposing the port. Setting a timeout on the write handle would not govern
/// how long the client waits for a *response*, which is what callers such as
/// `probe_settings` actually mean.
impl ConfigurableTimeout for LogDemux {
    fn set_timeout(
        &mut self,
        duration: Duration,
    ) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.timeout = duration;
        Ok(())
    }
}
