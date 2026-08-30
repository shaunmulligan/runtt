# Upstreaming `MCUmgrClient::new_from_transport`

A one-method addition to [`mcumgr-toolkit`](https://crates.io/crates/mcumgr-toolkit),
carried locally until it lands upstream. **This is for you to submit** — the
patch, the reasoning and a draft description are below.

Patch: `docs/patches/mcumgr-toolkit-new_from_transport.patch`
Vendored tree: `third_party/mcumgr-toolkit/` (0.16.0 as published, plus this one commit)

---

## What it adds

```rust
pub fn new_from_transport<T: Transport + Send + 'static>(transport: T) -> Self {
    Self {
        connection: Connection::new(transport),
        smp_frame_size: ZEPHYR_DEFAULT_SMP_FRAME_SIZE.into(),
    }
}
```

Plus `Transport` to the existing `use crate::transport::{…}` list. That is the
whole diff: one import, one method, and its doc comment.

## Why it is needed

`Transport` is a public trait, and `Connection::new<T: Transport + Send + 'static>`
is already public and generic. But `MCUmgrClient` cannot be built from either:

* its `connection` field is private, and
* all four constructors — `new_from_serial`, `new_from_usb_serial`,
  `new_from_ble`, `new_from_udp` — are tied to concrete transport types.

So a `Transport` implementation living outside the crate compiles fine and is
then unusable. The trait is public but unreachable.

## The motivating case

SMP over **ISO-TP** (ISO 15765-2) on a CAN bus, for a project that deploys MCU
firmware as OCI container images and needs a management channel over CAN
alongside the existing USB one.

ISO-TP is datagram-oriented and handles its own segmentation, so the transport
carries raw SMP frames and ends up close to a copy of the existing
`UdpTransport` — about ninety lines. Both Linux (`can-isotp`, mainline since
5.10) and Zephyr (`subsys/canbus/isotp`) provide ISO-TP, so no framing has to be
invented on either side. The only thing missing is a way to hand the finished
transport to a client.

It generalises beyond CAN: any bearer that can carry a raw SMP frame — a test
double, a custom radio link, a socket to a simulator — hits the same wall today.

## Risk

Additive. No existing signature changes, no behaviour changes, nothing removed.
`Connection::new` already accepts exactly this bound, so the new constructor
cannot express anything the crate does not already support internally.

## Draft PR description

> **Add `MCUmgrClient::new_from_transport`**
>
> `Transport` is public and `Connection::new` is already generic over it, but
> `MCUmgrClient` can only be constructed from one of the four built-in
> transports, and its `connection` field is private. That leaves out-of-crate
> `Transport` implementations compilable but unusable.
>
> This adds a generic constructor alongside the existing ones. It is purely
> additive: no signature or behaviour changes.
>
> My use case is SMP over ISO-TP on CAN — the transport is close to a copy of
> `UdpTransport`, since ISO-TP is datagram-oriented and does its own
> segmentation, but there is currently no way to hand it to a client. The same
> applies to any custom bearer, including test doubles.

## When it lands

1. Bump the dependency to the release containing it.
2. Delete the `[patch.crates-io]` block from the workspace `Cargo.toml`.
3. Delete `third_party/mcumgr-toolkit/`.
4. Delete this file and the patch.

Nothing else references the vendored tree — the pin exists solely for this.

---

*Co-authored with Claude*
