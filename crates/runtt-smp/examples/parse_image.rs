//! Inspect an MCUboot image the way the runtime does.
//!
//!     cargo run -p runtt-smp --example parse_image -- path/to/zephyr.signed.bin
//!
//! Prints the identity the DEVICE will report -- the MCUboot digest from the
//! image's TLV area -- alongside the SHA-256 of the file, because confusing the
//! two is the mistake that yields IMG_MGMT_ERR_HASH_NOT_FOUND.

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: parse_image <image.signed.bin>");
        std::process::exit(2);
    };
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("cannot read {path}: {e}");
            std::process::exit(1);
        }
    };

    let hex = |b: &[u8]| -> String { b.iter().map(|x| format!("{x:02x}")).collect() };

    match runtt_smp::mcuboot::parse(&bytes) {
        Ok(i) => {
            println!("{path}: {} bytes", bytes.len());
            println!("  version           {}", i.version);
            println!("  header size       {}", i.header_size);
            println!("  image size        {}", i.image_size);
            println!("  MCUboot digest    {}   <- image identity", hex(&i.digest));
            println!(
                "  file SHA-256      {}   <- transfer integrity only",
                hex(&runtt_smp::ToolkitClient::digest(&bytes))
            );
        }
        Err(e) => {
            eprintln!("not a usable MCUboot image: {e:#}");
            std::process::exit(1);
        }
    }
}
