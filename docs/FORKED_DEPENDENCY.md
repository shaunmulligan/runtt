# We build against an unreleased `mcumgr-toolkit` commit

**Status: merged upstream, not yet released.**
[Finomnis/mcumgr-toolkit#186](https://github.com/Finomnis/mcumgr-toolkit/pull/186)
landed as `40713d8` on 2026-09-27. The newest release, 0.17.1 (2026-09-26),
predates it.

```toml
[patch.crates-io]
mcumgr-toolkit = { git = "https://github.com/Finomnis/mcumgr-toolkit", rev = "40713d8" }
```

## Why

`Transport` is a public trait and implementing it is the documented way to add a
bearer, but up to 0.17.1 that cannot be done from outside the crate. Two
independent blockers:

* Its required methods name `SMP_HEADER_SIZE` and `SMP_TRANSFER_BUFFER_SIZE`,
  both private, so an external implementor cannot name its own parameter types
  (`E0603`).
* `MCUmgrClient`'s `connection` field is private and every constructor is tied to
  a concrete transport, so even a working impl could not be handed to a client.
  `Connection::new` is already public and generic over `Transport` — the
  capability exists one level down, just unexposed.

Our SMP-over-ISO-TP transport for CAN needs both. #186 makes the constants
public and adds a generic `MCUmgrClient::new_from_transport`; it is purely
additive, and cannot express anything the crate does not already do internally.

## Two consequences to know about

**`cargo publish` is impossible while this exists.** crates.io requires every
dependency to itself be on crates.io, so a git dependency closes publishing
outright. Taking the release is therefore the last step before any release to
crates.io — not an optional tidy-up.

**We pin a `rev`, not a branch.** Upstream `main` moves. Pinning the commit means
a push there cannot silently change what we build; taking new work is a
deliberate edit here.

Upstream asks for Rust ≥ 1.88 (`resolver = "3"`, `edition = "2024"`,
`rust-version = "1.88"`). An older toolchain fails at *manifest parse* with
`` `resolver` setting `3` is not valid ``, which looks unrelated and is not.

## When a release contains it

1. Bump the `mcumgr-toolkit` version in the workspace `Cargo.toml` to that
   release.
2. **Delete the `[patch.crates-io]` block entirely.**
3. `cargo update -p mcumgr-toolkit`, then `cargo test --workspace` and the
   `native_sim` gates.
4. Update the version named in `docs/WIRE_CONTRACT.md`.
5. Delete this file, and the references to it in `README.md` and `NOTES.md`.

Nothing else in the tree depends on the git source — the pin exists solely for
this one addition. `crates/runtt-smp/src/toolkit.rs` is the only consumer, via
`ToolkitClient::from_transport`.
