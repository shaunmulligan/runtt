//! Injectable faults.
//!
//! Each one corresponds to a real failure mode the runtime must survive, and
//! each is deterministic: a test asks for exactly one and gets it every time.

/// What the mock should do wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Fault {
    /// Behave correctly.
    #[default]
    None,
    /// Stop responding partway through an upload, as a yanked cable does.
    DisconnectMidUpload {
        /// Drop after this many upload chunks have been accepted.
        after_chunks: u32,
    },
    /// Accept the whole image but report `match: false` on the final chunk —
    /// the device computed a different SHA-256 than the client did.
    BadHash,
    /// Reply to an upload chunk with `off: 0`, demanding the client restart the
    /// transfer from the beginning (and re-send `len` and `sha`).
    RestartUpload {
        /// Demand the restart when the client reaches this offset.
        at_offset: u64,
    },
    /// Never answer a particular command, so the client must time out.
    Timeout { group: u16, cmd: u8 },
    /// Boot the new image once, then revert on the next reset because it was
    /// never confirmed.
    RevertOnBoot,
    /// Refuse to mark an image whose digest already failed once, so the runtime
    /// must surface a clear error instead of a reflash-revert storm.
    DigestAlreadyFailed,
}
