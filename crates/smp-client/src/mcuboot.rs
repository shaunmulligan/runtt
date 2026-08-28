//! Minimal MCUboot image parsing.
//!
//! There are **two different hashes** in play, and conflating them produces a
//! confusing `IMG_MGMT_ERR_HASH_NOT_FOUND` from the device:
//!
//! * the SHA-256 of the **image file bytes** — what the img group's `sha`
//!   upload field wants, for transfer integrity and resumption;
//! * the **MCUboot image digest** — SHA-256 over the header and body only,
//!   stored in the image's TLV area. This is the identity the device uses in
//!   `image list` and expects in `set_state`.
//!
//! We need the second one locally to answer "does the device already run this
//! image?" without uploading it first.

use anyhow::{bail, Context, Result};

/// `IMAGE_MAGIC` from `bootutil/image.h`, little-endian on the wire.
const IMAGE_MAGIC: u32 = 0x96f3_b83d;
/// `IMAGE_TLV_INFO_MAGIC` — the unprotected TLV area.
const TLV_INFO_MAGIC: u16 = 0x6907;
/// `IMAGE_TLV_PROT_INFO_MAGIC` — the protected TLV area, which precedes it.
const TLV_PROT_INFO_MAGIC: u16 = 0x6908;
/// `IMAGE_TLV_SHA256`: SHA-256 of the image header and body.
const TLV_SHA256: u8 = 0x10;

const HEADER_LEN: usize = 32;

/// What we need out of an image, and no more.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageInfo {
    /// The MCUboot image digest — the identity the device reports and expects.
    pub digest: [u8; 32],
    pub version: String,
    pub header_size: u16,
    pub image_size: u32,
}

fn u16le(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32le(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// Parse an MCUboot image, extracting its digest and version.
pub fn parse(image: &[u8]) -> Result<ImageInfo> {
    if image.len() < HEADER_LEN {
        bail!("not an MCUboot image: only {} bytes", image.len());
    }
    let magic = u32le(image, 0);
    if magic != IMAGE_MAGIC {
        bail!(
            "not an MCUboot image: header magic is {magic:#010x}, expected {IMAGE_MAGIC:#010x}. \
             Was the binary signed with imgtool?"
        );
    }

    let header_size = u16le(image, 8);
    let protect_tlv_size = u16le(image, 10);
    let image_size = u32le(image, 12);

    // struct image_version { major u8, minor u8, revision u16, build_num u32 }
    let version = format!(
        "{}.{}.{}+{}",
        image[20],
        image[21],
        u16le(image, 22),
        u32le(image, 24)
    );

    // The TLV areas sit immediately after the header and body: the protected
    // area first (if present), then the unprotected one.
    let body_end = header_size as usize + image_size as usize;
    let mut cursor = body_end
        .checked_add(protect_tlv_size as usize)
        .context("image header sizes overflow")?;
    if protect_tlv_size == 0 {
        cursor = body_end;
    }

    let digest = find_sha256(image, cursor)
        .or_else(|| {
            // Some images place only the protected area where we looked; try
            // the unprotected one directly after the body.
            if protect_tlv_size != 0 {
                find_sha256(image, body_end)
            } else {
                None
            }
        })
        .context(
            "MCUboot image has no SHA256 TLV; cannot determine the digest the \
             device will report",
        )?;

    Ok(ImageInfo {
        digest,
        version,
        header_size,
        image_size,
    })
}

/// Walk a TLV area looking for the SHA-256 entry.
fn find_sha256(image: &[u8], start: usize) -> Option<[u8; 32]> {
    if start + 4 > image.len() {
        return None;
    }
    let magic = u16le(image, start);
    if magic != TLV_INFO_MAGIC && magic != TLV_PROT_INFO_MAGIC {
        return None;
    }
    let total = u16le(image, start + 2) as usize;
    let area_end = (start + total).min(image.len());

    let mut at = start + 4;
    while at + 4 <= area_end {
        let kind = image[at];
        let len = u16le(image, at + 2) as usize;
        let data_at = at + 4;
        if data_at + len > image.len() {
            return None;
        }
        if kind == TLV_SHA256 && len == 32 {
            let mut out = [0u8; 32];
            out.copy_from_slice(&image[data_at..data_at + 32]);
            return Some(out);
        }
        at = data_at + len;
    }

    // The protected area is followed by the unprotected one; keep looking.
    if magic == TLV_PROT_INFO_MAGIC {
        return find_sha256(image, area_end);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_an_unsigned_binary_with_a_useful_message() {
        let err = parse(&[0u8; 512]).unwrap_err().to_string();
        assert!(err.contains("header magic"), "got: {err}");
        assert!(err.contains("imgtool"), "should hint at the cause: {err}");
    }

    #[test]
    fn rejects_something_far_too_short() {
        assert!(parse(b"nope")
            .unwrap_err()
            .to_string()
            .contains("only 4 bytes"));
    }
}
