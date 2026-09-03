# We build against a fork of `mcumgr-toolkit`

**Status: awaiting review.** [Finomnis/mcumgr-toolkit#186](https://github.com/Finomnis/mcumgr-toolkit/pull/186)

```toml
[patch.crates-io]
mcumgr-toolkit = { git = "https://github.com/shaunmulligan/mcumgr-toolkit", rev = "6595f7e" }
```

## Why

`Transport` is a public trait and implementing it is the documented way to add a
bearer, but in 0.16.0 that cannot be done from outside the crate. Two independent
blockers:

* Its required methods name `SMP_HEADER_SIZE` and `SMP_TRANSFER_BUFFER_SIZE`,
  both private, so an external implementor cannot name its own parameter types
  (`E0603`).
* `MCUmgrClient`'s `connection` field is private and every constructor is tied to
  a concrete transport, so even a working impl could not be handed to a client.
  `Connection::new` is already public and generic over `Transport` — the
  capability exists one level down, just unexposed.

Our SMP-over-ISO-TP transport for CAN needs both. The patch makes the constants
public and adds a generic `MCUmgrClient::new_from_transport`; it is purely
additive, and cannot express anything the crate does not already do internally.

## Two consequences to know about

**`cargo publish` is impossible while this exists.** crates.io requires every
dependency to itself be on crates.io, so a git dependency closes publishing
outright. Dropping the fork is therefore the last step before any release to
crates.io — not an optional tidy-up.

**We pin a `rev`, not a branch.** The branch will move as review feedback lands.
Pinning the commit means a rebase or amend upstream cannot silently change what
we build; taking new work is a deliberate edit here.

## Taking review feedback

Changes to the patch happen **in the fork**, not here — there is no vendored copy
any more. After pushing to `external-transports`:

```bash
# Get the new head, then point at it.
git ls-remote https://github.com/shaunmulligan/mcumgr-toolkit external-transports
# edit the rev in Cargo.toml, then:
cargo update -p mcumgr-toolkit
cargo test --workspace && ./scripts/native-sim-e2e.sh
```

Upstream asks for Rust ≥ 1.88 (`resolver = "3"`, `edition = "2024"`,
`rust-version = "1.88"`). An older toolchain fails at *manifest parse* with
`` `resolver` setting `3` is not valid ``, which looks unrelated to the patch and
is not.

Upstream's own gates, worth running in the fork before pushing:

```bash
cargo test -p mcumgr-toolkit
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

Note the repo is a **workspace** — the crate is in `mcumgr-toolkit/`, alongside
`mcumgr-toolkit-python/`, `mcumgrctl/` and `target_tests/`. Run those from the
repository root.

## When the PR merges

1. Wait for a **release** containing it. A merge is not enough: `[patch.crates-io]`
   would still point at a git source, so publishing stays closed.
2. Bump the `mcumgr-toolkit` version in the workspace `Cargo.toml` to that release.
3. **Delete the `[patch.crates-io]` block entirely.**
4. `cargo update -p mcumgr-toolkit`, then `cargo test --workspace` and the
   `native_sim` gates.
5. Delete this file, and the reference to it in `README.md` and `NOTES.md`.

Nothing else in the tree depends on the fork — the pin exists solely for this one
addition. `crates/runtt-smp/src/toolkit.rs` is the only consumer, via
`ToolkitClient::from_transport`.

## If the PR is rejected

The fallback is to keep the transport but stop needing the constructor: wrap
`Connection` ourselves, or move `runtt-smp` off `mcumgr-toolkit` entirely — it is
already a five-method trait precisely so that swapping the dependency is a
one-file change. That was the reason for the trait boundary, and this is the
scenario it was for.
