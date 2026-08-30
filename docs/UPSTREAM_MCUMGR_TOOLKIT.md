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

## When it lands

1. Bump the dependency to the release containing it.
2. Delete the `[patch.crates-io]` block from the workspace `Cargo.toml`.
3. Delete `third_party/mcumgr-toolkit/`.
4. Delete this file and the patch.

Nothing else references the vendored tree — the pin exists solely for this.

---

*Co-authored with Claude*
