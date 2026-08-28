//! The SMP server loop: codec + device, over any byte pipe.

use crate::codec::{self, Decoder, Frame};
use crate::device::{Device, UploadOutcome};
use crate::faults::Fault;
use anyhow::Result;
use ciborium::Value;
use std::io::{Read, Write};

/// os group command ids.
const OS_ECHO: u8 = 0;
const OS_RESET: u8 = 5;
/// MCUmgr buffer parameters. A client uses this to size its frames instead of
/// assuming Zephyr's defaults, so a mock that answers ENOTSUP here silently
/// pushes every client onto the fallback path.
const OS_MCUMGR_PARAMS: u8 = 6;
/// img group command ids.
const IMG_STATE: u8 = 0;
const IMG_UPLOAD: u8 = 1;
const IMG_ERASE: u8 = 5;
/// Our custom group's only command.
const DESCRIBE: u8 = 0;

/// MGMT_ERR_EINVAL, as Zephyr numbers it.
const ERR_EINVAL: i64 = 3;
/// MGMT_ERR_ENOTSUP.
const ERR_ENOTSUP: i64 = 8;

/// An SMP error payload. v2 uses a nested `err` map keyed by group; v0/v1 uses a
/// flat `rc`. We answer in whichever dialect the request used.
fn error_payload(version: u8, group: u16, rc: i64) -> Vec<u8> {
    if version >= 1 {
        map(vec![(
            "err",
            Value::Map(vec![
                (
                    Value::Text("group".into()),
                    Value::Integer((group as i64).into()),
                ),
                (Value::Text("rc".into()), Value::Integer(rc.into())),
            ]),
        )])
    } else {
        map(vec![("rc", Value::Integer(rc.into()))])
    }
}

pub struct Server<T> {
    io: T,
    dev: Device,
    decoder: Decoder,
    /// Once set, stop answering: the emulation of a yanked cable.
    silent: bool,
}

impl<T: Read + Write> Server<T> {
    pub fn new(io: T, fault: Fault) -> Self {
        Self {
            io,
            dev: Device::provisioned(fault),
            decoder: Decoder::new(),
            silent: false,
        }
    }

    pub fn device(&self) -> &Device {
        &self.dev
    }

