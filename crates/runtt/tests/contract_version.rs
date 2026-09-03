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

/// Read something that lives on the FIRMWARE side of the contract.
///
/// The firmware module and the runtime are heading for separate repositories, so
/// these files are present in the monorepo and absent once the runtime stands
/// alone. Returning None lets the same test file work in both layouts.
///
/// A skip is not silent: it prints, and `RUNTT_REQUIRE_FIRMWARE=1` turns it into
/// a failure. CI sets that once it fetches firmware fixtures, so the skip cannot
/// quietly become permanent -- an assertion nobody notices has stopped running is
/// worse than one that was never written.
fn read_firmware(rel: &str) -> Option<String> {
    let p = repo_root().join(rel);
    match std::fs::read_to_string(&p) {
        Ok(s) => Some(s),
        Err(_) if std::env::var_os("RUNTT_REQUIRE_FIRMWARE").is_none() => {
            eprintln!(
                "SKIP: {} is not in this repository. The firmware module lives in \
                 runtt-zephyr, which asserts the contract version on its own side. \
                 Set RUNTT_REQUIRE_FIRMWARE=1 to make this a failure.",
                rel
            );
            None
        }
        Err(e) => panic!(
            "RUNTT_REQUIRE_FIRMWARE is set but {} cannot be read: {e}",
            p.display()
        ),
    }
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
fn firmware_default_version() -> Option<String> {
    let kconfig = read_firmware("firmware/runtt/zephyr/Kconfig")?;
    let mut lines = kconfig.lines();
    while let Some(l) = lines.next() {
        if l.trim() == "config RUNTT_CONTRACT_VERSION" {
            for l in lines.by_ref().take(6) {
                if let Some(v) = l.trim().strip_prefix("default ") {
                    return Some(v.trim().trim_matches('"').to_string());
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
    let Some(fw) = firmware_default_version() else {
        return;
    };
    assert_eq!(
        documented_version(),
        fw,
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

/// Every doc, not just the contract.
///
/// A stale `contract 1.2.0` sat in three transcripts across WALKTHROUGH, ROADMAP
/// and MANUAL_LOG_DEMUX after the 2.0.0 bump, because the guard below only ever
/// read WIRE_CONTRACT.md. A transcript claiming a version the software does not
/// report is a small lie that makes a reader doubt the rest of the document, so
/// it is worth a test.
///
/// Only the phrase "contract <semver>" is matched, which is what the runtime and
/// `describe` actually print -- narrow enough not to trip over prose about
/// unrelated version numbers.
#[test]
fn no_doc_quotes_a_stale_contract_version() {
    let current = documented_version();
    let re_like = format!("contract {current}");
    let dir = repo_root().join("docs");
    let mut stale = Vec::new();

    // NOTES.md is included explicitly. The by-hand procedures and transcripts
    // that this guard was written for now live there rather than under docs/,
    // and a guard that quietly stops covering the files it was aimed at is
    // worse than one that was never written.
    let mut paths: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
        .expect("docs/ should exist")
        .map(|e| e.expect("dir entry").path())
        .collect();
    paths.push(repo_root().join("NOTES.md"));

    for path in paths {
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        // The contract's own version history is a record of the past, by design.
        let body = std::fs::read_to_string(&path).expect("readable");
        let body = if name == "WIRE_CONTRACT.md" {
            body.split("## 12. Version history")
                .next()
                .unwrap()
                .to_string()
        } else {
            body
        };

        for (i, line) in body.lines().enumerate() {
            for cap in line.split("contract ").skip(1) {
                let ver: String = cap
                    .chars()
                    .take_while(|c| c.is_ascii_digit() || *c == '.')
                    .collect();
                // Three dotted numbers, and not the current one.
                if ver.split('.').count() == 3
                    && ver.split('.').all(|p| !p.is_empty())
                    && ver != current
                {
                    stale.push(format!("{name}:{}: contract {ver}\n    {line}", i + 1));
                }
            }
        }
        let _ = &re_like;
    }

    assert!(
        stale.is_empty(),
        "these docs quote a contract version other than {current}:\n  {}",
        stale.join("\n  ")
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
    let Some(kconfig) = read_firmware("firmware/runtt/zephyr/Kconfig") else {
        return;
    };
    assert!(
        kconfig.contains("default 64"),
        "RUNTT_SMP_GROUP_ID should default to 64 (MGMT_GROUP_ID_PERUSER)"
    );
    assert_eq!(runtt_smp::describe::GROUP_PERUSER, 64);
}

#[test]
fn the_documented_interface_strings_are_the_ones_we_match_on() {
    let doc = read("docs/WIRE_CONTRACT.md");
    for s in [
        runtt_transport::usb::IFACE_MGMT,
        runtt_transport::usb::IFACE_LOG,
    ] {
        assert!(doc.contains(s), "WIRE_CONTRACT.md should document {s}");
        // And the udev rules must key off the same strings, or a device that
        // honours the contract still will not be discovered.
        let rules = read("udev/90-runtt.rules");
        assert!(rules.contains(s), "udev rules should match on {s}");
        // As should the hardware overlay that produces them -- when the firmware
        // module is in this repository. runtt-zephyr asserts it on its own side.
        if let Some(overlay) =
            read_firmware("firmware/runtt/snippets/runtt/boards/rpi_pico.overlay")
        {
            assert!(
                overlay.contains(s),
                "the rpi_pico overlay should declare {s}"
            );
        }
    }
}
