# Roadmap

Where this is, and what is worth doing next. Written 2026-08-30 after the Pico
and the Feather were proven end to end; revised 2026-09-03 after the split, CI
and the confirm deadline.

## Where we are

Three boards, three SoC families, one runtime and one contract. On each of a
Raspberry Pi Pico (RP2040, Cortex-M0+), a Raspberry Pi Pico 2 W (RP2350,
Cortex-M33) and an Adafruit Feather nRF52840 (Cortex-M4),
`docker run --runtime=runtt` uploads firmware over USB, MCUboot swaps and
confirms it, the new image boots, and its logs reach container stdio. Digests
verified independently against imgtool in every case.

That three different architectures pass the same contract is the part that makes
the contract credible rather than shaped around one board. The Pico 2 W is the
strongest evidence of that: it needed no new snippet files at all.

**Done since this was written:** the four repositories exist and every one has
CI that runs and is green — so "it works" no longer means "it works on one
laptop". `runtt` v0.1.1 and `runtt-boards` v0.2.0 publish release artefacts.
Revert is proven on hardware, and the reboot that it needs is now scheduled by
the device itself (§6).

**Not yet done:** the upstream MCUboot report is drafted but unfiled, the
`mcumgr-toolkit` fork is still in place so crates.io is closed, and no trust
root is enrolled (§5).

---

## 0. Push to a remote, and get CI green — DONE

**Done 2026-09-02.** All four repositories are on GitHub and all four have CI
that runs and is green. Kept here because the reasoning is what the outcome
vindicated: every artefact named below turned out to be broken, and CI is what
found it. Three CI faults surfaced on the runtime's first real run alone.

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
the board section's job — the same shape as the three boards already supported.

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

### Phase B — the split — DONE

**Done 2026-09-02**, in the order below and with history preserved. The one
prediction that did not survive contact is worth recording: this plan had
`runtt-boards` publishing `native_sim` fixtures as release assets for `runtt`'s
CI to download against a pinned tag, and named the resulting two-repo dance as
"the real price of splitting". `runtt` builds the fixtures itself from
`runtt-boards`' manifest instead, so that price was never paid.


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

### Phase C — CI, per repo — DONE

**Done 2026-09-02.** Every repository below has the jobs described, and the
tag-gated release paths have both been exercised: `runtt` v0.1.1 publishes three
static binaries plus `SHA256SUMS`, and `runtt-boards` v0.2.0 publishes
provisioning images for all three boards.


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
container stdio, and it works on all three boards today.

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
| Revert on hardware | **proven on a Pico 2 W** twice, and the reboot it needs is now scheduled on-device by `CONFIG_RUNTT_CONFIRM_DEADLINE` — 2026-09-03, tested in both directions. §6. Layers 2 and 3 remain open, so an image that faults before the module starts still does not revert |
| Hardware CI gate | designed in `docs/HARDWARE_GATE.md`, deliberately not built |
| Signing key | still MCUboot's published development key; no trust root is enrolled |
| Feather recovery | SWD-only now; the backup in `feather-backup/` is verified by checksum but **the restore has never been exercised** |
| udev hotplug monitor | unblocked (`pkg-config`, `libudev-dev` present), still polling |

The signing key is the one that must not reach a fleet. Rotating it means
re-flashing every board over SWD, so it is a provisioning decision, not a
build-time one.

---

## 6. Revert needs a reboot — Layer 1 has landed

**Layer 1 done and measured on hardware, 2026-09-03. Layers 2 and 3 still
open.** The decision this section used to leave open has been made: the deadline
defaults **on**, at **60 seconds**, tunable.

### What was actually happening

`crates/runtt/src/flash.rs` states the safety property, and the last line of it
was the problem: an unconfirmed image reverts on the next reset, and **nothing
scheduled that reset**.

Reproduced twice on a Pico 2 W, the second time deliberately to rule out a
fluke. An image built with `CONFIG_MCUMGR_TRANSPORT_UART=n` boots and runs
perfectly well — USB enumerates, both contract channels register, the
application ticks — it simply cannot be talked to, so the host can never
confirm it:

```
uptime 00:14:28, tick 434, boot banners: 1, SMP: silent throughout
```

Fourteen minutes, no self-recovery, and it reverted correctly the instant it was
reset over SWD. So the property held — eventually, and only because a human
intervened.

The container exiting non-zero does not close this. The restart policy fires,
the runtime comes back, and it cannot upload anything, because the board is
unmanageable — which is the whole problem.

### This is not a gap peculiar to us

Worth writing down, because it shapes the fix.