    /// Serve indefinitely.
    ///
    /// **A host disconnecting is not a reason to stop.** A real device stays
    /// powered and available when the host closes the port, and our runtime
    /// deliberately disconnects and reconnects around a reset. A server that
    /// exited on EOF would leave nothing listening for the reconnect, which
    /// presents as an intermittent hang rather than a clear failure.
    pub fn serve(&mut self) -> Result<()> {
        let mut line = Vec::new();
        // Read in chunks rather than a byte at a time: an upload is thousands of
        // bytes and one syscall per byte dominated the cost.
        let mut buf = [0u8; 512];
        let mut pending: std::collections::VecDeque<u8> = std::collections::VecDeque::new();
        loop {
            if pending.is_empty() {
                match self.io.read(&mut buf) {
                    Ok(0) => {
                        // Host went away. Drop any half-assembled packet so the
                        // next client starts clean, and keep waiting.
                        line.clear();
                        self.decoder = Decoder::new();
                        std::thread::sleep(std::time::Duration::from_millis(20));
                        continue;
                    }
                    Ok(n) => pending.extend(&buf[..n]),
                    // A timeout on the device side just means the client is idle.
                    Err(e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    // EIO on a pty master means the slave side closed: same as EOF.
                    Err(_) => {
                        line.clear();
                        self.decoder = Decoder::new();
                        std::thread::sleep(std::time::Duration::from_millis(20));
                        continue;
                    }
                }
            }
            let Some(b) = pending.pop_front() else {
                continue;
            };
            if b != b'\n' {
                line.push(b);
                continue;
            }

            let complete = self.decoder.push_line(&line);
            line.clear();
            let body = match complete {
                Ok(Some(b)) => b,
                Ok(None) => continue,
                // A bad CRC is the device's cue to drop the packet, not to die.
                Err(e) => {
                    tracing::warn!("dropping malformed packet: {e}");
                    continue;
                }
            };

            let req = match Frame::parse(&body) {
                Ok(f) => f,
                Err(e) => {
                    tracing::warn!("unparseable SMP frame: {e}");
                    continue;
                }
            };

            if self.silent {
                continue;
            }
            if let Fault::Timeout { group, cmd } = self.dev.fault {
                if req.group == group && req.cmd == cmd {
                    tracing::info!("fault: withholding response for group {group} cmd {cmd}");
                    continue;
                }
            }

            if let Some(resp) = self.dispatch(&req)? {
                let wire = codec::encode(&resp, codec::MAX_LINE)?;
                self.io.write_all(&wire)?;
                self.io.flush()?;
            }
        }
    }

    fn dispatch(&mut self, req: &Frame) -> Result<Option<Frame>> {
        let payload = decode_map(&req.payload);
        let out = match (req.group, req.cmd) {
            (codec::GROUP_OS, OS_ECHO) => {
                let d = get_str(&payload, "d").unwrap_or_default();
                map(vec![("r", Value::Text(d))])
            }
            (codec::GROUP_OS, OS_MCUMGR_PARAMS) => map(vec![
                // Matches Zephyr's own defaults: CONFIG_MCUMGR_TRANSPORT_NETBUF_SIZE
                // and _COUNT. buf_size includes the SMP header.
                ("buf_size", Value::Integer(384.into())),
                ("buf_count", Value::Integer(4.into())),
            ]),
            (codec::GROUP_OS, OS_RESET) => {
                self.dev.reset();
                map(vec![])
            }
            (codec::GROUP_IMG, IMG_STATE) if req.op == codec::OP_READ => self.state_payload(),
            (codec::GROUP_IMG, IMG_STATE) => {
                let hash = get_bytes(&payload, "hash");
                let confirm = get_bool(&payload, "confirm").unwrap_or(false);
                match self.dev.set_state(hash.as_deref(), confirm) {
                    Ok(()) => self.state_payload(),
                    Err(msg) => {
                        tracing::info!("set_state refused: {msg}");
                        error_payload(req.version, req.group, ERR_EINVAL)
                    }
                }
            }
            (codec::GROUP_IMG, IMG_UPLOAD) => {
                let off = get_u64(&payload, "off").unwrap_or(0);
                let len = get_u64(&payload, "len");
                let sha = get_bytes(&payload, "sha");
                let data = get_bytes(&payload, "data").unwrap_or_default();
                match self.dev.upload_chunk(off, len, sha, &data) {
                    UploadOutcome::GoSilent => {
                        self.silent = true;
                        return Ok(None);
                    }
                    UploadOutcome::Continue { off } => {
                        map(vec![("off", Value::Integer((off as i64).into()))])
                    }
                    UploadOutcome::Complete { off, matches } => map(vec![
                        ("off", Value::Integer((off as i64).into())),
                        ("match", Value::Bool(matches)),
                    ]),
                }
            }
            (codec::GROUP_IMG, IMG_ERASE) => {
                self.dev.slot1 = None;
                map(vec![])
            }
            (codec::GROUP_PERUSER, DESCRIBE) => map(vec![
                ("contract", Value::Text("1.0.0".into())),
                ("board", Value::Text("smp-mock".into())),
                ("app_version", Value::Text("0.1.0".into())),
                ("channels", Value::Integer(2.into())),
            ]),
            (g, c) => {
                tracing::info!("unsupported group {g} cmd {c}");
                error_payload(req.version, g, ERR_ENOTSUP)
            }
        };

        Ok(Some(Frame {
            op: req.response_op(),
            // Echo the request's version. A v2 client rejects a v0 response.
            version: req.version,
            flags: 0,
            group: req.group,
            seq: req.seq,
            cmd: req.cmd,
            payload: out,
        }))
    }

    fn state_payload(&self) -> Vec<u8> {
        let images: Vec<Value> = self
            .dev
            .image_states()
            .into_iter()
            .map(|r| {
                Value::Map(vec![
                    (Value::Text("image".into()), Value::Integer(0.into())),
                    (
                        Value::Text("slot".into()),
                        Value::Integer((r.slot as i64).into()),
                    ),
                    (
                        Value::Text("version".into()),
                        Value::Text(r.img.version.clone()),
                    ),
                    (Value::Text("hash".into()), Value::Bytes(r.img.hash.clone())),
                    (Value::Text("bootable".into()), Value::Bool(r.img.bootable)),
                    (Value::Text("pending".into()), Value::Bool(r.img.pending)),
                    (
                        Value::Text("confirmed".into()),
                        Value::Bool(r.img.confirmed),
                    ),
                    (Value::Text("active".into()), Value::Bool(r.active)),
                    (
                        Value::Text("permanent".into()),
                        Value::Bool(r.img.permanent),
                    ),
                ])
            })
            .collect();
        let v = Value::Map(vec![(Value::Text("images".into()), Value::Array(images))]);
        let mut buf = Vec::new();
        ciborium::into_writer(&v, &mut buf).expect("CBOR encode of our own value");
        buf
    }
}

// --- small CBOR helpers -----------------------------------------------------
// Requests are read tolerantly: an unknown or missing field yields None rather
// than an error, because a mock that is picky about encoding tests the encoder
// rather than the runtime.

fn map(entries: Vec<(&str, Value)>) -> Vec<u8> {
    let v = Value::Map(
        entries
            .into_iter()
            .map(|(k, v)| (Value::Text(k.to_string()), v))
            .collect(),
    );
    let mut buf = Vec::new();
    ciborium::into_writer(&v, &mut buf).expect("CBOR encode of our own value");
    buf
}

fn decode_map(bytes: &[u8]) -> Vec<(Value, Value)> {
    match ciborium::from_reader::<Value, _>(bytes) {
        Ok(Value::Map(m)) => m,
        _ => Vec::new(),
    }
}

fn lookup<'a>(m: &'a [(Value, Value)], key: &str) -> Option<&'a Value> {
    m.iter()
        .find(|(k, _)| matches!(k, Value::Text(t) if t == key))
        .map(|(_, v)| v)
}

fn get_str(m: &[(Value, Value)], key: &str) -> Option<String> {
    match lookup(m, key)? {
        Value::Text(t) => Some(t.clone()),
        _ => None,
    }
}

fn get_bool(m: &[(Value, Value)], key: &str) -> Option<bool> {
    match lookup(m, key)? {
        Value::Bool(b) => Some(*b),
        _ => None,
    }
}

fn get_u64(m: &[(Value, Value)], key: &str) -> Option<u64> {
    match lookup(m, key)? {
        Value::Integer(i) => u128::try_from(*i).ok().and_then(|v| u64::try_from(v).ok()),
        _ => None,
    }
}

fn get_bytes(m: &[(Value, Value)], key: &str) -> Option<Vec<u8>> {
    match lookup(m, key)? {
        Value::Bytes(b) => Some(b.clone()),
        _ => None,
    }
}
