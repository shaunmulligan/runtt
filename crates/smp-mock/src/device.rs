//! The emulated image-slot state machine.
//!
//! Models MCUboot's slot semantics closely enough to exercise the runtime's
//! logic, and no further:
//!
//! ```text
//! upload -> slot1
//! set_state(hash, confirm=false)  => slot1 pending,  not permanent   [TEST]
//! reset  => slot1 swaps into slot0, active, NOT confirmed
//!   confirm  => confirmed, permanent                                 [CONFIRMED]
//!   no confirm, next reset => the old image swaps back               [REVERT]
//!
//! set_state(hash, confirm=true) => pending and permanent             [PERMANENT]
//! ```
//!
//! The invariant the whole design rests on: **confirmation is only reachable
//! through the contract.** An image that cannot speak SMP cannot be confirmed,
//! so contract loss is never permanent.

use crate::faults::Fault;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Default)]
pub struct Image {
    pub version: String,
    pub hash: Vec<u8>,
    pub bytes: usize,
    pub bootable: bool,
    pub pending: bool,
    pub confirmed: bool,
    pub permanent: bool,
}

impl Image {
    /// Build an image record the way a real device would.
    ///
    /// The reported hash is the **MCUboot image digest** from the image's TLV
    /// area, not the SHA-256 of the file bytes. That distinction is not
    /// cosmetic: it is the identity `set_state` matches on, and getting it wrong
    /// produces IMG_MGMT_ERR_HASH_NOT_FOUND on real firmware. The mock reports
    /// the same thing so that a client which works here works there.
    ///
    /// Falls back to hashing the bytes when the payload is not a valid MCUboot
    /// image, so tests that only care about transport behaviour can still use
    /// arbitrary blobs.
    pub fn from_bytes(data: &[u8], fallback_version: &str) -> Self {
        let (hash, version) = match smp_client::mcuboot::parse(data) {
            Ok(info) => (info.digest.to_vec(), info.version),
            Err(_) => {
                let mut h = Sha256::new();
                h.update(data);
                (h.finalize().to_vec(), fallback_version.to_string())
            }
        };
        Self {
            version,
            hash,
            bytes: data.len(),
            bootable: true,
            pending: false,
            confirmed: false,
            permanent: false,
        }
    }
}

/// An in-progress upload.
#[derive(Debug, Default)]
pub struct Upload {
    pub expected_len: u64,
    pub declared_sha: Option<Vec<u8>>,
    pub received: Vec<u8>,
    pub chunks: u32,
    /// Set once we have demanded a restart, so we only do it once.
    pub restart_demanded: bool,
}

pub struct Device {
    /// slot 0 — the running image.
    pub slot0: Option<Image>,
    /// slot 1 — the staging slot.
    pub slot1: Option<Image>,
    pub upload: Upload,
    pub fault: Fault,
    /// Digests that have already failed to boot. A real runtime must not
    /// reflash these in a loop.
    pub failed_digests: Vec<Vec<u8>>,
    /// Incremented on every reset, so tests can assert reboots happened.
    pub resets: u32,
    /// True once a test-marked image has booted but not yet been confirmed.
    pub awaiting_confirm: bool,
}

impl Device {
    /// A freshly provisioned board: a confirmed image in slot 0, nothing staged.
    pub fn provisioned(fault: Fault) -> Self {
        let mut img = Image::from_bytes(b"factory-image", "1.0.0+factory");
        img.confirmed = true;
        img.permanent = true;
        Self {
            slot0: Some(img),
            slot1: None,
            upload: Upload::default(),
            fault,
            failed_digests: Vec::new(),
            resets: 0,
            awaiting_confirm: false,
        }
    }

