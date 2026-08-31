//! SMP console-transport framing.
//!
//! Per the Zephyr SMP transport spec. This is the part that costs three days if
//! you get it subtly wrong, so it is written once, here, and unit-tested against
//! the spec's own numbers.
//!
//! Wire format, outermost first:
//!
//! ```text
//! line := marker || base64( len_be16 || body || crc16_be16 ) || '\n'
//! body := smp_header(8) || cbor_payload
//! ```
//!
//! Byte 0 is a bitfield, and it is not laid out the way a casual reading of the
//! docs suggests. Zephyr's `struct smp_hdr` on a little-endian target is
//! `nh_op:3, nh_version:2, _res1:3`, so:
//!
//! ```text
//!   op      = byte0 & 0x07        (3 bits, not 4)
//!   version = (byte0 >> 3) & 0x03
//! ```
//!
//! Measured against `mcumgr-toolkit`: it sends `0x08` for a read and `0x0a` for
//! a write, i.e. **version 1 (SMP v2)**. The version must be echoed in the
//! response or the client rejects it as unexpected.
//!
//! Three further details that are easy to invert:
//!   * the CRC16 (XMODEM: poly 0x1021, init 0x0000) covers **the body only** —
//!     not the length prefix, and it is itself covered by the length;
//!   * `len` counts `body + 2` (the CRC) and does **not** count itself;
//!   * the first line of a packet carries `0x06 0x09`, every continuation
//!     carries `0x04 0x14`, and each line is at most 127 bytes including the
//!     marker and the terminating newline.

use anyhow::{bail, Result};
use base64::Engine;

/// Start-of-packet marker.
pub const MARKER_START: [u8; 2] = [0x06, 0x09];
/// Continuation marker. 0x14 is decimal 20.
pub const MARKER_CONT: [u8; 2] = [0x04, 0x14];
/// Hard limit baked into MCUmgr: 127 bytes per line, marker and newline included.
pub const MAX_LINE: usize = 127;

const CRC: crc::Crc<u16> = crc::Crc::<u16>::new(&crc::CRC_16_XMODEM);

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

/// An SMP header plus its CBOR payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// Low 3 bits of byte 0.
    pub op: u8,
    /// Bits 3-4 of byte 0. 0 = legacy, 1 = SMP v2. Echoed in responses.
    pub version: u8,
    pub flags: u8,
    pub group: u16,
    pub seq: u8,
    pub cmd: u8,
    pub payload: Vec<u8>,
}

/// SMP operations.
pub const OP_READ: u8 = 0;
pub const OP_READ_RSP: u8 = 1;
pub const OP_WRITE: u8 = 2;
pub const OP_WRITE_RSP: u8 = 3;

/// Group IDs we implement.
pub const GROUP_OS: u16 = 0;
pub const GROUP_IMG: u16 = 1;
/// User-defined groups start here; our `describe` command lives at this id.
pub const GROUP_PERUSER: u16 = 64;

impl Frame {
    pub fn header_bytes(&self) -> [u8; 8] {
        let len = self.payload.len() as u16;
        [
            (self.op & 0x07) | ((self.version & 0x03) << 3),
            self.flags,
            (len >> 8) as u8,
            (len & 0xff) as u8,
            (self.group >> 8) as u8,
            (self.group & 0xff) as u8,
            self.seq,
            self.cmd,
        ]
    }

    pub fn parse(body: &[u8]) -> Result<Self> {
        if body.len() < 8 {
            bail!("SMP body too short: {} bytes, need at least 8", body.len());
        }
        let declared = u16::from_be_bytes([body[2], body[3]]) as usize;
        let payload = &body[8..];
        if payload.len() != declared {
            bail!(
                "SMP length mismatch: header declares {declared} payload bytes, got {}",
                payload.len()
            );
        }
        Ok(Frame {
            op: body[0] & 0x07,
            version: (body[0] >> 3) & 0x03,
            flags: body[1],
            group: u16::from_be_bytes([body[4], body[5]]),
            seq: body[6],
            cmd: body[7],
            payload: payload.to_vec(),
        })
    }

    /// The response op for this request's op.
    pub fn response_op(&self) -> u8 {
        match self.op {
            OP_READ => OP_READ_RSP,
            OP_WRITE => OP_WRITE_RSP,
            other => other,
        }
    }
}

