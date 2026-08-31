//! The contract version must agree in three places, or it means nothing.
//!
//! `docs/WIRE_CONTRACT.md` is the interface between two independently versioned
//! parties. If the document, the firmware default and the runtime's accepted
//! major can drift apart silently, the version is decoration rather than a
//! guarantee — so pin them together here.

use std::path::Path;

fn repo_root() -> &'static Path {
    // CARGO_MANIFEST_DIR is crates/runtt.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repo root")
}

fn read(rel: &str) -> String {
    let p = repo_root().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

/// `**Version 1.0.0**` from the document's header.
fn documented_version() -> String {
    let doc = read("docs/WIRE_CONTRACT.md");
    doc.lines()
        .find_map(|l| l.strip_prefix("**Version ")?.strip_suffix("**"))
        .expect("WIRE_CONTRACT.md should state **Version x.y.z** near the top")
        .trim()
        .to_string()
}

/// The default the firmware reports over `describe`.
fn firmware_default_version() -> String {
    let kconfig = read("firmware/runtt/zephyr/Kconfig");
    let mut lines = kconfig.lines();
    while let Some(l) = lines.next() {
        if l.trim() == "config RUNTT_CONTRACT_VERSION" {
            for l in lines.by_ref().take(6) {
                if let Some(v) = l.trim().strip_prefix("default ") {
                    return v.trim().trim_matches('"').to_string();
                }
            }
        }
    }
    panic!("no default found for RUNTT_CONTRACT_VERSION");
}

/// The major the runtime is willing to talk to.
fn runtime_major() -> u32 {
    let src = read("crates/runtt/src/flash.rs");
    src.lines()
        .find_map(|l| {
            let l = l.trim();
            let rest = l.strip_prefix("const CONTRACT_MAJOR: u32 = ")?;
            rest.trim_end_matches(';').trim().parse().ok()
        })
        .expect("CONTRACT_MAJOR should be declared in flash.rs")
}

#[test]
fn the_document_and_the_firmware_agree() {
    assert_eq!(
        documented_version(),
        firmware_default_version(),
        "docs/WIRE_CONTRACT.md and RUNTT_CONTRACT_VERSION disagree. \
         Whichever you changed, change the other."
    );
}

#[test]
fn the_runtime_accepts_the_documented_major() {
    let doc = documented_version();
    let major: u32 = doc
        .split('.')
        .next()
        .and_then(|m| m.parse().ok())
        .unwrap_or_else(|| panic!("documented version {doc:?} is not semver-ish"));
    assert_eq!(
        major,
        runtime_major(),
        "the runtime implements contract major {} but the document describes {doc}. \
         A runtime that refuses the contract it ships with is worse than no check.",
        runtime_major()
    );
}

#[test]
fn the_document_does_not_mention_a_stale_version() {
    // The header is not the only place the version appears: it is also in the
    // Kconfig table and the describe field description. Bumping only the header
    // leaves those quietly wrong, which is worse than not stating them.
    let doc = read("docs/WIRE_CONTRACT.md");
    let current = documented_version();
    let history_marker = "## 12. Version history";
    let body = doc
        .split(history_marker)
        .next()
        .expect("version history section");

    for (i, line) in body.lines().enumerate() {
        // Look for anything that looks like a semver in backticks or quotes.
        for cand in ["1.0.0", "1.1.0", "1.2.0", "2.0.0"] {
            if line.contains(cand) && cand != current {
                panic!(
                    "docs/WIRE_CONTRACT.md line {} mentions version {cand} but the \
                     contract is {current}. Version history is exempt; the body is not.\n  {line}",
                    i + 1
                );
            }
        }
    }
}

#[test]
fn the_mock_declares_the_documented_contract() {
    // The mock stands in for a device in every integration test. If it advertises
    // a contract the runtime would refuse -- or worse, one it would wrongly
    // accept -- those tests stop meaning anything.
    //
    // The version is the mock's DEFAULT rather than a literal in the describe
    // payload, because with_contract() can override it to exercise the host's
    // major-version handling. What matters is that a mock nobody has configured
    // speaks the documented contract.
    let server = read("crates/runtt-mock/src/server.rs");
    let want = format!("contract: \"{}\".to_string()", documented_version());
    assert!(
        server.contains(&want),
        "runtt-mock should DEFAULT to contract {} over describe, like the document \
         says; found no `{}` in its initialiser",
        documented_version(),
        want
    );
}

#[test]
fn the_describe_group_is_the_per_user_base() {
    // 64 is MGMT_GROUP_ID_PERUSER. Below it the ids belong to Zephyr, so a
    // command there would be squatting on someone else's number.
    let kconfig = read("firmware/runtt/zephyr/Kconfig");
    assert!(
        kconfig.contains("default 64"),
        "RUNTT_SMP_GROUP_ID should default to 64 (MGMT_GROUP_ID_PERUSER)"
    );
    assert_eq!(runtt_smp::describe::GROUP_PERUSER, 64);
}

#[test]
fn the_documented_interface_strings_are_the_ones_we_match_on() {
    let doc = read("docs/WIRE_CONTRACT.md");
    for s in [runtt_transport::usb::IFACE_MGMT, runtt_transport::usb::IFACE_LOG] {
        assert!(doc.contains(s), "WIRE_CONTRACT.md should document {s}");
        // And the udev rules must key off the same strings, or a device that
        // honours the contract still will not be discovered.
        let rules = read("udev/90-runtt.rules");
        assert!(rules.contains(s), "udev rules should match on {s}");
        // As should the hardware overlay that produces them.
        let overlay = read("firmware/runtt/snippets/runtt/boards/rpi_pico.overlay");
        assert!(
            overlay.contains(s),
            "the rpi_pico overlay should declare {s}"
        );
    }
}