    /// Accept an upload chunk. Returns the offset the client should continue
    /// from, and whether the digest matched once the image is complete.
    pub fn upload_chunk(
        &mut self,
        off: u64,
        len: Option<u64>,
        sha: Option<Vec<u8>>,
        data: &[u8],
    ) -> UploadOutcome {
        if off == 0 {
            // A restart, whether the client's idea or ours. `len` and `sha` are
            // mandatory here, which is what makes resumption possible at all.
            self.upload = Upload {
                expected_len: len.unwrap_or(0),
                declared_sha: sha,
                received: Vec::new(),
                chunks: 0,
                restart_demanded: self.upload.restart_demanded,
            };
        } else if off != self.upload.received.len() as u64 {
            // The client is out of step; tell it where we actually are. This is
            // the resumable-offset behaviour the spec requires.
            return UploadOutcome::Continue {
                off: self.upload.received.len() as u64,
            };
        }

        // Fault: demand a full restart once, at a chosen offset.
        if let Fault::RestartUpload { at_offset } = self.fault {
            if !self.upload.restart_demanded && off >= at_offset && off > 0 {
                self.upload.restart_demanded = true;
                self.upload.received.clear();
                return UploadOutcome::Continue { off: 0 };
            }
        }

        self.upload.received.extend_from_slice(data);
        self.upload.chunks += 1;

        // Fault: go silent partway through, as an unplugged cable does.
        if let Fault::DisconnectMidUpload { after_chunks } = self.fault {
            if self.upload.chunks >= after_chunks {
                return UploadOutcome::GoSilent;
            }
        }

        let complete = self.upload.expected_len > 0
            && self.upload.received.len() as u64 >= self.upload.expected_len;

        if !complete {
            return UploadOutcome::Continue {
                off: self.upload.received.len() as u64,
            };
        }

        // Complete: verify the digest the client declared up front.
        let mut h = Sha256::new();
        h.update(&self.upload.received);
        let actual = h.finalize().to_vec();

        let matches = match self.fault {
            Fault::BadHash => false,
            _ => self.upload.declared_sha.as_deref() == Some(actual.as_slice()),
        };

        if matches {
            let img = Image::from_bytes(&self.upload.received, "2.0.0+staged");
            self.slot1 = Some(img);
        }
        UploadOutcome::Complete {
            off: self.upload.received.len() as u64,
            matches,
        }
    }

    /// Mark an image by digest. `confirm=false` is test, `true` is permanent.
    pub fn set_state(&mut self, hash: Option<&[u8]>, confirm: bool) -> Result<(), String> {
        let Some(hash) = hash else {
            // No hash means "confirm the running image".
            if let Some(s0) = self.slot0.as_mut() {
                s0.confirmed = true;
                s0.permanent = true;
                self.awaiting_confirm = false;
                return Ok(());
            }
            return Err("no running image to confirm".into());
        };

        if self.fault == Fault::DigestAlreadyFailed && self.failed_digests.iter().any(|d| d == hash)
        {
            return Err("image with this digest already failed to boot".into());
        }

        // Confirming the image that is already running.
        if let Some(s0) = self.slot0.as_mut() {
            if s0.hash == hash {
                s0.confirmed = true;
                s0.permanent = true;
                self.awaiting_confirm = false;
                return Ok(());
            }
        }

        match self.slot1.as_mut() {
            Some(s1) if s1.hash == hash => {
                s1.pending = true;
                s1.permanent = confirm;
                Ok(())
            }
            _ => Err("no image with that digest in a slot".into()),
        }
    }

    /// Reset: run MCUboot's decision, then boot.
    pub fn reset(&mut self) {
        self.resets += 1;

        // An unconfirmed image that already booted once reverts now. This is
        // the case native_sim cannot reach, and the reason the mock exists.
        if self.awaiting_confirm {
            if let Some(bad) = self.slot0.take() {
                self.failed_digests.push(bad.hash.clone());
                // Swap the previous image back out of slot 1.
                self.slot0 = self.slot1.take();
                self.slot1 = Some(bad);
            }
            self.awaiting_confirm = false;
            return;
        }

        // A pending image swaps in.
        let pending = self.slot1.as_ref().map(|s| s.pending).unwrap_or(false);
        if pending {
            let mut incoming = self.slot1.take().expect("checked");
            incoming.pending = false;
            let outgoing = self.slot0.take();
            let permanent = incoming.permanent;
            incoming.confirmed = permanent;
            self.slot0 = Some(incoming);
            self.slot1 = outgoing;
            // A test-marked image must be confirmed before the next reset, or
            // it reverts. A permanent one is already settled.
            self.awaiting_confirm = !permanent;
            if self.fault == Fault::RevertOnBoot {
                self.awaiting_confirm = true;
            }
        }
    }

