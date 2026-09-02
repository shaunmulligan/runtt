# Upstreaming: let `Transport` be implemented outside `mcumgr-toolkit`

Two small changes to [`mcumgr-toolkit`](https://crates.io/crates/mcumgr-toolkit)
0.16.0, carried locally until they land upstream. **This is for you to submit** —
patch, reasoning and a draft PR description are below.

Patch: `docs/patches/mcumgr-toolkit-external-transports.patch`

A `git format-patch` file carrying its own commit message, against
**`Finomnis/mcumgr-toolkit` `main`**. Touches three files and nothing else:

```
mcumgr-toolkit/src/client.rs
mcumgr-toolkit/src/transport/mod.rs
mcumgr-toolkit/tests/external_transport_tests.rs   (new)
```

Verified to apply three ways from the repository root — `git am`, `git apply`,
and plain `patch -p1`.

Vendored tree: `third_party/mcumgr-toolkit/` (0.16.0 as published, plus one commit)

> **The upstream repo is a Cargo workspace, and the crate is not at its root.**
> It holds `mcumgr-toolkit/`, `mcumgr-toolkit-python/`, `mcumgrctl/` and
> `target_tests/`. The published crate is the flattened form of that one
> subdirectory, so a patch generated against `third_party/` has paths like
> `src/client.rs` that do not exist at the repo root — `patch` then fails the
> `Cargo.toml` hunk and prompts `File to patch:` for the rest. This patch carries
> the `mcumgr-toolkit/` prefix so it applies from the root.

**The two source files on `main` are byte-identical to published 0.16.0**
(checked, not assumed), so the hunks apply exactly. And the repo's crate manifest
has no `[[test]]` stanzas — test autodiscovery is on, unlike the packaged form —
so **no `Cargo.toml` change is needed at all**. That hunk existed only to satisfy
the published tarball and has been dropped.

To regenerate after changing the vendored tree, see the end of this document.

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
ordinary GitHub fork-and-PR flow. Run everything **from the repository root**, not
from inside `mcumgr-toolkit/`.

```bash
gh repo fork Finomnis/mcumgr-toolkit --clone --remote
cd mcumgr-toolkit          # the repo, which also contains a directory of that name
git checkout -b external-transports

# `git am` keeps the patch's commit message and authorship. Verify first:
git apply --check /path/to/runtt/docs/patches/mcumgr-toolkit-external-transports.patch
git am              /path/to/runtt/docs/patches/mcumgr-toolkit-external-transports.patch

# Prove it. The workspace builds everything, so this covers mcumgrctl too.
cargo test -p mcumgr-toolkit
cargo clippy --all-targets
cargo fmt --check

git push -u origin external-transports
gh pr create --repo Finomnis/mcumgr-toolkit --fill
```

`--fill` takes the title and body from the commit message the patch carries.

### If `git am` is unavailable

`patch -p1` works from the repository root too, and was verified. It loses the
commit message, so write one yourself — the patch file's own header is the text to
use, and everything above the first `diff --git` line is it.

### What went wrong the first time, in case it recurs

The original patch was generated by diffing `third_party/mcumgr-toolkit/` against
the **published crate**, which is the flattened form of the repo's
`mcumgr-toolkit/` subdirectory. Two consequences, both now fixed:

* **Paths were missing the `mcumgr-toolkit/` prefix.** `patch -p1` looked for
  `Cargo.toml` at the repo root, found the *workspace* manifest, failed the hunk,
  then could not find `src/client.rs` and fell back to prompting `File to patch:`
  — which is `patch` asking a human to name the file it could not locate.
* **The `Cargo.toml` hunk was never needed.** `cargo package` normalises the
  manifest and switches test autodiscovery off, so the packaged form needs each
  test target declared. The repo does not.

If a future regeneration is needed, generate it against the **repo layout**, not
the published crate — the recipe at the end of this document does that.

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
* **The existing suite is unaffected.** Verified against the published 0.16.0
  source standalone: 94 tests pass, ours included.

## When it lands

1. Bump the dependency to the release containing it.
2. Delete the `[patch.crates-io]` block from the workspace `Cargo.toml`.
3. Delete `third_party/mcumgr-toolkit/`.
4. Delete this file and the patch.

## Regenerating the patch

If the vendored tree changes, regenerate against the **repo layout** rather than
the published crate, or the paths will be wrong again:

```bash
UP=https://raw.githubusercontent.com/Finomnis/mcumgr-toolkit/main/mcumgr-toolkit
W=$(mktemp -d); cd "$W" && git init -q .
mkdir -p mcumgr-toolkit/src/transport mcumgr-toolkit/tests
for f in src/client.rs src/transport/mod.rs; do
  curl -fsSL "$UP/$f" -o "mcumgr-toolkit/$f"
done
git add -A && git commit -qm base

# Overlay the patched files, commit with the message, and emit the patch.
V=/path/to/runtt/third_party/mcumgr-toolkit
cp $V/src/client.rs        mcumgr-toolkit/src/client.rs
cp $V/src/transport/mod.rs mcumgr-toolkit/src/transport/mod.rs
cp $V/tests/external_transport_tests.rs mcumgr-toolkit/tests/
git add -A && git commit -q -m "Allow Transport to be implemented outside the crate" -m "<body>"
git format-patch -1 --stdout
```

Then verify it with `git apply --check` against a fresh clone before trusting it.

Nothing else references the vendored tree — the pin exists solely for this.

---

*Co-authored with Claude*
