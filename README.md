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

## Quick start

**Assumes Ubuntu or Debian with Docker installed**, and one of the five boards
below. Nothing here needs a Rust toolchain or a debug probe.

### 1. Install the runtime

One static binary, plus the udev rules — which matter more than they look,
because stock Ubuntu runs ModemManager and an AT-command probe landing
mid-upload is a corrupted transfer:

```bash
curl -LO https://github.com/shaunmulligan/runtt/releases/latest/download/runtt-x86_64
chmod +x runtt-x86_64 && sudo install -m 0755 runtt-x86_64 /usr/local/bin/runtt

sudo curl -fLo /etc/udev/rules.d/90-runtt.rules \
  https://raw.githubusercontent.com/shaunmulligan/runtt/main/udev/90-runtt.rules
sudo udevadm control --reload && sudo udevadm trigger
```

Then **merge** this into `/etc/docker/daemon.json` — creating the file if it is
absent, not overwriting what is already in it — and
`sudo systemctl restart docker`, after which `--runtime=runtt` resolves by name:

```json
{ "runtimes": { "runtt": { "path": "/usr/local/bin/runtt" } } }
```

### 2. Provision a board, once

A board needs MCUboot and the device half of the contract in flash before the
runtime can reach it: one downloaded script and one command, with no toolchain
and no checkout.

```bash
curl -fLO https://raw.githubusercontent.com/shaunmulligan/runtt-boards/main/scripts/runtt-board
chmod +x runtt-board
./runtt-board provision rpi_pico --name mcu-01
```

`--name` is written into the board's flash and becomes its USB serial, so
`usb:mcu-01` addresses that board on any machine from then on.

