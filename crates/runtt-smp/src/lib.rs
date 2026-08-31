//! The five-method SMP surface the runtime actually needs.
//!
//! Deliberately narrow. `mcumgr-toolkit` is pinned exactly (`=0.16.0`) because
//! it is young with a small maintainer base; keeping the runtime behind this
//! trait means replacing it is a one-file change rather than a refactor.

use anyhow::Result;

pub mod can;
pub mod demux;
pub mod describe;
pub mod mcuboot;
pub mod toolkit;
pub use demux::LogDemux;
pub use toolkit::ToolkitClient;

/// One image slot as reported by the SMP img group (group 1, command 0).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageSlot {
    pub image: Option<u32>,
    /// 0 = primary (running), 1 = secondary (staging).
    pub slot: u32,
    pub version: String,
    /// SHA-256 of the image, as reported by the device.
    pub hash: Option<Vec<u8>>,
    pub bootable: bool,
    /// Marked for swap on next reset.
    pub pending: bool,
    /// Will not revert.
    pub confirmed: bool,
    /// Currently executing.
    pub active: bool,
    pub permanent: bool,
}

/// Progress during an upload, so the runtime can log something useful during
/// the ~25-30s a 200 KB image takes over a 115200 baud bring-up link.
pub trait Progress: Send {
    fn advance(&mut self, uploaded: u64, total: u64);
}

/// What the runtime needs from an SMP server. Nothing more.
pub trait SmpClient: Send {
    /// Upload an image into the secondary slot. Does **not** mark it.
    fn flash(&mut self, image: &[u8], progress: Option<&mut dyn Progress>) -> Result<()>;

    /// `os` group echo — the heartbeat. Proves the kernel is alive.
    fn echo(&mut self, payload: &str) -> Result<String>;

    /// `img` group state read.
    fn image_list(&mut self) -> Result<Vec<ImageSlot>>;

    /// `os` group reset. On native_sim this re-execs the process.
    fn reset(&mut self) -> Result<()>;

    /// Mark an image. `confirm=false` is **test** (reverts on the next reset
    /// unless confirmed); `confirm=true` is permanent.
    ///
    /// The safety invariant: only ever call this with `confirm=true` *after* the
    /// new image has enumerated, spoken SMP and heartbeated. Confirmation must
    /// be reachable only through the contract, so an image that broke the
    /// contract can never be confirmed.
    fn set_state(&mut self, hash: &[u8], confirm: bool) -> Result<()>;
}
