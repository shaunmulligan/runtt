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

Phases 0 of the plan is complete and the simulator loop runs headless in CI.

Working:

- The five OCI verbs, driven successfully by **Docker 28** and **podman**.
- The full deploy sequence — upload, mark test, reset, verify, confirm — against
  both a mock device and **Zephyr native_sim**, end to end through a container
  engine (`scripts/native-sim-engine-e2e.sh`).
- Exclusive occupancy, restart-policy propagation, `docker logs` capture.
- A fault-injecting SMP mock with seven failure modes.

Not yet built: the Zephyr module and native_sim integration (phase 1), and
hardware bring-up (phase 2).

## Layout

| Path | What |
|---|---|
| `crates/mcu-runtime` | the runtime binary: OCI verbs, resident proxy, deploy sequence |
| `crates/smp-client` | the five-method SMP surface, over `mcumgr-toolkit` |
| `crates/transport` | the transport seam: USB now, CAN later |
| `crates/smp-mock` | SMP server with injectable faults, for testing error paths |
| `docs/ARCHITECTURE.md` | how it fits together, and why an OCI runtime rather than a service |
| `docs/FIRMWARE_GUIDE.md` | getting a Zephyr app onto the platform: tree layout, build, traps |
| `docs/WIRE_CONTRACT.md` | the firmware-side interface: channels, framing, image semantics |
| `docs/PROVISIONING.md` | the one physical act: getting a board manageable in the first place |
| `docs/OCI_COMPLIANCE.md` | what we implement, what we don't, and what engines actually pass |
| `docs/MANUAL_VERIFICATION.md` | walk the native_sim flow by hand, step by step |
| `docs/MANUAL_LOG_DEMUX.md` | verify the single-channel log demux by hand |
| `docs/HARDWARE_GATE.md` | why CI is simulated-only, and the design for a hardware gate |
| `docs/MICROROS.md` | research: what a micro-ROS robotics use case would need (future cycle) |
| `udev/90-balena-mcu.rules` | device access and the contract-keyed device tree |
| `scripts/setup-prereqs.sh` | one-time host setup for hardware work (`--check` to just verify) |

## Trying it

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