| Board | SoC | Provisioning | Transports |
|---|---|---|---|
| [Raspberry Pi Pico](https://github.com/shaunmulligan/runtt-boards/blob/main/docs/start-rpi_pico.md) | RP2040, Cortex-M0+ | drag-and-drop UF2, no probe | USB |
| [Raspberry Pi Pico 2 W](https://github.com/shaunmulligan/runtt-boards/blob/main/docs/start-rpi_pico2.md) | RP2350, Cortex-M33 | drag-and-drop UF2, no probe | USB |
| [Waveshare ESP32-S3 DevKitC](https://github.com/shaunmulligan/runtt-boards/blob/main/docs/start-esp32s3_devkitc.md) | ESP32-S3, Xtensa LX7 | `esptool` over USB, no probe | USB |
| [Adafruit RP2040 CAN Bus Feather](https://github.com/shaunmulligan/runtt-boards/blob/main/docs/start-adafruit_feather_canbus_rp2040.md) | RP2040 + MCP25625 | drag-and-drop UF2, no probe | USB **and** CAN |
| [Adafruit Feather nRF52840](https://github.com/shaunmulligan/runtt-boards/blob/main/docs/start-adafruit_feather_nrf52840.md) | nRF52840, Cortex-M4 | **SWD probe, and it erases the stock bootloader** | USB |

Three silicon vendors, four SoC families, two instruction-set architectures —
all passing the same contract, which is what makes it a contract rather than
something shaped around one board. Each is proven on hardware end to end:
upload, mark test, reset, verify, confirm, and revert.

⚠️ Provisioning images are signed with MCUboot's **published development key**,
so no trust root is enrolled and an image signature proves nothing. Fine on a
bench, unfit for a fleet — generate your own key before shipping anything.

### 3. Build firmware as a container image

The build environment is one image, built once per machine; an application
directory then needs only a six-line Dockerfile:

```bash
git clone https://github.com/shaunmulligan/runtt-boards
docker build -f runtt-boards/builder/Dockerfile -t runtt-builder:v4.4.2 runtt-boards

git clone https://github.com/shaunmulligan/runtt-examples
cd runtt-examples/app1
docker build --build-arg BOARD=rpi_pico/rp2040/mcuboot -t my-firmware:v1 .
```

The builder's base is `zephyrprojectrtos/ci` at **23 GB**, almost all of it
toolchains — a once-per-machine download, but worth knowing about before
starting on a small disk.

`BOARD` is the only board-specific thing in the build — one of
`rpi_pico/rp2040/mcuboot`, `rpi_pico2/rp2350a/m33/w/mcuboot`,
`esp32s3_devkitc/esp32s3/procpu`,
`adafruit_feather_canbus_rp2040/rp2040/mcuboot` or
`adafruit_feather_nrf52840/nrf52840`. The result is `FROM scratch`: a signed
MCUboot image and an entrypoint naming it, which is all the runtime reads.

### 4. Deploy

```bash
docker run --rm --network none --runtime=runtt \
  --annotation dev.runtt.target=usb:mcu-01 my-firmware:v1
```

The image uploads over SMP, MCUboot swaps it in, and it is confirmed only once
the new firmware enumerates, speaks SMP and heartbeats — so an image that broke
the contract reverts on its own.

That command then **does not return**: the runtime stays resident as the
container process, with the board's logs as its stdout. `docker logs -f` works,
`docker stop` releases the board, redeploying the same digest is a no-op, and
losing the device exits non-zero so the restart policy applies. A firmware
service needs no network namespace, hence `--network none`.

Next: [runtt-examples](https://github.com/shaunmulligan/runtt-examples) walks
the whole loop — two applications, deployed, switched and switched back — with
every command and transcript run against real hardware.

## Install details

Release binaries are built for `x86_64`, `aarch64` and `armv7hf`, each
statically linked against musl, which is the point: the devices this runs on are
rarely the machine it was built on. Every release also carries `SHA256SUMS`.

**podman** needs no daemon configuration at all — it takes the runtime as a
path, and needs no root:

```bash
podman run --rm --network none --runtime=/usr/local/bin/runtt \
  --annotation dev.runtt.target=usb:mcu-01 my-firmware:v1
```

Pick one engine and stay on it within a session: docker and podman keep separate
image stores, so building with one and running with the other fails to find the
image it just built. balena-engine is stock moby architecture and takes the same
`daemon.json` registration as Docker.

## Trying it, with no hardware — from source

**This path needs a checkout and a Rust toolchain**, so it is aimed at anyone
working *on* runtt rather than deploying with it. The installed binaries above
are the deploy path; `runtt-mock` is a development tool and is deliberately not
published as a release asset.

`runtt-mock` stands up a fake board on a pty: it speaks the whole wire contract
and can inject faults, so every path through the runtime — including the error
paths — can be exercised with nothing plugged in.

```bash
cargo build

# Stand up a mock device on a pty.
./target/debug/runtt-mock --symlink /tmp/mcu-tty &

# Build a firmware image. Use a real imgtool-signed image, not random bytes:
# the runtime parses the MCUboot header and TLV digest to establish image
# identity, so anything else is rejected with "not an MCUboot image".
mkdir -p /tmp/fw && cp crates/runtt-smp/tests/fixtures/app.signed.bin /tmp/fw/
printf 'FROM scratch\nADD app.signed.bin /\nENTRYPOINT ["app.signed.bin"]\n' > /tmp/fw/Dockerfile
podman build -t mcu-fw:demo /tmp/fw

# Deploy to it. podman takes --runtime=<path> with no daemon config and no root.
podman --runtime="$PWD/target/debug/runtt" run --rm \
  --annotation dev.runtt.target="tty:$(readlink -f /tmp/mcu-tty)" \
  mcu-fw:demo
```

The runtime stays **resident** after the deploy, streaming the device's logs to
container stdio for as long as the firmware runs — so that last command does not
return on its own. Ctrl-C it, or wrap it in `timeout 60` as CI does.

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
| `docs/FORKED_DEPENDENCY.md` | why we build against an unreleased `mcumgr-toolkit` commit, and how to drop it |
| [`NOTES.md`](NOTES.md) | **for maintainers, not users:** roadmap, by-hand procedures, research, and how the current state was reached |

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
