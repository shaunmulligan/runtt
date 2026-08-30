# mcu-runtime

An OCI runtime that deploys firmware to a discrete microcontroller instead of
running a container.

A firmware service is a normal container image — `FROM scratch`, a signed
MCUboot image, an entrypoint — pulled by the engine like any other image. This
runtime is handed the bundle in place of runc. It resolves the target board,
uploads the image over MCUmgr **SMP**, resets, and confirms only once the new
image proves itself. It then stays resident as the container process: board logs
go to container stdio, SMP echo heartbeats prove liveness, and losing the device
means a non-zero exit, so the engine's restart policy does the rest.

One service = one MCU, exclusive occupancy.

## Why this shape

Today, flashing an attached MCU from a container means a privileged service
running vendor tools against `/dev/ttyUSB0` — outside releases, deltas, restart
policies and the dashboard. Making the runtime the integration point puts
firmware on the same rails as every other service, and the firmware container
needs no privileges or device mappings at all, because the runtime is outside
the container.

## Status

**The full loop works on real hardware.** Two firmware applications, each an
ordinary container project, deployed and switched on a Raspberry Pi Pico with
`docker run` — see `docs/WALKTHROUGH.md`, where every command and transcript was
run against the board.

Working:

- The five OCI verbs, driven by **Docker 28** and **podman**.
- The full deploy sequence — upload, mark test, reset, verify, confirm — against
  a mock device, **Zephyr native_sim**, and an **RP2040 with MCUboot**, end to
  end through a container engine.
- Firmware as a self-contained container project: `docker build .` from an
  application directory, against a reusable builder image
  (`firmware/examples/app1`, `app2`).
- Provisioning over UF2 with no debug probe (`docs/PROVISIONING.md`).
- Exclusive occupancy, restart-policy propagation, `docker logs` capture,
  same-digest no-op redeploy.
- A fault-injecting SMP mock with seven failure modes.

**In progress — Adafruit Feather nRF52840** (branch `nrf52840-bringup`). The
board support, build and provisioning scripts are written and both example apps
build clean against `adafruit_feather_nrf52840/nrf52840` with MCUboot. **None of
it has run on hardware yet**, so treat it as compiled, not working.

Not built:

- **A hardware CI gate.** CI is simulated-only, deliberately; the design and its
  traps are written up in `docs/HARDWARE_GATE.md`.
- **CAN transport.** The target annotation is transport-prefixed for it, but
  only `usb:` and `tty:` are implemented.
- **A fleet trust root.** Everything is signed with MCUboot's *published*
  development key, which is fine for a bench PoC and unfit for anything else.
  See the signing-key warning in `docs/FIRMWARE_GUIDE.md`.

> **On CI:** `.github/workflows/ci.yml` is a well-formed file, not a running
> system. This repo has no git remote and no tags, so it has never executed
> anywhere. The suites it runs do pass locally.

## Layout

| Path | What |
|---|---|
| `crates/mcu-runtime` | the runtime binary: OCI verbs, resident proxy, deploy sequence |
| `crates/smp-client` | the five-method SMP surface, over `mcumgr-toolkit` |
| `crates/transport` | the transport seam: USB now, CAN later |
| `crates/smp-mock` | SMP server with injectable faults, for testing error paths |
| `docs/WALKTHROUGH.md` | build two releases and switch an MCU between them with `docker run` |
| `docs/UPSTREAM_MCUMGR_TOOLKIT.md` | a one-method patch to submit upstream, and why |
| `docs/ROADMAP.md` | where this is and what is worth doing next |
| `docs/ARCHITECTURE.md` | how it fits together, and why an OCI runtime rather than a service |
| `docs/FIRMWARE_GUIDE.md` | getting a Zephyr app onto the platform: tree layout, build, traps |
| `docs/WIRE_CONTRACT.md` | the firmware-side interface: channels, framing, image semantics |
| `docs/PROVISIONING.md` | the one physical act: getting a board manageable in the first place |
| `docs/OCI_COMPLIANCE.md` | what we implement, what we don't, and what engines actually pass |
| `docs/MANUAL_VERIFICATION.md` | walk the native_sim flow by hand, step by step |
| `docs/MANUAL_LOG_DEMUX.md` | verify the single-channel log demux by hand |
| `docs/HARDWARE_GATE.md` | why CI is simulated-only, and the design for a hardware gate |
| `docs/MICROROS.md` | research: what a micro-ROS robotics use case would need (future cycle) |
| `docs/MCUBOOT_SWAP_BUG.md` | draft upstream report: MCUboot hangs in find_last_idx on RP2040 |
| `udev/90-balena-mcu.rules` | device access and the contract-keyed device tree |
| `scripts/setup-prereqs.sh` | one-time host setup for hardware work (`--check` to just verify) |
| `scripts/build-feather.sh` | build for the Feather nRF52840: bringup, mcuboot, provision |
| `scripts/backup-nrf52840.sh` | back up flash and UICR before the first destructive flash |
| `scripts/flash-feather.sh` | provision a Feather over SWD; refuses to run without a backup |

## Trying it

No hardware needed — this deploys a throwaway image to a mock device on a pty.
For the real thing on a board, follow `docs/WALKTHROUGH.md` instead.

```bash
cargo build

# Stand up a mock device on a pty.
./target/debug/smp-mock --symlink /tmp/mcu-tty &

# Build a firmware image.
mkdir -p /tmp/fw && head -c 8192 /dev/urandom > /tmp/fw/app.signed.bin
printf 'FROM scratch\nADD app.signed.bin /\nENTRYPOINT ["app.signed.bin"]\n' > /tmp/fw/Dockerfile
podman build -t mcu-fw:demo /tmp/fw

# Deploy to it. podman takes --runtime=<path> with no daemon config and no root.
podman --runtime="$PWD/target/debug/mcu-runtime" run --rm \
  --annotation io.balena.mcu.target="tty:$(readlink -f /tmp/mcu-tty)" \
  mcu-fw:demo
```

For Docker, register the runtime once (`sudo scripts/register-docker.sh`), then
`docker run --runtime mcu-runtime --network none ...`. A firmware service needs
no network namespace.

Inject a fault to watch an error path:

```bash
./target/debug/smp-mock --fault bad-hash --symlink /tmp/mcu-tty
```

## Placement

The target comes from an OCI annotation, transport-prefixed from day one so
other transports slot in without breaking existing labels:

```
io.balena.mcu.target: usb:3-6          # kernel USB port path
io.balena.mcu.target: tty:/dev/ttyACM0 # bare serial, or a simulator's pty
io.balena.mcu.target: can:vcan0/0x42   # named, not implemented this cycle
```

`usb:` resolution reads sysfs directly and identifies the management and log
channels by their **USB interface string descriptor**
(`balena-mcu-mgmt` / `balena-mcu-log`), never by interface number — customers may
ship their own VID, and the descriptor is the part the firmware contract owns.

Also honoured: `io.balena.mcu.skip-if-same-hash` (default on). Redeploying an
image the device already runs, confirmed, is a no-op.

## The safety invariant

Confirmation is only reachable through the contract. The runtime uploads to the
inactive slot, marks it **test**, resets, and sends **confirm** only after the
new image enumerates, speaks SMP and heartbeats. An image that removed or broke
the contract therefore can never be confirmed — because confirming requires the
very capability that was lost — so MCUboot reverts it on the next reset.

Contract loss is never remotely permanent, by construction.

---

*Co-authored with Claude*
