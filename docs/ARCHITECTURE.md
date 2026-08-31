# Architecture

Firmware for a discrete MCU, delivered as a normal container image.

## The problem this replaces

A device with an MCU attached to it normally gets that MCU flashed from a
privileged container running vendor tools against `/dev/ttyUSB0`. That works, and
it sits outside everything a container platform provides: no releases, no deltas,
no restart policies, no log capture, no rollback. The firmware is not a service,
it is a side effect of one.

This is the shape whether the platform is plain Docker, a Kubernetes edge
distribution, or a fleet manager like balena. The gap is the same one: the
orchestrator has no idea the firmware exists.

## The idea in one paragraph

A firmware service is an ordinary OCI image — `FROM scratch`, a signed MCUboot
image, an entrypoint naming it. The engine pulls it like any image, deltas
included, and hands it to a **custom runtime instead of runc**. The runtime
resolves which board the service is for, uploads the image over MCUmgr **SMP**,
resets, and confirms only once the new firmware proves itself. It then stays
resident *as the container process*: board logs become container stdio, SMP echo
heartbeats prove liveness, and losing the device is a non-zero exit, so restart
policies do the rest.

One service, one MCU, exclusive occupancy.

## The shape

```
  balena-engine / docker / podman
            │  create/start/state/kill/delete   (runc-style CLI)
            ▼
      runtt  ◄─────────── the container process, from the engine's view
            │
            │  SMP over a byte pipe
            ▼
   ┌──────────────────┐        ┌──────────────────────────┐
   │  transport       │  USB   │  MCU                     │
   │  usb: / tty:     ├───────►│  MCUboot  +  application │
   │  can: (later)    │        │  runtt module       │
   └──────────────────┘        └──────────────────────────┘
        two channels:  runtt-mgmt (SMP)  ·  runtt-log (stdio)
```

The runtime runs **outside** the container. That is worth dwelling on: the
firmware container needs no `privileged: true`, no `devices:` mapping and no
feature labels, because nothing inside it ever touches the MCU. It is a security
improvement over the status quo, not just a packaging one.

## Why an OCI runtime rather than a service

Because it makes firmware a *release*. Everything the platform already does —
pulling, deltas, restart policies, log capture, the dashboard — applies with no
new machinery, provided the firmware presents as a container process. That is the
whole trick, and it is why the runtime stays resident rather than exiting after
flashing: a process that exits is a container that stopped.

Prior art: [`arm/remoteproc-runtime`](https://github.com/arm/remoteproc-runtime)
does the same for coprocessors behind the Linux remoteproc API. We diverge in one
deliberate way — it drops stdio because "firmware has no standard I/O channels",
whereas piping MCU logs to container stdout is our headline feature.

## Components

| Crate | Responsibility |
|---|---|
| `runtt` | the OCI verbs, the resident proxy, the deploy sequence |
| `smp-client` | a five-method SMP surface over `mcumgr-toolkit`, plus MCUboot image parsing |
| `transport` | the byte-pipe seam: USB and bare serial now, CAN later |
| `runtt-mock` | an SMP server with injectable faults, for testing error paths |

Two boundaries carry the future flexibility, and both were cheap to put in early:

* **`smp-client`** is five methods (`flash`, `echo`, `image_list`, `reset`,
  `set_state`) in front of a young dependency, so replacing it is one file.
* **`transport`** is the byte pipe under the SMP logic. The production follow-on
  is CAN, and keeping it a separate crate makes that structural rather than
  aspirational. Bring-up over a debug probe's UART bridge exercises the same seam
  for free.

## The container lifecycle

The detail that makes restart policies work, and the easiest thing to get wrong:

1. The engine's shim runs `create <id> --bundle <dir> --pid-file <path>`. We
   **fork the resident proxy, write its PID to `--pid-file`, and exit 0.** Because
   the parent exits, the proxy reparents to the shim — and *that PID is the
   container*.
2. `start <id>` signals the proxy (`SIGUSR1`); only then does it do real work.
3. The proxy flashes, then pumps logs and heartbeats, holding the stdio the engine
   gave it. Whatever it writes to fd 1 is `docker logs`.
4. It exits non-zero on detach or heartbeat loss → the shim reaps it → `TaskExit`
   → **the restart policy fires.**

The proxy must be spawned in `create`, not `start`, or there is a window where the
shim has no PID to track.

See `docs/OCI_COMPLIANCE.md` for what the engine actually passes, measured rather
than assumed.

## The deploy sequence, which is the safety property

```
upload            → the inactive slot
set_state(TEST)     never confirm here
reset
   wait for the device to enumerate, speak SMP and heartbeat
set_state(CONFIRM)  only now
```

**Confirmation is reachable only through the contract.** An image that removed or
broke the contract can never be confirmed, because confirming requires the very
capability that was lost. If the confirm never arrives, MCUboot reverts on the
next reset.

So a bad update is self-healing by construction, not by a timer or a watchdog we
have to get right. This is the single most important property in the design, and
everything else is arranged to preserve it.

## Placement

Which board a service is for arrives as an OCI annotation:

```
dev.runtt.target: usb:3-6
```

Transport-prefixed from the outset, so `can:` and `tty:` slot in without breaking
existing labels. An unprefixed label is an error, not a guess.

`usb:` resolution reads sysfs and identifies the two channels by their **USB
interface string descriptor**, never by VID/PID or interface number. Customers
ship their own VID, and `ID_PATH` is interface-suffixed — the two channels of one
composite device land on different `ID_PATH`s. The descriptor is the part of the
identity the firmware contract actually controls.

Exclusive occupancy is an `flock` held by the *open file description*, so `create`
acquires it and the proxy inherits it. `create` exits, the claim survives, and the
kernel releases it when the proxy dies. No stale lockfiles.

## Testing strategy

The rungs each reduce what the next has to prove:

| Rung | Proves | Needs |
|---|---|---|
| `runtt-mock` | the client's error paths, deterministically | nothing |
| MCUboot's own `sim/` | swap, revert and confirm under injected power failure | nothing |
| Zephyr `native_sim` | the real SMP server, two channels, reset, reconnect | nothing |
| a container engine | the OCI contract, stdio, restart policies | podman |
| real hardware | USB enumeration, the descriptors, actual swap | a board |

The first four are headless and run in CI, which is why the inner loop needs no
hardware at all.

`native_sim` deliberately does **not** prove swap or confirm: MCUboot cannot
chain-load there — its POSIX path computes `flash_base + offset` and calls it as
a function pointer, and "flash" is an `mmap`'d data file with no `PROT_EXEC`.
MCUboot's own Rust simulator covers that instead, compiling the real `bootutil`
sources over a NOR-flash model with injectable failures.

## Deliberately out of scope

No MCU-side networking. No power control — recovering a wedged board is user-wired
via loom GPIO, not platform code. No wasm. No log streaming over SMP (Zephyr has
none, which is why the second channel exists). No serial recovery in shipped
images; it is a bench aid, and on RP2040 the mask-ROM bootloader makes it
unnecessary.

### Orchestrator integration is deliberately out of scope

runtt reads placement out of the OCI spec's `annotations` map and nothing more.
How an annotation gets there is somebody else's business: `docker run
--annotation`, a compose file, or a fleet manager's agent reconciling target
state. That boundary is the reason the runtime works unchanged under Docker,
podman and balena-engine, and would work under anything else that speaks the
runc-style CLI.

Concretely, a platform wanting to drive runtt needs to do two things: pass
`runtime: runtt` and the `dev.runtt.*` annotations through to the engine. Nothing
in this repository depends on how.

---

*Co-authored with Claude*
