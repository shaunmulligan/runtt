# Roadmap

Where this is, and what is worth doing next. Written 2026-08-30, after both the
Pico and the Feather were proven end to end.

## Where we are

Two boards, two architectures, one runtime and one contract. On each of a
Raspberry Pi Pico (RP2040, Cortex-M0+) and an Adafruit Feather nRF52840
(Cortex-M4), `docker run --runtime=runtt` uploads firmware over USB,
MCUboot swaps and confirms it, the new image boots, and its logs reach container
stdio. Digests verified independently against imgtool in both cases.

That two different architectures pass the same contract is the part that makes
the contract credible rather than shaped around one board.

**Not yet done:** no git remote, so CI has never run anywhere; revert is
untested on hardware; and the upstream MCUboot report is drafted but unfiled.

---

## 0. Push to a remote, and get CI green

**Before anything else, and it is not a formality.** Three separate artefacts in
this repo turned out never to have been executed: `ci.yml` (two independent
faults), the test fixtures (`*.bin` gitignored, so a fresh clone would not
compile), and `firmware/app/Dockerfile` (three faults). Every one looked
plausible on review.

The three CI jobs are all simulated and need no hardware. Until they run
somewhere other than one laptop, "it works" means "it works here".

---

## 1. ESP32 — a third target

Both options verified against our pinned Zephyr v4.4.2 tree.

### ESP32-S3 — the direct port

`esp32s3_common.dtsi` declares `usb_otg@60080000`, compatible
`"espressif,esp32-usb-otg", "snps,dwc2"`, and `udc_dwc2_vendor_quirks.h`
already carries ESP32 quirks. So the S3 has a real USB device controller on the
same `device_next` stack we use, and the dual-CDC contract ports directly.

Board `esp32s3_devkitc`. The node ships `status = "disabled"`, so enabling it is
the board section's job — the same shape as the other two boards.

### ESP32-C3 — the single-channel proof

No USB-OTG. Instead the built-in USB Serial/JTAG peripheral, driven by
`serial_esp32_usb.c`, which presents exactly **one** fixed CDC channel.

That is `CONFIG_RUNTT_CHANNELS=1` plus the log demux — and the module's own
Kconfig help already names "ESP32-C3 class" as the reason that option exists. It
would be the first real hardware validation of the demux, which today is proven
on native_sim, the mock, and only incidentally on hardware.

Board `esp32c3_devkitm`, about five pounds.

### What to expect to fight

MCUboot's Kconfig has ESP32-specific carve-outs — `BOOT_PREFER_SWAP_OFFSET` is
`default y if … && !SOC_FAMILY_ESPRESSIF_ESP32`, so ESP32 gets a different swap
mode by default. Given a mismatched swap assumption cost a full day on RP2040,
**pin the swap mode explicitly and verify it rather than inheriting a default.**

No Espressif board in Zephyr currently declares a `cdc-acm` node, so we would be
first there too.

**Order:** S3 first (proves portability), C3 second (proves single-channel). Skip
the classic ESP32 — no USB at all, so after the Feather it validates nothing new.

---

## 2. CAN — and it needs no hardware to start

**Answering the question directly: CAN can be developed and tested end to end
with no hardware at all.** The blocker is not a board, it is a missing transport.

What already exists:

* `zephyr/drivers/can/can_native_linux.c` bridges native_sim to a **Linux
  SocketCAN interface**, and `native_sim.dts` already declares the node
  (`compatible = "zephyr,native-linux-can"`, `host-interface = "zcan0"`,
  currently `status = "disabled"`). Its own comment documents the setup:
  `sudo ip link property add dev vcan0 altname zcan0`.
* `can_loopback.c` for a pure in-process loopback.
* The host kernel here already has the `vcan` module.

So the full loop is available on one machine: native_sim firmware on `vcan0`,
our runtime speaking SocketCAN on the same interface. No transceivers, no wiring,
and it can run in CI exactly like the existing native_sim gates.

**What does not exist, and is the real cost:** Zephyr has **no MCUmgr transport
over CAN**. The shipped transports are serial, shell, BLE, UDP, LoRaWAN and the
dummies — there is no CAN. SMP over CAN means writing that transport, device
side, plus `crates/transport/src/can.rs` on the host.