    /// The img group's state read.
    pub fn image_states(&self) -> Vec<SlotReport> {
        let mut out = Vec::new();
        if let Some(s) = &self.slot0 {
            out.push(SlotReport {
                slot: 0,
                active: true,
                img: s.clone(),
            });
        }
        if let Some(s) = &self.slot1 {
            out.push(SlotReport {
                slot: 1,
                active: false,
                img: s.clone(),
            });
        }
        out
    }
}

#[derive(Debug)]
pub struct SlotReport {
    pub slot: u32,
    pub active: bool,
    pub img: Image,
}

#[derive(Debug, PartialEq, Eq)]
pub enum UploadOutcome {
    /// Keep going from this offset.
    Continue { off: u64 },
    /// Image fully received; `matches` reports the digest check.
    Complete { off: u64, matches: bool },
    /// Stop answering entirely.
    GoSilent,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upload_whole(dev: &mut Device, image: &[u8]) -> UploadOutcome {
        let mut h = Sha256::new();
        h.update(image);
        let sha = h.finalize().to_vec();
        let mut off = 0u64;
        let mut last = UploadOutcome::Continue { off: 0 };
        while off < image.len() as u64 {
            let end = ((off + 128) as usize).min(image.len());
            let chunk = &image[off as usize..end];
            let (len, s) = if off == 0 {
                (Some(image.len() as u64), Some(sha.clone()))
            } else {
                (None, None)
            };
            last = dev.upload_chunk(off, len, s, chunk);
            match last {
                UploadOutcome::Continue { off: next } => off = next,
                UploadOutcome::Complete { .. } | UploadOutcome::GoSilent => break,
            }
        }
        last
    }

    #[test]
    fn a_clean_upload_lands_in_slot_one_with_a_matching_digest() {
        let mut dev = Device::provisioned(Fault::None);
        let out = upload_whole(&mut dev, b"new-firmware-payload");
        assert!(
            matches!(out, UploadOutcome::Complete { matches: true, .. }),
            "{out:?}"
        );
        assert!(dev.slot1.is_some());
        // Uploading must not disturb the running image.
        assert!(dev.slot0.as_ref().unwrap().confirmed);
    }

    #[test]
    fn test_then_confirm_sticks() {
        let mut dev = Device::provisioned(Fault::None);
        upload_whole(&mut dev, b"new-firmware-payload");
        let staged = dev.slot1.as_ref().unwrap().hash.clone();

        dev.set_state(Some(&staged), false).unwrap();
        assert!(
            dev.slot1.as_ref().unwrap().pending,
            "test mark should set pending"
        );
        assert!(!dev.slot1.as_ref().unwrap().permanent);

        dev.reset();
        assert_eq!(
            dev.slot0.as_ref().unwrap().hash,
            staged,
            "new image should be running"
        );
        assert!(
            !dev.slot0.as_ref().unwrap().confirmed,
            "must not self-confirm"
        );
        assert!(dev.awaiting_confirm, "an unconfirmed image is on probation");

        // The runtime confirms only after the new image heartbeats.
        dev.set_state(Some(&staged), true).unwrap();
        assert!(dev.slot0.as_ref().unwrap().confirmed);

        dev.reset();
        assert_eq!(
            dev.slot0.as_ref().unwrap().hash,
            staged,
            "confirmed image must survive"
        );
    }

