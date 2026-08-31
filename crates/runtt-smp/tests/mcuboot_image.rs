//! Checks our MCUboot parser against a real imgtool-signed image.
//!
//! The image and imgtool's own reported digest are committed as fixtures, so
//! this runs in CI without needing a Zephyr toolchain.

use runtt_smp::mcuboot;

const SIGNED: &[u8] = include_bytes!("fixtures/app.signed.bin");
/// What `imgtool verify` reports for the fixture.
const IMGTOOL_DIGEST: &str = include_str!("fixtures/app.signed.digest");

#[test]
fn our_digest_matches_imgtool() {
    let info = mcuboot::parse(SIGNED).expect("fixture should parse");
    let ours: String = info.digest.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(
        ours,
        IMGTOOL_DIGEST.trim(),
        "our TLV walk must agree with imgtool; this is the digest the device \
         reports in image list and expects in set_state"
    );
}

#[test]
fn reads_the_version_imgtool_signed_with() {
    let info = mcuboot::parse(SIGNED).unwrap();
    assert_eq!(info.version, "2.0.0+0");
}

#[test]
fn the_file_hash_and_the_image_digest_are_different() {
    // The distinction that caused IMG_MGMT_ERR_HASH_NOT_FOUND: the upload's
    // `sha` field is over the file bytes, but image identity is the MCUboot
    // digest over header+body only.
    let info = mcuboot::parse(SIGNED).unwrap();
    let file_hash = runtt_smp::ToolkitClient::digest(SIGNED);
    assert_ne!(
        info.digest, file_hash,
        "if these ever coincide the test is not proving anything"
    );
}
