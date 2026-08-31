# Upstreaming: let `Transport` be implemented outside `mcumgr-toolkit`

Two small changes to [`mcumgr-toolkit`](https://crates.io/crates/mcumgr-toolkit)
0.16.0, carried locally until they land upstream. **This is for you to submit** —
patch, reasoning and a draft PR description are below.

Patch: `docs/patches/mcumgr-toolkit-external-transports.patch` — 86 lines against
0.16.0 as published, touching `src/client.rs` and `src/transport/mod.rs` and
nothing else. It carries a commit message and applies with `patch -p1` inside an
unpacked 0.16.0. Verified by applying it to the pristine crates.io source and
confirming the result is byte-identical to the vendored tree.

Vendored tree: `third_party/mcumgr-toolkit/` (0.16.0 as published, plus one commit)

To regenerate after changing the vendored tree:

```bash
PRISTINE=~/.cargo/registry/src/index.crates.io-*/mcumgr-toolkit-0.16.0
diff -ruN "$PRISTINE" third_party/mcumgr-toolkit   # then re-add the message header
```

---

## The problem

`Transport` is a public trait, and implementing it is the documented way to add
a bearer. In 0.16.0 you can do neither half of that from outside the crate.

**1. The trait cannot be implemented.** Its required methods name
`SMP_HEADER_SIZE` and `SMP_TRANSFER_BUFFER_SIZE`, and both are private:

```rust
const SMP_HEADER_SIZE: usize = 8;
const SMP_TRANSFER_BUFFER_SIZE: usize = u16::MAX as usize;
```

```rust
fn send_raw_frame(&mut self, header: [u8; SMP_HEADER_SIZE], data: &[u8]) -> …;
fn recv_raw_frame<'a>(&mut self, buffer: &'a mut [u8; SMP_TRANSFER_BUFFER_SIZE]) -> …;
```

An external implementor cannot name the parameter types. `rustc` reports
`error[E0603]: constant SMP_HEADER_SIZE is private`.

**2. A working implementation could not be used anyway.** `MCUmgrClient`'s
`connection` field is private and all four constructors — `new_from_serial`,
`new_from_usb_serial`, `new_from_ble`, `new_from_udp` — are tied to a concrete
transport type. `Connection::new` is already public and generic over
`Transport`, so the capability exists one level down; it is simply not exposed.

Together these make the public trait unreachable.

## The change

Make the two constants `pub`, with doc comments saying why, and add:

```rust
pub fn new_from_transport<T: Transport + Send + 'static>(transport: T) -> Self {
    Self {
        connection: Connection::new(transport),
        smp_frame_size: ZEPHYR_DEFAULT_SMP_FRAME_SIZE.into(),
    }
}
```

Both are additive. No signature changes, no behaviour changes, nothing removed.
`Connection::new` already accepts exactly this bound, so the new constructor
cannot express anything the crate does not already do internally.

## The motivating case

SMP over **ISO-TP** (ISO 15765-2) on a CAN bus, for a project that ships MCU
firmware as OCI container images and wants a management channel over CAN
alongside USB.

ISO-TP is datagram-oriented and handles its own segmentation, so the transport
carries raw SMP frames and comes out close to a copy of `UdpTransport` — about
ninety lines. Both Linux (`can-isotp`, mainline since 5.10) and Zephyr
(`subsys/canbus/isotp`) implement ISO-TP, so no framing has to be invented.

It generalises past CAN: any bearer that can carry a raw SMP frame — a test
double, a custom radio, a socket to a simulator — hits the same wall.

## Draft PR description

> **Allow `Transport` to be implemented outside the crate**
>
> `Transport` is public and implementing it is the documented way to add a
> bearer, but in 0.16.0 that is not possible from another crate:
>
> * its required methods name `SMP_HEADER_SIZE` and `SMP_TRANSFER_BUFFER_SIZE`,
>   which are private, so an external implementor cannot name the parameter
>   types (`E0603`);
> * and `MCUmgrClient` can only be built from one of the four built-in
>   transports, since `connection` is private — so even a working impl could not
>   be handed to a client.
>
> This makes the two constants public and adds a generic
> `MCUmgrClient::new_from_transport`, mirroring the already-public generic
> `Connection::new`. Purely additive.
>
> My use case is SMP over ISO-TP on CAN: ISO-TP is datagram-oriented and does
> its own segmentation, so the transport is close to a copy of `UdpTransport`.
> The same applies to any custom bearer, including test doubles.

## Submitting it

Upstream is **[github.com/Finomnis/mcumgr-toolkit](https://github.com/Finomnis/mcumgr-toolkit)**,
by Finomnis, dual MIT/Apache-2.0. There is no CONTRIBUTING file, so this is the
ordinary GitHub fork-and-PR flow.

```bash
# 1. Fork on GitHub (the web UI, or `gh repo fork` below), then clone YOUR fork.
gh repo fork Finomnis/mcumgr-toolkit --clone --remote
cd mcumgr-toolkit
git checkout -b external-transports

# 2. Apply the source changes. See the caveat below before applying Cargo.toml.
patch -p1 --dry-run < /path/to/runtt/docs/patches/mcumgr-toolkit-external-transports.patch
patch -p1           < /path/to/runtt/docs/patches/mcumgr-toolkit-external-transports.patch

# 3. Prove it.
cargo test
cargo clippy --all-targets
cargo fmt --check

# 4. Commit and open the PR. The patch file's header is written to be the
#    commit message, so this keeps it.
git add -A
git commit -F <(sed -n '1,/^---$/p' /path/to/runtt/docs/patches/mcumgr-toolkit-external-transports.patch | head -n -1)
git push -u origin external-transports
gh pr create --repo Finomnis/mcumgr-toolkit --fill
```

### The one caveat: the patch is against the *published crate*

It was generated by diffing `third_party/mcumgr-toolkit/` against 0.16.0 as
published on crates.io, because that is what we vendored. **The published
`Cargo.toml` is normalised by `cargo package`** — dependencies rewritten into
`[dependencies.foo]` tables, and test targets listed explicitly because
autodiscovery is switched off in the packaged form.

Consequences when applying to the git repo:

* The `src/` and `tests/` hunks apply cleanly; they are the real change.
* **The `Cargo.toml` hunk probably will not apply, and probably is not needed.**
  It adds a `[[test]]` stanza registering the new test file, which the published
  manifest requires and a normal repo does not — autodiscovery picks up anything
  in `tests/`. If `patch` rejects that hunk, check whether the repo's manifest
  lists its other test targets explicitly. If it does, add ours the same way; if
  it does not, drop the hunk.

If that is fiddly, apply the two `src/` edits by hand — they are about twenty
lines between them — and copy `tests/external_transport_tests.rs` across whole.

### What the maintainer will check

* **It is additive.** No signature changes, nothing removed. `Connection::new` is
  already public and generic over `Transport` with the same bound, so
  `new_from_transport` cannot express anything the crate does not already do.
* **It is tested, and tested in the only way that works.**
  `tests/external_transport_tests.rs` implements `Transport` over a datagram echo
  bearer and drives a real `os_echo` round-trip through it. Integration tests are
  separate crates, so it sees what a third party sees: if either constant went
  private again, or the constructor were removed, it would fail to build. An
  in-crate unit test could check neither, because it can name private items.
* **The existing suite is unaffected.** 94 tests pass with the patch applied
  (84 unit + 2 external + 1 + 2 + 5 across the other targets).

## When it lands

1. Bump the dependency to the release containing it.
2. Delete the `[patch.crates-io]` block from the workspace `Cargo.toml`.
3. Delete `third_party/mcumgr-toolkit/`.
4. Delete this file and the patch.

Nothing else references the vendored tree — the pin exists solely for this.

---

*Co-authored with Claude*