    #[test]
    fn an_unconfirmed_image_reverts_on_the_next_reset() {
        let mut dev = Device::provisioned(Fault::None);
        let factory = dev.slot0.as_ref().unwrap().hash.clone();
        upload_whole(&mut dev, b"broken-firmware");
        let staged = dev.slot1.as_ref().unwrap().hash.clone();

        dev.set_state(Some(&staged), false).unwrap();
        dev.reset();
        assert_eq!(dev.slot0.as_ref().unwrap().hash, staged);

        // No confirm arrives — the image never spoke SMP. Next reset reverts.
        dev.reset();
        assert_eq!(
            dev.slot0.as_ref().unwrap().hash,
            factory,
            "must roll back to factory"
        );
        assert!(
            dev.failed_digests.contains(&staged),
            "the bad digest must be remembered"
        );
    }

    #[test]
    fn confirm_marked_upfront_does_not_revert() {
        let mut dev = Device::provisioned(Fault::None);
        upload_whole(&mut dev, b"trusted-firmware");
        let staged = dev.slot1.as_ref().unwrap().hash.clone();

        dev.set_state(Some(&staged), true).unwrap();
        dev.reset();
        assert!(dev.slot0.as_ref().unwrap().confirmed);
        assert!(!dev.awaiting_confirm);
        dev.reset();
        assert_eq!(dev.slot0.as_ref().unwrap().hash, staged);
    }

    #[test]
    fn fault_bad_hash_reports_no_match_and_stages_nothing() {
        let mut dev = Device::provisioned(Fault::BadHash);
        let out = upload_whole(&mut dev, b"payload-the-device-hashes-differently");
        assert!(
            matches!(out, UploadOutcome::Complete { matches: false, .. }),
            "{out:?}"
        );
        assert!(
            dev.slot1.is_none(),
            "a digest mismatch must not stage an image"
        );
    }

    #[test]
    fn fault_disconnect_mid_upload_goes_silent() {
        let mut dev = Device::provisioned(Fault::DisconnectMidUpload { after_chunks: 2 });
        let out = upload_whole(&mut dev, &vec![0xAB; 4096]);
        assert_eq!(out, UploadOutcome::GoSilent);
        assert!(dev.slot1.is_none());
    }

    #[test]
    fn fault_restart_upload_demands_offset_zero_once_then_succeeds() {
        let mut dev = Device::provisioned(Fault::RestartUpload { at_offset: 256 });
        let image = vec![0x5A; 2048];
        let out = upload_whole(&mut dev, &image);
        // The helper honours the off:0 demand and re-sends len and sha, so the
        // transfer must still complete.
        assert!(
            matches!(out, UploadOutcome::Complete { matches: true, .. }),
            "{out:?}"
        );
        assert!(
            dev.upload.restart_demanded,
            "the restart should have been exercised"
        );
    }

    #[test]
    fn fault_digest_already_failed_refuses_a_remark() {
        let mut dev = Device::provisioned(Fault::DigestAlreadyFailed);
        upload_whole(&mut dev, b"repeatedly-broken");
        let staged = dev.slot1.as_ref().unwrap().hash.clone();
        dev.failed_digests.push(staged.clone());

        let err = dev.set_state(Some(&staged), false).unwrap_err();
        assert!(err.contains("already failed"), "got: {err}");
    }

    #[test]
    fn an_out_of_step_client_is_told_the_real_offset() {
        let mut dev = Device::provisioned(Fault::None);
        let image = vec![1u8; 1024];
        let mut h = Sha256::new();
        h.update(&image);
        dev.upload_chunk(0, Some(1024), Some(h.finalize().to_vec()), &image[..128]);
        // Claim to be much further along than we are.
        let out = dev.upload_chunk(900, None, None, &image[..64]);
        assert_eq!(
            out,
            UploadOutcome::Continue { off: 128 },
            "server offset is authoritative"
        );
    }
}