That is why `transport` was made a separate crate from the beginning. The seam is
already there; the work is filling it.

### Status: working over `vcan`, 2026-08-30

The loop is closed. native_sim firmware on `zcan0` and the runtime on `vcan0`
exchange the full contract with no hardware involved:

```
$ cargo run -p runtt-smp --example can-ping -- vcan0 0x42
  can:vcan0/0x42
  echo -> "runtt"
  image list -> no images
  describe -> board: "native_sim/native/64", contract: "1.2.0", channels: 2
```

Host setup, once per machine:

```bash
sudo modprobe vcan can-isotp
sudo ip link add dev vcan0 type vcan
sudo ip link set vcan0 up                        # device first: `set up vcan0` parses badly
sudo ip link property add dev vcan0 altname zcan0 # native_sim hardcodes "zcan0"
```

### Status: the runtime deploys over CAN, 2026-08-30

`Resolved` is now an enum rather than a path pair, so `can:` labels go all the way
through. `scripts/native-sim-can-e2e.sh` is the serial gate's twin and differs from
it in the placement label and almost nothing else -- which is the transport seam
doing its job:

```
$ ./scripts/native-sim-can-e2e.sh
  ok: runtime resolved the can: placement label
  ok: runtime used the MCUboot image digest, not the file hash
  ok: uploaded over ISO-TP and marked test
  ok: slot 1 holds 8364 bytes starting with the MCUboot magic
  ok: MCUboot trailer written: device reports swap type 'test'
  ok: os reset re-execed the simulator, and it came back on the bus
```

It gates in CI alongside the existing native_sim jobs.

**Logs over CAN work too**, on a third id (`node_id + 2`) as raw frames rather
than ISO-TP -- ISO-TP waits for the receiver's flow control, and a log backend
that blocks deadlocks boot. The channel is lossy under backpressure and has no
backlog, both deliberately; see docs/WIRE_CONTRACT.md for what a consumer has to
accept. An explicit `dev.runtt.log-target` still overrides it, for a board
managed over the bus whose console comes back over a wire.

Two things cost real time here and are worth carrying forward:

* Under `CONFIG_LOG_MODE_IMMEDIATE` the log core hands a backend **one byte per
  call**. Emitting a frame per call meant one CAN frame per character, which
  overran the queue on the first line. The backend batches into full frames now.
* `can_send()` with `K_NO_WAIT` fails with `-EAGAIN` whenever no mailbox is free,
  and a mailbox frees only once the frame is on the wire -- so sending inline from
  the log path discarded nearly everything. A queue plus a sender thread moves the
  lossy boundary to somewhere that means what it says.

**Hardware:** two CAN boards are on order -- a Waveshare ESP32-S3 (TWAI, paired
with an SN65HVD230 we already have) and an Adafruit RP2040 CAN Feather (MCP25625,
controller and transceiver onboard). Deliberately two *different* controllers, so
the ISO-TP layer is proven controller-agnostic rather than merely intended to be.
See [HARDWARE_TARGETS.md](HARDWARE_TARGETS.md) for what each needs before it boots.

**Sequencing:** the transport crate is where this lands, so the repo split
clarifies its boundary -- but it did not need to wait, and a `vcan` gate in CI is
now a cheap and genuinely strong artefact.

---

## 3. Splitting the repo

The coupling is smaller than it looks. Contract knowledge in the runtime lives in
four files: `transport/usb.rs` (the two interface strings), `transport/resolve.rs`,
`smp-client/describe.rs`, and `runtt/annotations.rs`. **The wire contract is
the entire seam.**

| Repo | Contents | Consumers |
|---|---|---|
| `runtt` | `crates/`, `udev/` | anyone running `docker run`, podman, or a fleet manager |
| `runtt` (west module) | `firmware/runtt/` | every customer's `west.yml` |
| `runtt-boards` | `firmware/{idle,bringup,patches,builder}`, provisioning and flashing scripts | whoever provisions hardware |
| `runtt-examples` | `firmware/examples/`, the walkthrough | customers learning the workflow |

`runtt` is the one customers add to their manifest, so it should stay small,
stable and boring: module, snippet, board `.conf`/`.overlay`. Nothing else.