/// Frame a packet into wire lines.
pub fn encode(frame: &Frame, max_line: usize) -> Result<Vec<u8>> {
    if max_line < 16 {
        bail!("max_line {max_line} is implausibly small");
    }
    let mut body = Vec::with_capacity(8 + frame.payload.len());
    body.extend_from_slice(&frame.header_bytes());
    body.extend_from_slice(&frame.payload);

    // CRC over the body only, then length over body+CRC but not itself.
    let crc = CRC.checksum(&body);
    let mut inner = Vec::with_capacity(2 + body.len() + 2);
    inner.extend_from_slice(&((body.len() + 2) as u16).to_be_bytes());
    inner.extend_from_slice(&body);
    inner.extend_from_slice(&crc.to_be_bytes());

    let encoded = b64().encode(&inner);

    // Each line spends 2 bytes on its marker and 1 on the newline.
    let per_line = max_line - 3;
    let mut out = Vec::new();
    let mut first = true;
    for chunk in encoded.as_bytes().chunks(per_line) {
        out.extend_from_slice(if first { &MARKER_START } else { &MARKER_CONT });
        out.extend_from_slice(chunk);
        out.push(b'\n');
        first = false;
    }
    Ok(out)
}

/// Reassembles wire lines into packets.
#[derive(Default)]
pub struct Decoder {
    /// Accumulated base64 text of the packet currently being assembled.
    acc: Vec<u8>,
    in_packet: bool,
}

