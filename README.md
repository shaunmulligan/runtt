# runtt

**An OCI runtime for tiny tethered devices.** It deploys firmware to a discrete
microcontroller instead of running a container.

A firmware service is an ordinary container image — `FROM scratch`, a signed
MCUboot image, an entrypoint — pulled by the engine like any other image. runtt is
handed the bundle in place of runc. It resolves the target board, uploads the
image over MCUmgr **SMP**, resets, and confirms only once the new image proves
itself. It then stays resident as the container process: board logs go to
container stdio, SMP echo heartbeats prove liveness, and losing the device means a
non-zero exit, so the engine's restart policy does the rest.

One service = one MCU, exclusive occupancy.

## Why this shape

Flashing an attached MCU from a container normally means a privileged service
running vendor tools against `/dev/ttyUSB0` — outside releases, deltas, restart
policies and log capture. Making the runtime the integration point puts firmware
on the same rails as every other service, and the firmware container needs no
privileges and no device mappings at all, because the runtime lives outside the
container.

## Status

**The full loop works on real hardware**, on two boards, through three engines.

| | |
|---|---|
| **Boards** | Raspberry Pi Pico (RP2040) and Adafruit Feather nRF52840, both end to end with MCUboot: upload, mark test, reset, verify, confirm |
| **Engines** | Docker 28, podman, and the plain runc-style CLI. Also compatible with balena-engine, which is stock moby architecture |
| **Transports** | USB CDC-ACM, bare serial, and CAN (SMP over ISO-TP) |
| **Simulated** | Zephyr `native_sim` and a fault-injecting SMP mock with seven failure modes |

Also working: firmware as a self-contained container project (`docker build .`
from an application directory against a reusable builder image); per-board
identity in flash so one image serves a fleet; provisioning over UF2 with no
debug probe; exclusive occupancy; restart-policy propagation; `docker logs`
capture; same-digest no-op redeploy.

**Not built yet:**

- **A hardware CI gate.** CI is simulated-only, deliberately — the design and the
  traps that make a naive gate worthless are in `docs/HARDWARE_GATE.md`.