**MCUboot puts the scheduling out of scope, explicitly.** Its
[design document](https://docs.mcuboot.com/design.html) says a revert happens
"during the next boot" and that "the new image can then update the contents of
flash at runtime to mark itself OK". It describes only the decision made at
startup from the trailer flags. No watchdog, no deadline, no mention of what
causes that boot.

**Zephyr's own OTA clients hit the same wall and stop there.** In our pinned
v4.4.2: hawkbit logs `"Current image is not confirmed"` and terminates with
`HAWKBIT_UNCONFIRMED_IMAGE` (`subsys/mgmt/hawkbit/hawkbit.c`); updatehub returns
`-EIO`. Neither reboots. hawkbit also offers
`CONFIG_HAWKBIT_CONFIRM_IMG_ON_INIT`, which confirms at init — that is the
anti-pattern, since it makes every image permanent before anything is proven.
Zephyr assigns the duty in its own Kconfig help for MCUmgr's image group:
"instead application should have a means to test and confirm the image."

**The documented idiom does not fit us, and that is the interesting part.** The
standard pattern is a *local* self-test: boot, check yourself, then either
`boot_write_img_confirmed()` or `sys_reboot()`. That works when the verdict is
local. runtt's verdict is **remote by design** — confirmation travels over the
very contract being tested, which is exactly what makes contract loss
unrecoverable-proof. No self-test can produce it: the firmware cannot know it is
broken, only that nobody has confirmed it. Hence a deadline rather than a
self-test.

### Layer 1 — the confirm deadline (done)

`runtt-zephyr-module`, `src/confirm.c`, `CONFIG_RUNTT_CONFIRM_DEADLINE`. At boot,
if the running image is unconfirmed it arms delayed work; at the deadline it
re-checks and `sys_reboot(SYS_REBOOT_COLD)`s if still unconfirmed. MCUboot
reverts on that boot.

**Default on**, and the cost this section previously ascribed to that was
overstated. It is not "a reboot timer on every board": the deadline is armed
only while the running image is unconfirmed, which is only the window between a
fresh deploy and its confirmation. A board on settled firmware arms nothing.

**Default 60 s.** The footgun points the opposite way to intuition — too
**short** reverts **good** firmware. Measured confirm latency is ~2 s, but it is
bounded by the host rather than the device, and a loaded machine or slow
container start is unbounded in practice. 60 s is ~30x the measurement.

Two guards, both load-bearing:

* **Arms only when `mcuboot_swap_type() == BOOT_SWAP_TYPE_REVERT`** — only when
  a reboot would actually revert something. Without this it is a bootloop
  generator: an image flashed straight into the primary slot without
  `--confirm` boots unconfirmed too, but with an empty secondary there is
  nothing to revert to, so it would reboot into itself forever. Declining to
  arm leaves that board running and manageable, which is strictly better.
* **Off on `ARCH_POSIX`.** `MCUBOOT_IMG_MANAGER` is set on native_sim as well,
  because `RUNTT_SIM_SLOT_SHIM` makes `img_mgmt` buildable there — so without
  this the deadline ships inside the native_sim fixture this repository's e2e
  gates consume, where there is no MCUboot to revert to and `sys_reboot()`
  re-execs the process. Its only possible effect there is restarting the
  simulator underneath a running gate. Found by reading the generated
  `.config`; the assumption going in was that native_sim would not have the
  symbol.

It lives in its own file and its own Kconfig symbol rather than in `health.c`,
as this section used to suggest: `RUNTT_HEALTH` defaults `n`, so that placement
would have shipped the feature disabled.

**Tested in both directions on a Pico 2 W**, which was the requirement this
section set and the second half of which is the part that is easy not to write:

| | broken image | good image |
|---|---|---|
| boot banners | 2 — self-rebooted | **1** — never rebooted |
| deadline fired | yes, ~60 s | **no** |
| outcome | reverted; manageable **75 s** after deploy start | ran to 2:30 uptime, ticks unbroken through the 60 s mark |
| final slot 0 | previous image, confirmed | new image, confirmed |

Against 14 minutes and an operator with a debug probe, before it existed.

### Layer 2 — a hardware watchdog (open)

Layer 1 cannot help firmware that faults before the module initialises, or
firmware that does not include the module at all. A WDT armed early and fed only
while the contract is up covers both, and it is what makes this property hold
for *arbitrary* firmware rather than only ours. RP2350 has `wdt0`
(`raspberrypi,pico-watchdog`, driver `wdt_rpi_pico.c`) and the nRF52840 has one
too.

The crash case is worth restating, because it is easy to assume otherwise:
Zephyr's default fatal handler **halts** (`arch_system_halt`, `kernel/fatal.c`),
it does not reboot. An image that hard faults therefore does not revert either,
and Layer 1 does not change that.

Note what does **not** do this: MCUboot's `CONFIG_BOOT_WATCHDOG_FEED` is
literally "Feed the watchdog while doing swap" — verified in
`bootloader/mcuboot/boot/zephyr/Kconfig`. It feeds an already-running watchdog
during long operations and does not arm one for the application, so this is
separate per-SoC work.

Zephyr does ship a building block: `subsys/task_wdt`, a task-level software
watchdog that `select`s `REBOOT`.

### Layer 3 — host-side tidy-up (open)

On the next deploy, if the device answers SMP with an unconfirmed image pending,
reset it rather than proceeding. Cheap, but it only helps cases that were
already recoverable, so it is the lowest-value of the three.


## Suggested order

Revised 2026-09-03. Struck because they are done: CAN over `vcan` and the CAN
transport in `runtt-transport`; the fork swap and deleting `third_party/`; the
four-repo split; CI per repo; and the confirm deadline (§6, Layer 1).

1. **File the MCUboot patch** — the regression test injecting a corrupt trailer
   comes first, then the issue, then its URL into `patches.yml` as `issue:`.
   `mcumgr-toolkit` is with review; dropping that fork is the prerequisite for
   any crates.io release.
2. **Layer 2, the hardware watchdog** (§6) — what makes the safety property
   hold for firmware that is not ours, rather than only for firmware carrying
   the module.
3. **Exercise the Feather restore** (§5). The backup is verified by checksum and
   the restore has never been run, which means it is not yet a backup.
4. **ESP32-S3** — cheap, proves the contract is not Pico-shaped, and the board
   is on order along with the two CAN boards.
5. **CAN on physical hardware** — two different controllers, which is what makes
   the ISO-TP layer proven rather than merely intended.
6. **Robotics demo** — most visible, and benefits from the split being done.

Note what moved off the list entirely: "get a remote and make CI green" was the
single highest-leverage act for months, on the grounds that nothing here had
executed anywhere but one laptop. That is now true of nothing.

---

*Co-authored with Claude*