impl Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one complete line (without its trailing newline). Returns a body
    /// once a packet is complete and its CRC verifies.
    pub fn push_line(&mut self, line: &[u8]) -> Result<Option<Vec<u8>>> {
        if line.len() < 2 {
            return Ok(None);
        }
        let (marker, rest) = line.split_at(2);
        match marker {
            m if m == MARKER_START => {
                self.acc.clear();
                self.in_packet = true;
                self.acc.extend_from_slice(rest);
            }
            m if m == MARKER_CONT => {
                if !self.in_packet {
                    // A continuation with no start: a truncated packet, or we
                    // joined mid-stream. Drop it rather than mis-assembling.
                    return Ok(None);
                }
                self.acc.extend_from_slice(rest);
            }
            _ => return Ok(None),
        }

        // A packet is complete when the decoded length prefix is satisfied.
        let decoded = match b64().decode(&self.acc) {
            Ok(d) => d,
            // Not yet valid base64: more continuation lines to come.
            Err(_) => return Ok(None),
        };
        if decoded.len() < 2 {
            return Ok(None);
        }
        let declared = u16::from_be_bytes([decoded[0], decoded[1]]) as usize;
        if decoded.len() - 2 < declared {
            return Ok(None);
        }

        self.in_packet = false;
        let payload_and_crc = &decoded[2..2 + declared];
        if payload_and_crc.len() < 2 {
            bail!("packet too short to contain a CRC");
        }
        let split = payload_and_crc.len() - 2;
        let body = &payload_and_crc[..split];
        let got = u16::from_be_bytes([payload_and_crc[split], payload_and_crc[split + 1]]);
        let want = CRC.checksum(body);
        if got != want {
            bail!("CRC16 mismatch: got {got:#06x}, computed {want:#06x}");
        }
        Ok(Some(body.to_vec()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte0_matches_the_bytes_mcumgr_toolkit_actually_sends() {
        // Captured from mcumgr-toolkit 0.16.0 over a pty: read = 0x08,
        // write = 0x0a. Both carry version 1.
        for (op, expected) in [(OP_READ, 0x08u8), (OP_WRITE, 0x0a)] {
            let f = Frame {
                op,
                version: 1,
                flags: 0,
                group: 0,
                seq: 0,
                cmd: 0,
                payload: vec![],
            };
            assert_eq!(f.header_bytes()[0], expected, "op {op}");
        }
        // And the parse is the exact inverse.
        for (byte0, op) in [(0x08u8, OP_READ), (0x0a, OP_WRITE)] {
            let mut body = vec![byte0, 0, 0, 0, 0, 0, 0, 0];
            body.truncate(8);
            let f = Frame::parse(&body).unwrap();
            assert_eq!(f.op, op);
            assert_eq!(f.version, 1);
        }
    }

    #[test]
    fn crc16_xmodem_matches_the_known_vector() {
        // The canonical XMODEM check value for "123456789".
        assert_eq!(CRC.checksum(b"123456789"), 0x31C3);
    }

    #[test]
    fn header_is_big_endian_and_eight_bytes() {
        let f = Frame {
            op: OP_WRITE,
            version: 1,
            flags: 0,
            group: 0x0102,
            seq: 0x42,
            cmd: 0x07,
            payload: vec![0xa0], // CBOR empty map
        };
        // 0x0a = version 1 in bits 3-4, op 2 in bits 0-2. This is the exact
        // byte mcumgr-toolkit was observed to send for a write.
        assert_eq!(
            f.header_bytes(),
            [0x0a, 0x00, 0x00, 0x01, 0x01, 0x02, 0x42, 0x07]
        );
    }

    #[test]
    fn group_id_is_sixteen_bits() {
        // Group 64 (PERUSER) must survive the round trip; an 8-bit group field
        // would still pass, so also check a group above 255.
        for group in [GROUP_OS, GROUP_IMG, GROUP_PERUSER, 300, 0xBEEF] {
            let f = Frame {
                op: OP_READ,
                version: 1,
                flags: 0,
                group,
                seq: 1,
                cmd: 0,
                payload: vec![0xa0],
            };
            let wire = encode(&f, MAX_LINE).unwrap();
            let body = feed(&wire).expect("a complete packet");
            assert_eq!(Frame::parse(&body).unwrap().group, group);
        }
    }

    #[test]
    fn round_trips_a_single_line_packet() {
        let f = Frame {
            op: OP_WRITE,
            version: 1,
            flags: 0,
            group: GROUP_OS,
            seq: 9,
            cmd: 0,
            payload: vec![0xa0],
        };
        let wire = encode(&f, MAX_LINE).unwrap();
        assert_eq!(&wire[..2], &MARKER_START);
        assert_eq!(*wire.last().unwrap(), b'\n');
        assert_eq!(Frame::parse(&feed(&wire).unwrap()).unwrap(), f);
    }

    #[test]
    fn round_trips_a_fragmented_packet_and_uses_continuation_markers() {
        // 512 bytes of payload cannot fit in a 127-byte line.
        let f = Frame {
            op: OP_WRITE,
            version: 1,
            flags: 0,
            group: GROUP_IMG,
            seq: 3,
            cmd: 1,
            payload: (0..512).map(|i| (i % 251) as u8).collect(),
        };
        let wire = encode(&f, MAX_LINE).unwrap();
        let lines: Vec<&[u8]> = wire
            .split(|b| *b == b'\n')
            .filter(|l| !l.is_empty())
            .collect();
        assert!(
            lines.len() > 1,
            "expected fragmentation, got {} line(s)",
            lines.len()
        );
        assert_eq!(&lines[0][..2], &MARKER_START);
        for l in &lines[1..] {
            assert_eq!(&l[..2], &MARKER_CONT, "continuations must use 0x04 0x14");
        }
        assert_eq!(Frame::parse(&feed(&wire).unwrap()).unwrap(), f);
    }

    #[test]
    fn every_line_respects_the_127_byte_limit() {
        let f = Frame {
            op: OP_WRITE,
            version: 1,
            flags: 0,
            group: GROUP_IMG,
            seq: 0,
            cmd: 1,
            payload: vec![0x41; 4096],
        };
        let wire = encode(&f, MAX_LINE).unwrap();
        for line in wire.split_inclusive(|b| *b == b'\n') {
            assert!(
                line.len() <= MAX_LINE,
                "line of {} bytes exceeds 127",
                line.len()
            );
        }
    }

    #[test]
    fn rejects_a_corrupted_crc() {
        let f = Frame {
            op: OP_WRITE,
            version: 1,
            flags: 0,
            group: GROUP_OS,
            seq: 1,
            cmd: 0,
            payload: vec![0xa0],
        };
        let wire = encode(&f, MAX_LINE).unwrap();
        let line: Vec<u8> = wire.iter().copied().filter(|b| *b != b'\n').collect();
        // Flip a base64 character in the middle of the encoded body.
        let mut corrupted = line.clone();
        let i = corrupted.len() / 2;
        corrupted[i] = if corrupted[i] == b'A' { b'B' } else { b'A' };
        let mut d = Decoder::new();
        assert!(
            d.push_line(&corrupted).is_err(),
            "a flipped byte must not verify"
        );
    }

    #[test]
    fn a_stray_continuation_is_dropped_not_misassembled() {
        let mut d = Decoder::new();
        let mut stray = MARKER_CONT.to_vec();
        stray.extend_from_slice(b"AAAA");
        assert_eq!(d.push_line(&stray).unwrap(), None);
    }

    /// Feed whole wire bytes through a decoder, returning the first packet.
    fn feed(wire: &[u8]) -> Option<Vec<u8>> {
        let mut d = Decoder::new();
        for line in wire.split(|b| *b == b'\n') {
            if line.is_empty() {
                continue;
            }
            if let Some(body) = d.push_line(line).unwrap() {
                return Some(body);
            }
        }
        None
    }
}
