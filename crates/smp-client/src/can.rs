//! An SMP transport over ISO-TP, so the client can drive a device on CAN.
//!
//! The byte pipe itself lives in `transport::can`; this is the thin SMP-shaped
//! layer over it. That split is deliberate and matches the serial path: the
//! `transport` crate knows about bearers and nothing about SMP, and this crate
//! knows about SMP and as little as possible about bearers.
//!
//! ISO-TP delivers whole messages, so a frame is simply the 8-byte SMP header
//! followed by the CBOR body -- no console framing, no CRC, nothing to
//! reassemble. That makes this very close to `mcumgr-toolkit`'s UDP transport,
//! which is the other datagram bearer it supports.

use mcumgr_toolkit::transport::{
    ReceiveError, SMP_HEADER_SIZE, SMP_TRANSFER_BUFFER_SIZE, SendError, Transport,
};
use std::io::{Read, Write};
use std::time::Duration;
use transport::can::IsoTpChannel;

/// SMP over ISO-TP on a CAN bus.
pub struct IsoTpTransport {
    ch: IsoTpChannel,
    /// Reused between sends so a firmware upload does not allocate per frame.
    send_buffer: Vec<u8>,
}

impl IsoTpTransport {
    pub fn new(ch: IsoTpChannel) -> Self {
        Self {
            ch,
            send_buffer: Vec::new(),
        }
    }

    pub fn describe(&self) -> String {
        self.ch.describe()
    }
}

impl Transport for IsoTpTransport {
    fn send_raw_frame(
        &mut self,
        header: [u8; SMP_HEADER_SIZE],
        data: &[u8],
    ) -> Result<(), SendError> {
        // One ISO-TP message per SMP frame. The kernel segments it into CAN
        // frames and handles flow control; we never see the fragments.
        self.send_buffer.clear();
        self.send_buffer.extend_from_slice(&header);
        self.send_buffer.extend_from_slice(data);
        self.ch.write_all(&self.send_buffer)?;
        Ok(())
    }

    fn recv_raw_frame<'a>(
        &mut self,
        buffer: &'a mut [u8; SMP_TRANSFER_BUFFER_SIZE],
    ) -> Result<&'a [u8], ReceiveError> {
        let len = self.ch.read(buffer)?;
        if len < SMP_HEADER_SIZE {
            // A runt message is not a partial frame to be continued: ISO-TP
            // delivers whole messages or nothing, so this is a malformed peer.
            return Err(ReceiveError::UnexpectedResponse);
        }
        Ok(&buffer[..len])
    }

    fn set_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.ch
            .set_read_timeout(timeout)
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.to_string().into() })
    }

    /// ISO-TP segments for us, so the ceiling is the protocol's own 4095-byte
    /// limit rather than anything about CAN frames. Cap well below it: the
    /// device's SMP buffer is the real constraint, and `use_auto_frame_size()`
    /// will lower this to whatever the device reports.
    fn max_smp_frame_size(&self) -> usize {
        1024
    }
}