- **CAN on physical hardware.** The transport is proven end to end on a virtual
  bus (`vcan`) and gates in CI; two boards with different CAN controllers are on
  order to prove it is controller-agnostic. See [runtt-boards](https://github.com/shaunmulligan/runtt-boards).
- **A fleet trust root.** Everything is signed with MCUboot's *published*
  development key. Fine on a bench, unfit for anything else — see the
  signing-key warning in [runtt-zephyr-module](https://github.com/shaunmulligan/runtt-zephyr-module).
- **Revert on real hardware.** Exercised in MCUboot's own simulator, not yet on a
  board.

> **On CI:** `.github/workflows/ci.yml` is a well-formed file, not a running
> system. This repo has no git remote, so it has never executed anywhere. The
> suites it runs do pass locally.

## Trying it, with no hardware

```bash
cargo build

# Stand up a mock device on a pty.
./target/debug/runtt-mock --symlink /tmp/mcu-tty &

# Build a firmware image.
mkdir -p /tmp/fw && head -c 8192 /dev/urandom > /tmp/fw/app.signed.bin
printf 'FROM scratch\nADD app.signed.bin /\nENTRYPOINT ["app.signed.bin"]\n' > /tmp/fw/Dockerfile
podman build -t mcu-fw:demo /tmp/fw

# Deploy to it. podman takes --runtime=<path> with no daemon config and no root.
podman --runtime="$PWD/target/debug/runtt" run --rm \
  --annotation dev.runtt.target="tty:$(readlink -f /tmp/mcu-tty)" \
  mcu-fw:demo
```

For Docker, register the runtime once (`sudo scripts/register-docker.sh`), then
`docker run --runtime runtt --network none ...`. A firmware service needs no
network namespace.

Inject a fault to watch an error path:

```bash
./target/debug/runtt-mock --fault bad-hash --symlink /tmp/mcu-tty
```

For the real thing on a board, follow the walkthrough in
[runtt-examples](https://github.com/shaunmulligan/runtt-examples) — every command and transcript there was run
against hardware.

## Placement

The target comes from an OCI annotation, transport-prefixed so new transports
slot in without breaking existing labels:

```
dev.runtt.target: usb:3-6            # kernel USB port path
dev.runtt.target: usb:feather-01     # ...or the board's own serial
dev.runtt.target: tty:/dev/ttyACM0   # bare serial, or a simulator's pty
dev.runtt.target: can:can0/0x45      # SocketCAN interface and ISO-TP node id
```

The two `usb:` forms answer different questions, and both are legitimate: a **port
path** means *"whatever board is in this physical position"*, right when boards
are replaceable and position defines the role; a **serial** means *"this specific
board, wherever it is"*, and is the only form that makes a compose file portable
between machines. They are told apart by shape, not by guessing — see
[docs/WIRE_CONTRACT.md](docs/WIRE_CONTRACT.md).

Resolution identifies the management and log channels by their **USB interface
string descriptor** (`runtt-mgmt` / `runtt-log`), never by interface number, and
never by VID/PID — a product may ship its own VID, and the descriptor is the part
the firmware contract owns.

Also honoured: `dev.runtt.log-target` (a serial console for a board managed over
CAN) and `dev.runtt.skip-if-same-hash` (default on — redeploying an image the
device already runs, confirmed, is a no-op).

## The safety invariant

Confirmation is only reachable through the contract. The runtime uploads to the
inactive slot, marks it **test**, resets, and sends **confirm** only after the new
image enumerates, speaks SMP and heartbeats. An image that removed or broke the
contract therefore can never be confirmed — because confirming requires the very
capability that was lost — so MCUboot reverts it on the next reset.

Contract loss is never remotely permanent, by construction.

## Layout

| Path | What |
|---|---|
| `crates/runtt` | the runtime binary: OCI verbs, resident proxy, deploy sequence |
| `crates/runtt-smp` | the five-method SMP surface, over `mcumgr-toolkit` |
| `crates/runtt-transport` | the transport seam: USB, bare serial, CAN |
| `crates/runtt-mock` | SMP server with injectable faults, for testing error paths |
| `udev/90-runtt.rules` | device access and the contract-keyed device tree |

### Documentation

| Doc | What |
|---|---|
| `docs/ARCHITECTURE.md` | how it fits together, and why an OCI runtime rather than a service |
| `docs/WIRE_CONTRACT.md` | the firmware-side interface: channels, framing, image semantics, identity |
| `docs/OCI_COMPLIANCE.md` | what we implement, what we don't, and what engines actually pass |
| `docs/ROADMAP.md` | where this is and what is worth doing next |
| `docs/HARDWARE_GATE.md` | why CI is simulated-only, and the design for a hardware gate |
| `docs/MANUAL_VERIFICATION.md` | walk the native_sim flow by hand, step by step |
| `docs/MANUAL_LOG_DEMUX.md` | verify the single-channel log demux by hand |
| `docs/MICROROS.md` | research: what a micro-ROS robotics use case would need |
| `docs/FORKED_DEPENDENCY.md` | why we build against a fork of `mcumgr-toolkit`, and how to drop it |

## The runtt repositories

runtt is four repositories, because they have different lifecycles: this one ships
binaries, the module goes into other people's source trees, and board support
changes on hardware's schedule rather than the runtime's.

| Repo | What it holds | Start here if |
|---|---|---|
| **`runtt`** (this one) | the OCI runtime — the **host** side | you want to know what runtt is, or to work on the runtime |
| [`runtt-zephyr-module`](https://github.com/shaunmulligan/runtt-zephyr-module) | the Zephyr module — the **device** side | you have firmware and want it manageable |
| [`runtt-boards`](https://github.com/shaunmulligan/runtt-boards) | provisioning, board bring-up, the west manifest | you have a board that has never run runtt |
| [`runtt-examples`](https://github.com/shaunmulligan/runtt-examples) | two worked applications, and the walkthrough | you want to watch it work end to end |

**New here?** You are in the right place — read on, then follow the walkthrough in
[`runtt-examples`](https://github.com/shaunmulligan/runtt-examples).

[docs/WIRE_CONTRACT.md](docs/WIRE_CONTRACT.md) lives here and is the seam between
all four: the runtime is what enforces the version and refuses a device whose
major disagrees.

## Contributing

See `CONTRIBUTING.md`. In short: the test suites and the `native_sim` gates are
the contract, `cargo clippy --all-targets` must be clean, and a change to
anything on the wire needs `docs/WIRE_CONTRACT.md` updated in the same commit —
there is a test that enforces the last one.

## Licence

Dual licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this work by you shall be dual licensed as above, without any
additional terms or conditions.