### Keeping the contract honest across repos

`contract_version.rs` currently guards runtime-against-firmware agreement only
because they share a tree. Split them and that test cannot exist as written.

1. **Version the contract explicitly.** `WIRE_CONTRACT.md` already carries 1.2.0
   and `describe` reports it; the runtime already refuses a mismatched major.
   Publish the document as the authority and have each repo assert a pinned
   version. Start here.
2. **A contract test repo** pulling both and cross-checking in CI. More faithful,
   more machinery.
3. **A shared `runtt-contract` crate** generating the module's headers.
   Cleanest, adds a publishing step.

Do (1) now, (3) only if drift actually bites. The failure mode is loud — a
mismatched contract is reported by `describe` and named by the runtime — not
silent.

**Do not split before CI is green.** Four repos multiply the never-executed
problem by four. Split `runtt` out first; it has the fewest inbound
dependencies.

---

## 4. The robotics demo

### The constraint to design around

`micro_ros_zephyr_module` officially supports **Zephyr up to 4.1**; we pin 4.4.2,
and the maintainer says so in
[issue #158](https://github.com/micro-ROS/micro_ros_zephyr_module/issues/158).
Its USB transport `select`s the **legacy** USB stack, which hard-conflicts with
the `device_next` stack we mandate. And the Zephyr-supported micro-ROS boards are
Cortex-M4 class, so the **Pico (M0+) is below the practical floor** while the
Feather is fine. See `docs/MICROROS.md` for the full evidence.

A two-device micro-ROS demo is therefore not a small step.

### What to build instead

Use the **log channel as the data path**. It exists, it already streams to
container stdio, and it works on both boards today.

```
Pico   ──USB──┐
              ├── runtt (one container each) ──► container stdout
Feather ─USB──┘                                             │
                                                            ▼
                                                  mcu-bridge container
                                            (reads stdio, publishes ROS 2 topics)
                                                            │
                                                            ▼
                                              ROS 2 container: rviz / rosbag
```

Each MCU emits line-delimited JSON telemetry on its log channel; a small bridge
container republishes it as ROS 2 topics; a stock ROS 2 container consumes them.

This is the right demo rather than a compromise. It shows the genuinely novel
part — firmware delivered and updated as container images, with data flowing into
a normal Linux container graph — on **both** boards, with no new firmware stack.
And it demos the update live: `docker run` a new firmware version and watch the
topic change while ROS keeps running.

### If micro-ROS proper is wanted later

Feather only. Cherry-pick PR #163's roughly thirty-line DT-alias hunk so
`UART_NODE` is not hardcoded to an STM32 nodelabel, point it at a third CDC-ACM
channel (nRF52840 has the endpoints: 6 IN / 3 OUT of 7 / 7), and run one agent
per MCU rather than `multiserial`.

**Design for this regardless of transport:** the agent never reaps clients, so an
unplugged board still reads as alive in `ros2 node list` indefinitely. Anything
safety-relevant needs its own staleness check.

---

## 5. Carried debt

| Item | State |
|---|---|
| MCUboot `find_last_idx` unbounded loop | patch written and carried via `west patch`; simulator green on both swap modes; **needs a regression test, then filing** |
| Revert on hardware | never tested on a real board — the one safety property still unproven |
| Hardware CI gate | designed in `docs/HARDWARE_GATE.md`, deliberately not built |
| Signing key | still MCUboot's published development key; no trust root is enrolled |
| Feather recovery | SWD-only now; the backup in `feather-backup/` is verified by checksum but **the restore has never been exercised** |
| udev hotplug monitor | unblocked (`pkg-config`, `libudev-dev` present), still polling |

The signing key is the one that must not reach a fleet. Rotating it means
re-flashing every board over SWD, so it is a provisioning decision, not a
build-time one.

---

## Suggested order

1. **Remote + CI green** — everything else compounds on this
2. **ESP32-S3** — cheap, proves the contract is not Pico-shaped
3. **Repo split** — `runtt` out first
4. **CAN over `vcan`** — no hardware needed; lands in the transport crate
5. **Robotics demo** — most visible, benefits from the split being done
6. **ESP32-C3** — first real validation of the single-channel demux

---

*Co-authored with Claude*
