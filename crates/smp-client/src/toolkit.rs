//! `SmpClient` implemented over `mcumgr-toolkit`.
//!
//! This file is the entire blast radius of that dependency. If the crate goes
//! quiet, is replaced by `mcumgr-smp`, or we end up writing our own framing,
//! nothing outside this module changes.

use crate::{ImageSlot, Progress, SmpClient};
use anyhow::{Context, Result};
use mcumgr_toolkit::client::MCUmgrClient;
use mcumgr_toolkit::transport::serial::ConfigurableTimeout;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::time::Duration;

pub struct ToolkitClient {
    inner: MCUmgrClient,
}

impl ToolkitClient {
    /// Wrap any byte pipe. The bound is `mcumgr-toolkit`'s, which is what lets
    /// the same code drive a real CDC-ACM port, a native_sim pty, and the mock.
    pub fn new<T>(channel: T, timeout: Duration) -> Result<Self>
    where
        T: Send + Read + Write + ConfigurableTimeout + 'static,
    {
        let inner = MCUmgrClient::new_from_serial(channel);
        inner
            .set_timeout(timeout)
            .map_err(|e| anyhow::anyhow!("failed to set SMP timeout: {e}"))?;
        Ok(Self { inner })
    }

    /// Ask the device for its own buffer sizes and size frames accordingly,
    /// rather than assuming Zephyr's default. Falls back silently: a device
    /// without the MCUmgr parameters command is not an error.
    pub fn tune_frame_size(&self) {
        if let Err(e) = self.inner.use_auto_frame_size() {
            tracing::debug!("could not auto-size SMP frames, keeping default: {e}");
        }
    }

    /// SHA-256 of an image, as the img group's `sha` field wants it.
    pub fn digest(image: &[u8]) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(image);
        h.finalize().into()
    }
}

fn convert(s: mcumgr_toolkit::commands::image::ImageState) -> ImageSlot {
    ImageSlot {
        image: Some(s.image),
        slot: s.slot,
        version: s.version,
        hash: s.hash,
        bootable: s.bootable,
        pending: s.pending,
        confirmed: s.confirmed,
        active: s.active,
        permanent: s.permanent,
    }
}

impl SmpClient for ToolkitClient {
    fn flash(&mut self, image: &[u8], progress: Option<&mut dyn Progress>) -> Result<()> {
        let checksum = Self::digest(image);

        // Adapt our Progress trait to the callback shape the toolkit wants.
        // Returning true keeps the upload going; we never cancel from here.
        let mut cb;
        let arg: Option<&mut dyn FnMut(u64, u64) -> bool> = match progress {
            Some(p) => {
                cb = move |done: u64, total: u64| {
                    p.advance(done, total);
                    true
                };
                Some(&mut cb)
            }
            None => None,
        };

        self.inner
            .image_upload(image, None, Some(checksum), false, arg)
            .context("SMP image upload failed")
    }

    fn echo(&mut self, payload: &str) -> Result<String> {
        self.inner.os_echo(payload).context("SMP echo failed")
    }

    fn image_list(&mut self) -> Result<Vec<ImageSlot>> {
        Ok(self
            .inner
            .image_get_state()
            .context("SMP image state read failed")?
            .into_iter()
            .map(convert)
            .collect())
    }

    fn reset(&mut self) -> Result<()> {
        // force=false: let the device refuse if it is mid-something important.
        self.inner
            .os_system_reset(false, None)
            .context("SMP reset failed")
    }

    fn set_state(&mut self, hash: &[u8], confirm: bool) -> Result<()> {
        self.inner
            .image_set_state(Some(hash), confirm)
            .map(|_| ())
            .with_context(|| {
                format!(
                    "SMP set-state failed (confirm={confirm}); \
                     the image may already have failed once"
                )
            })
    }
}
