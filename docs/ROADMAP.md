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
  describe -> board: "native_sim/native/64", contract: "2.0.0", channels: 2
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

## 3. Upstream, then split, then CI

In that order, and the order matters. An earlier version of this document said
**"do not split before CI is green"**, reasoning that four repos multiply the
never-executed-CI problem by four. That was wrong, and it is worth saying why
rather than quietly deleting it: the argument assumed there was working CI to
replicate. There is not — `ci.yml` has never run anywhere. Nothing is lost by
splitting first, and the CI is *genuinely different per repo*, so building one
monolithic pipeline and then splitting it means writing it twice.

### Phase A — get the two upstream patches out first

Both are carried locally today. They get **different treatment**, and conflating
them would be a mistake.

**`mcumgr-toolkit` → done.** Submitted as
[Finomnis/mcumgr-toolkit#186](https://github.com/Finomnis/mcumgr-toolkit/pull/186),
and this repo now builds against the fork rather than a vendored tree — 436 KB and
32 files lighter. See [FORKED_DEPENDENCY.md](FORKED_DEPENDENCY.md) for the state
of it and the steps to drop the fork when a release contains the patch.

⚠️ **A git dependency cannot be published to crates.io**, so while the fork is in
place `cargo publish` is closed. Not urgent, but it means dropping the fork is a
prerequisite for any crates.io release rather than an optional tidy-up.

**MCUboot → submit the patch, do NOT fork.** It rides `west patch` in
`firmware/patches.yml` today: explicit, pinned by sha256, visible in the tree, and
re-applied against a known upstream revision. A fork would be an entire vendor
tree to keep rebased for the sake of a few lines. `west patch` is the better
mechanism here and should stay even after the patch is filed. Still pending before
filing: a regression test in MCUboot's own simulator, noted in
[MCUBOOT_SWAP_BUG.md](MCUBOOT_SWAP_BUG.md).

### Phase B — the split

Four repos, listed in the order to extract them — fewest inbound dependencies
first, so each extraction can be proven before the next.

| # | Repo | Contents | Artefacts |
|---|---|---|---|
| 1 | `runtt` | `crates/`, `udev/`, `register-docker.sh`, the wire contract | three static binaries |
| 2 | `runtt-zephyr-module` | `firmware/runtt/` only — module, snippet, board conf/overlay | none |
| 3 | `runtt-boards` | `firmware/{idle,bringup,builder,patches.yml,west.yml}`, provisioning and flashing scripts | provisioning images per board, plus `native_sim` test fixtures |
| 4 | `runtt-examples` | `firmware/examples/`, the walkthrough | none |

`runtt-zephyr-module` is the one a firmware author adds to their `west.yml`, so it should
stay small, stable and boring. Module, snippet, board files. Nothing else.

**Extract with history, not with `cp`.** `git subtree split` or `git filter-repo`,
so `git blame` survives. That matters more here than in most projects: the commit
messages carry the reasoning for decisions that are not obvious from the code, and
a fresh `git init` throws all of it away.

### The hard part: the gates span the split

This is the part that will actually cost time, and it is worth planning before
moving a single file. **The local gates are the only thing that has ever verified
this project** — CI has never run — so a split that breaks them trades a working
safety net for three untested pipelines.

| Gate | Needs | After the split |
|---|---|---|
| `contract_version.rs` | the doc, the firmware Kconfig, the runtime's major, the mock | loses one input: the Kconfig moves to `runtt-zephyr-module` |
| `native-sim-e2e.sh` | built `native_sim` firmware + the runtime binary | firmware source moves away |
| `native-sim-can-e2e.sh` | as above, plus the CAN module and a `vcan` bus | as above |
| `native-sim-engine-e2e.sh` | as above, plus podman | as above |

**The answer for the e2e gates: `runtt-boards` publishes signed `native_sim`
binaries as release assets, and `runtt`'s CI downloads a pinned release.** The
runtime's tests need *a firmware binary*, not firmware source — and this is the
same machinery `runtt-boards` needs anyway for provisioning images, so it is not
extra work.

Pin the fixture release explicitly rather than tracking `latest`. Otherwise a
change in `runtt-boards` can break `runtt`'s CI without anyone choosing to, which
is the classic way a multi-repo split becomes miserable. A contract change then
needs a deliberate two-repo dance: publish the new fixtures, bump the pin, land
both. That coordination cost is the real price of splitting, and naming it up
front is cheaper than discovering it.

**The answer for `contract_version.rs`: split it in two.** `runtt` keeps checking
that the document, the runtime's accepted major and the mock agree.
`runtt-zephyr-module` gains a small test asserting its Kconfig default matches the
contract version it pins. Both are cheap, and together they cover what the single
test covers today.

### Phase C — CI, per repo

| Repo | Jobs |
|---|---|
| `runtt` | `cargo test` + clippy on x86; cross-build three targets; download pinned firmware fixtures and run all three gates; publish binaries on tag |
| `runtt-zephyr-module` | build against every supported board (matrix); the Kconfig contract assertion |
| `runtt-boards` | Zephyr SDK; matrix over every supported board; publish provisioning images and `native_sim` fixtures on tag |
| `runtt-examples` | build both examples against the pinned module |

**Release artefacts for `runtt` — verified, not assumed.** All three targets
cross-compile with **no external toolchain**: `rustup target add` plus
`RUSTFLAGS="-C linker=rust-lld"`. No `cross`, no Docker-in-Docker, no apt
packages. This works because `serialport` is configured `default-features =
false`, so nothing links libudev and the binaries are fully static:

| Target | Size | Type |
|---|---|---|
| `x86_64-unknown-linux-musl` | 4.8 MB | static-pie |
| `aarch64-unknown-linux-musl` | 3.9 MB | static |
| `armv7-unknown-linux-musleabihf` | 3.5 MB | static |

Measured with the `strip = "symbols"` release profile. Strip **in the compiler**,
not with binutils afterwards: the host `strip` silently fails to strip ARM
binaries and leaves the symbols in place, which is how a 6.4 MB "stripped"
aarch64 binary came to be measured during this work.

The x86_64 static binary was run against the full `native-sim-e2e.sh` gate and
passes, so these are working artefacts rather than merely things that link.

**`runtt-boards` CI is the heavy one.** The builder image is ~33 GB, so the choice
is between publishing it to a registry (GHCR, built on a schedule) and installing
the Zephyr SDK per job (~10 minutes each, times the board matrix). Prefer the
prebuilt image; the per-job install multiplied across 5+ boards is the difference
between a usable pipeline and one nobody waits for.

### The one rule while splitting

**Keep the monorepo's gates green until each extracted repo's replacement is
green.** Do not delete anything from here until the new home proves itself. The
extraction is reversible right up to the point of deletion, and irreversible
after.

---

## 3b. `runtt-provision` as a Rust binary

`runtt-boards`' `scripts/runtt-board` provisions a board from a single downloaded
file today: it fetches the published image, writes a name into it and flashes it,
needing no checkout and no Zephyr toolchain. It reads `boards.json` with the
stdlib so it does not even need pyyaml.

**On Linux only**, and the reason is worth stating precisely: *the language is not
what blocks cross-platform, the shell-outs are.* It calls `pyocd`, `udisksctl`,
`findmnt`, `sha256sum` and `sync`. `udisksctl` and `findmnt` are Linux-only and
`pyocd` needs Python, so a mechanical Rust port that still shelled out to those
would be **no more portable than the Python it replaced**.

A rewrite earns its keep only by replacing them:

| Shell-out | Replacement |
|---|---|
| `pyocd` | **`probe-rs` as a library** — 0.32.0, published with `has_lib: true`, CMSIS-DAP support, and it implements the nRF52840 CTRL-AP unlock we depend on (see runtt-boards' PROVISIONING.md) |
| `sha256sum` | the `sha2` crate, already a dependency here |
| `udisksctl` + `findmnt` | native volume discovery — `/dev/disk/by-label` on Linux, `/Volumes/RPI-RP2` on macOS, drive letters on Windows. **This is the actual portability work** |
| `pyyaml` | `serde_yaml`, though `boards.json` already removes that need |

**Where it goes:** a second crate in this workspace, released alongside the
runtime. The musl cross-build machinery is already proven — 3.5–4.8 MB static
binaries for x86_64, aarch64 and armv7 with no external toolchain — and Windows
and macOS are standard additional targets. It should fetch `boards.json` at
runtime exactly as the Python does, so the binary stays board-agnostic and the
manifest stays in `runtt-boards` where contributors add boards.

**When:** once a non-Linux user actually appears, or when pyocd becomes the thing
that hurts. Not before: the Python works, covers the hardware on the bench, and a
port done for its own sake would be effort spent for no gain. Start with the SWD
path if it does happen, since that is where `pyocd` costs the most.

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
| Revert on hardware | **proven on a Pico 2 W, 2026-09-02** — a deliberately unmanageable image failed to confirm and MCUboot reverted. But see §6: the revert needed a reboot that nothing schedules |
| Hardware CI gate | designed in `docs/HARDWARE_GATE.md`, deliberately not built |
| Signing key | still MCUboot's published development key; no trust root is enrolled |
| Feather recovery | SWD-only now; the backup in `feather-backup/` is verified by checksum but **the restore has never been exercised** |
| udev hotplug monitor | unblocked (`pkg-config`, `libudev-dev` present), still polling |

The signing key is the one that must not reach a fleet. Rotating it means
re-flashing every board over SWD, so it is a provisioning decision, not a
build-time one.

---

## 6. Revert needs a reboot, and nothing schedules it

**Parked deliberately, 2026-09-02.** The gap is understood and recorded rather
than fixed, because the fix changes device behaviour on every board and the
default wants a decision, not a guess.

### What actually happens

`crates/runtt/src/flash.rs` states the safety property, and the last line of it
is the problem:

> If we never send the confirm, MCUboot reverts on the next reset by itself.

True about what MCUboot does **at boot**. But nothing in the system schedules
that boot. Observed end to end on a Pico 2 W:

1. an image with `CONFIG_MCUMGR_TRANSPORT_UART=n` was uploaded and marked test —
   it boots and runs perfectly well, it simply cannot be talked to
2. MCUboot swapped it in and booted it as a test image
3. the host could not confirm it, and said so
4. **the board went on running the unmanageable image indefinitely**
5. only an operator-issued reset made MCUboot revert, at which point it did so
   correctly and the previous image came back confirmed

So the property holds — eventually, and only if somebody resets the board. The
container exiting non-zero and the restart policy firing does not help: the
runtime comes back up and cannot upload anything, because the board is
unmanageable. That is the loop nothing breaks.

The crash case has the same shape and is worth stating separately, because it is
easy to assume otherwise: Zephyr's default fatal handler **halts**
(`arch_system_halt`, `kernel/fatal.c`), it does not reboot. An image that hard
faults therefore also never reverts.

### The fix, in three layers

**Layer 1 — a confirm deadline in the runtt module.** At boot, if
`boot_is_img_confirmed()` is false, arm delayed work for N seconds; at the
deadline, re-check and `sys_reboot()` if still unconfirmed. MCUboot reverts on
that boot. Everything needed already exists — `boot_is_img_confirmed()` and
`mcuboot_swap_type()` are in `zephyr/dfu/mcuboot.h`, and
`CONFIG_MCUBOOT_IMG_MANAGER=y` is already set on every MCUboot target we build.
Roughly 40 lines in `src/health.c`.

This closes the observed case: firmware that runs but cannot be confirmed.

> **The footgun, and it points the wrong way.** A deadline shorter than the
> host's realistic confirm time reverts *good* firmware — it would turn the
> safety property into a liability. Measured confirm latency is ~2 s, but a
> loaded host or a slow container start is unbounded in practice, so the default
> wants to be generous (~2 minutes) and Kconfig-tunable. Any implementation must
> be tested in BOTH directions on hardware: a broken image self-reverts, **and a
> good image survives the deadline untouched.** The second test is the one that
> matters, and the one that is easy not to write.

**Layer 2 — a hardware watchdog.** Layer 1 cannot help firmware that faults
before the module initialises, or firmware that does not include the module at
all. A WDT armed early and fed only while the contract is up covers both, and it
is what makes this property hold for *arbitrary* firmware rather than only ours.
RP2350 has `wdt0` (`raspberrypi,pico-watchdog`, driver `wdt_rpi_pico.c`) and the
nRF52840 has one too.

Note what does **not** do this: MCUboot's `CONFIG_BOOT_WATCHDOG_FEED` only feeds
a watchdog *while doing the swap* (`bootloader/mcuboot/boot/zephyr/Kconfig`). It
does not arm one for the application, so this is separate per-SoC work.

**Layer 3 — host-side tidy-up.** On the next deploy, if the device answers SMP
with an unconfirmed image pending, reset it rather than proceeding. Cheap, but it
only helps cases that were already recoverable.

### The open decision

Layer 1 defaulting **on** changes device behaviour for the Feather and Pico 1 as
well, and it is a self-reboot path, so the default is a product decision rather
than an implementation one:

* **default on** — the safety property actually holds everywhere, at the cost of
  a reboot timer on every board;
* **default off, boards opt in** — no behaviour change, but the property stays
  unenforced by default, which rather undercuts the point.

Until this lands, `flash.rs`'s module comment has been corrected so it no longer
reads as though recovery were automatic.

---

## Suggested order

Revised, and two items are struck because they are done: CAN over `vcan` works
and gates locally, and the CAN transport landed in `runtt-transport`.

1. **Submit both upstream patches** — `mcumgr-toolkit` by fork, MCUboot by
   `west patch`. Doing this first means the split does not have to carry a
   vendored tree into a new repo.
2. **Point this repo at the `mcumgr-toolkit` fork** and delete `third_party/`.
3. **Split, one repo at a time**, `runtt` first — with the monorepo's gates kept
   green until each new repo's replacement is green.
4. **CI per repo**, including the cross-compiled release matrix and the
   `native_sim` fixture publishing that the runtime's gates depend on.
5. **ESP32-S3** — cheap, proves the contract is not Pico-shaped, and the board is
   on order along with the two CAN boards.
6. **CAN on physical hardware** — two different controllers, which is what makes
   the ISO-TP layer proven rather than merely intended.
7. **Robotics demo** — most visible, and benefits from the split being done.

Note what moved: "remote + CI green" is no longer first on its own, because the
split changes what CI should even be. Getting a remote is still the single
highest-leverage act — nothing here has ever executed anywhere but one laptop —
but it now belongs *with* the split rather than before it.

---

*Co-authored with Claude*
