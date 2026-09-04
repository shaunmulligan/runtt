# Development notes

**For agents and maintainers, not for users.** How the current state was
reached: investigations, failures worth not repeating, by-hand procedures, and
decisions with their reasoning. Nothing here is required to *use* runtt — the
user-facing surface is the README, `docs/ARCHITECTURE.md`,
`docs/WIRE_CONTRACT.md` and `docs/OCI_COMPLIANCE.md`.

Content is preserved verbatim from the documents it was gathered from; the
original paths are noted per section so `git log --follow` still reaches their
history.


---

# Roadmap, and how the current state was reached

> Was `docs/ROADMAP.md`.

Where this is, and what is worth doing next. Written 2026-08-30 after the Pico
and the Feather were proven end to end; revised 2026-09-03 after the split, CI
and the confirm deadline.

### Where we are

Four boards, three SoC families, one runtime and one contract. On each of a
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

### 0. Push to a remote, and get CI green — DONE

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

### 1. ESP32 — a third target

Both options verified against our pinned Zephyr v4.4.2 tree.

#### ESP32-S3 — the direct port

`esp32s3_common.dtsi` declares `usb_otg@60080000`, compatible
`"espressif,esp32-usb-otg", "snps,dwc2"`, and `udc_dwc2_vendor_quirks.h`
already carries ESP32 quirks. So the S3 has a real USB device controller on the
same `device_next` stack we use, and the dual-CDC contract ports directly.

Board `esp32s3_devkitc`. The node ships `status = "disabled"`, so enabling it is
the board section's job — the same shape as the three boards already supported.

#### ESP32-C3 — the single-channel proof

No USB-OTG. Instead the built-in USB Serial/JTAG peripheral, driven by
`serial_esp32_usb.c`, which presents exactly **one** fixed CDC channel.

That is `CONFIG_RUNTT_CHANNELS=1` plus the log demux — and the module's own
Kconfig help already names "ESP32-C3 class" as the reason that option exists. It
would be the first real hardware validation of the demux, which today is proven
on native_sim, the mock, and only incidentally on hardware.

Board `esp32c3_devkitm`, about five pounds.

#### What to expect to fight

MCUboot's Kconfig has ESP32-specific carve-outs — `BOOT_PREFER_SWAP_OFFSET` is
`default y if … && !SOC_FAMILY_ESPRESSIF_ESP32`, so ESP32 gets a different swap
mode by default. Given a mismatched swap assumption cost a full day on RP2040,
**pin the swap mode explicitly and verify it rather than inheriting a default.**

No Espressif board in Zephyr currently declares a `cdc-acm` node, so we would be
first there too.

**Order:** S3 first (proves portability), C3 second (proves single-channel). Skip
the classic ESP32 — no USB at all, so after the Feather it validates nothing new.

---

### 2. CAN — and it needs no hardware to start

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

#### Status: working over `vcan`, 2026-08-30

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

#### Status: the runtime deploys over CAN, 2026-08-30

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

**Hardware: half done, 2026-09-04.** The Adafruit RP2040 CAN Bus Feather
(MCP25625) arrived and is a supported board: deploy, logs and revert all proven
over the physical bus at 500 kbit/s, host side a BTT U2C (gs_usb), including a
deploy addressed by the board's identity record (`can:can0/0x45`) and a revert
recovered over the bus in 78 s. The Waveshare ESP32-S3 (TWAI, paired with an
SN65HVD230 already on hand) is still on order -- and still matters, because two
*different* controllers is what makes the ISO-TP layer proven controller-agnostic
rather than merely intended to be. Bring-up notes live in
[runtt-boards' NOTES.md](https://github.com/shaunmulligan/runtt-boards/blob/main/NOTES.md).

**Sequencing:** the transport crate is where this lands, so the repo split
clarifies its boundary -- but it did not need to wait, and a `vcan` gate in CI is
now a cheap and genuinely strong artefact.

---

### 3. Upstream, then split, then CI

In that order, and the order matters. An earlier version of this document said
**"do not split before CI is green"**, reasoning that four repos multiply the
never-executed-CI problem by four. That was wrong, and it is worth saying why
rather than quietly deleting it: the argument assumed there was working CI to
replicate. There is not — `ci.yml` has never run anywhere. Nothing is lost by
splitting first, and the CI is *genuinely different per repo*, so building one
monolithic pipeline and then splitting it means writing it twice.

#### Phase A — get the two upstream patches out first

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
[MCUBOOT_SWAP_BUG.md](https://github.com/shaunmulligan/runtt-boards/blob/main/NOTES.md).

#### Phase B — the split — DONE

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

#### The hard part: the gates span the split

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

#### Phase C — CI, per repo — DONE

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

#### The one rule while splitting

**Keep the monorepo's gates green until each extracted repo's replacement is
green.** Do not delete anything from here until the new home proves itself. The
extraction is reversible right up to the point of deletion, and irreversible
after.

---

### 3b. `runtt-provision` as a Rust binary

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

### 4. The robotics demo

#### The constraint to design around

`micro_ros_zephyr_module` officially supports **Zephyr up to 4.1**; we pin 4.4.2,
and the maintainer says so in
[issue #158](https://github.com/micro-ROS/micro_ros_zephyr_module/issues/158).
Its USB transport `select`s the **legacy** USB stack, which hard-conflicts with
the `device_next` stack we mandate. And the Zephyr-supported micro-ROS boards are
Cortex-M4 class, so the **Pico (M0+) is below the practical floor** while the
Feather is fine. See `docs/MICROROS.md` for the full evidence.

A two-device micro-ROS demo is therefore not a small step.

#### What to build instead

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

#### If micro-ROS proper is wanted later

Feather only. Cherry-pick PR #163's roughly thirty-line DT-alias hunk so
`UART_NODE` is not hardcoded to an STM32 nodelabel, point it at a third CDC-ACM
channel (nRF52840 has the endpoints: 6 IN / 3 OUT of 7 / 7), and run one agent
per MCU rather than `multiserial`.

**Design for this regardless of transport:** the agent never reaps clients, so an
unplugged board still reads as alive in `ros2 node list` indefinitely. Anything
safety-relevant needs its own staleness check.

---

### 5. Carried debt

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

### 6. Revert needs a reboot — Layer 1 has landed

**Layer 1 done and measured on hardware, 2026-09-03. Layers 2 and 3 still
open.** The decision this section used to leave open has been made: the deadline
defaults **on**, at **60 seconds**, tunable.

#### What was actually happening

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

#### This is not a gap peculiar to us

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

#### Layer 1 — the confirm deadline (done)

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

#### Layer 2 — a hardware watchdog (done, 2026-09-04)

`CONFIG_RUNTT_WATCHDOG` in runtt-zephyr-module: arms the SoC watchdog at init,
feeds it from the system workqueue, and — because the feed consults
`runtt_health_ok()` — an application that opts into `runtt_health_feed()` gets
"application alive" rather than "kernel alive". Tested on hardware in both
directions on both SoC families: a `k_panic()`ed image reboots and reverts
(RESETREAS/REASON read TIMER, from registers cleared immediately beforehand),
and the same image without the watchdog halts permanently.

What building it surfaced, and the design that came out:

* **The watchdog survives a soft reset on both RP2350 and nRF52840**, so an
  app-armed watchdog imposes its remaining period on MCUboot's swap and the next
  image's startup. On nRF52840 MCUboot's `BOOT_WATCHDOG_FEED` handles it, on by
  upstream default — verified, an 8 s watchdog through an 82 KB swap. On RP2 that
  path is inert (Zephyr's driver refuses to feed a watchdog its instance did not
  arm), so the module stops the watchdog on MCUmgr's os-reset hook instead
  (`RUNTT_WATCHDOG_DISARM_ON_RESET`). Verified on a Pico 2 W: a watchdog-free
  image deployed over a watchdog-armed one boots with `ENABLE=0` and the
  inherited countdown frozen — where without the hook the same sequence was
  reset 5.5 s after boot.
* The prediction below that MCUboot's `BOOT_WATCHDOG_FEED` "does not arm one for
  the application, so this is separate per-SoC work" was half right: upstream
  also ships `BOOT_WATCHDOG_SETUP_AT_BOOT`, which does arm one. Deliberately not
  used — it would bootloop every application that does not feed it.

Still uncovered, deliberately: firmware that does not carry the module, and a
fault before the module initialises. Covering those means MCUboot arming a
watchdog for arbitrary firmware, which is the bootloop above. The full findings,
including two wrong turns and how they were caught, are in
[runtt-zephyr-module's NOTES.md](https://github.com/shaunmulligan/runtt-zephyr-module/blob/main/NOTES.md).

#### Layer 3 — host-side tidy-up (open)

On the next deploy, if the device answers SMP with an unconfirmed image pending,
reset it rather than proceeding. Cheap, but it only helps cases that were
already recoverable, so it is the lowest-value of the three.


### Suggested order

Revised 2026-09-03. Struck because they are done: CAN over `vcan` and the CAN
transport in `runtt-transport`; the fork swap and deleting `third_party/`; the
four-repo split; CI per repo; the confirm deadline (§6, Layer 1); and the
hardware watchdog (§6, Layer 2), verified on both SoC families.

1. **File the MCUboot patch** — the regression test injecting a corrupt trailer
   comes first, then the issue, then its URL into `patches.yml` as `issue:`.
   `mcumgr-toolkit` is with review; dropping that fork is the prerequisite for
   any crates.io release.
2. **The Feather restore, reframed** (§5). There is no restore command — only a
   procedure at the end of `backup-nrf52840.sh` — and the backup captures the
   *provisioned* state, not the factory state, so "exercise the restore" is
   really "add `runtt-board restore`, and decide whether a factory image is
   worth keeping at all".
3. **ESP32-S3** — proves the contract is not Pico-shaped AND closes the second
   half of the CAN controller-agnostic claim (on-die TWAI against the CAN
   Feather's MCP25625, which is done). The board is still on order.
4. **Robotics demo** — most visible, and benefits from the split being done.

Struck 2026-09-04: **CAN on physical hardware** — deploy, logs, revert and an
identity-addressed placement all proven on the CAN Feather's MCP25625 through a
real bus. One controller family of the two, hence ESP32-S3 above.

Note what moved off the list entirely: "get a remote and make CI green" was the
single highest-leverage act for months, on the grounds that nothing here had
executed anywhere but one laptop. That is now true of nothing.



---

# The hardware CI gate: designed, deliberately not built

> Was `docs/HARDWARE_GATE.md`.

**Status: not built. Deliberately.** CI is simulated-only, and that is a
considered choice rather than a gap we haven't got to.

This document exists so that whoever builds the hardware gate doesn't rediscover
the traps from scratch. Everything below was verified against this repo and a
Pico on the bench; where something is inferred rather than run, it says so.

### What CI covers today, without any hardware

`.github/workflows/ci.yml`, three jobs, all on hosted runners:

| Job | Proves |
|---|---|
| `test` | fmt, clippy, the Rust suite, and a podman run of a real `FROM scratch` image against `runtt-mock` over a `tty:` target — including that **mark-test precedes confirm**, asserted by line order, not just by both appearing |
| `native-sim` | the real Zephyr SMP server, two channels, upload, trailer write, reset, reconnect — driven directly *and* through podman |
| `mcuboot-sim` | swap, revert and confirm under **injected power failures**, using MCUboot's own Rust simulator over the real `bootutil` sources |

That is a lot of the risk surface, and none of it needs a board. The gap is
genuinely narrow — but it is not empty.

> **Note:** this used to warn that `ci.yml` had never executed anywhere, and
> that a hardware job was really a three-step plan — push to a remote, get the
> headless jobs green, then attach a runner to a desk machine. The first two
> steps are done: CI runs on every push and is green, and tagged releases
> publish. So what remains here really is just the last step, and the argument
> below for not taking it stands on its own merits rather than on CI being
> unproven.

### What only hardware can prove

* USB enumeration, and interface-string-descriptor matching — the board
  enumerates as `2fe3:0004 NordicSemiconductor runtt device`, an RP2040
  claiming to be Nordic on Zephyr's VID. Nothing about that is findable by
  VID/PID; the descriptor is the whole identity.
* Port-path → target label resolution (`usb:3-4`).
* Real MCUboot swap on RP2040 over QSPI, and its timing.
* Actual link throughput, and whether SMP timeouts hold at real speed.
* Replug and device-disappears-mid-operation behaviour.
* **Revert.** Asserted nowhere in this repo today: `DigestAlreadyFailed` is
  exercised only inside `runtt-mock`'s own unit test, never through the runtime.

### The trap that makes a naive gate worthless

**A hardware gate is not idempotent, and the obvious version silently no-ops
from the second run onwards.**

`skip_if_same` defaults to **true** (`crates/runtt/src/verbs.rs:96`,
`.unwrap_or(true)`), and rebuilding from an unchanged tree produces a
byte-identical `zephyr.signed.bin`. So `crates/runtt/src/flash.rs:102-109`
takes the early return:

```
mcu: device already runs this digest, confirmed; nothing to do
```

No upload, no mark-test, no reset, no swap, no confirm. Run 1 proves everything;
run 2 proves nothing — **and goes green fastest exactly when it is testing
least.** That is worse than having no gate, because it reports safety it never
checked.

The obvious workaround is worse still. Forcing `skip-if-same-hash=false` puts
the same image in both slots, and `flash.rs:153` then matches the *already
active, already confirmed* slot-0 copy first, routing the deploy down the
direct-write branch that never calls `set_state` at all. The one invariant the
gate exists to prove becomes unfalsifiable while the log reads reassuringly.

> The slot-ordering half of that — that MCUboot lists slot 0 first, so `find()`
> matches it — is **inferred from the code, not run.** Confirm it on the bench
> before relying on the conclusion.

#### The fix, which solves several problems at once

**Re-sign the same known-bootable binary with a rolling `--version` on every
run.** Use the imgtool block the sim gates already have
(`scripts/native-sim-e2e.sh:34-44`), changing only `--slot-size` to `0xd0000`
for RP2040 — *not* native_sim's `0x69000`, and *not* the Feather's `0x76000`.
(Read the value off `slot0_partition` in the generated `app-test/zephyr/zephyr.dts`
of a build tree; `runtt-boards`' `scripts/build-pico.sh`, `build-pico2w.sh` and
`build-feather.sh` each produce one.) A gate covering every board needs the slot
size per target, not a constant — though RP2040 and RP2350 happen to share
`0xd0000`, verified in the generated devicetree rather than assumed from the
family.

The version lives in the MCUboot header, so the digest is new every run while
the code is identical and already proven to boot. That single trick:

* makes the gate idempotent from any starting slot state;
* sidesteps the anti-reflash-storm guard at `flash.rs:113-122`, which otherwise
  makes one bad build's digest **permanently** rejected (*"already present and
  marked unbootable; refusing to reflash it. Push a different release."*);
* guarantees the deployed image is one that boots, which is what keeps the gate
  from wedging its own hardware.

Record the digest in the run directory, and read `image list` **before and
after** to assert the active digest actually changed. Never infer that from the
runtime's own log.

### The second trap: exit-code polarity inverts

`scripts/native-sim-engine-e2e.sh:182` asserts the container exits **non-zero**,
because native_sim cannot swap. On real hardware a successful deploy means the
container **never exits** — it heartbeats indefinitely.

So `timeout 120 podman run …` returns 124 on success, which is also exactly what
a deploy that died at second 3 returns. Copied unchanged, that assertion is
worthless in both directions.

Drive the direct OCI path instead: `create`, `start`, poll for
`mcu: image confirmed`, then explicitly `kill <id> TERM` and assert the proxy
exits **0** (`stay_resident` returns `Ok` on `should_stop`), then
`delete --force`.

### Testing revert without a human, and without power control

The valuable scenario, and the one that looks impossible at first.

**Make the test image bad *only* by being unconfirmed.** It is a real,
contract-speaking image that enumerates perfectly — it just never gets confirmed.
Then:

1. Board at known-good A.
2. Stage B: upload, `set_state(hash, confirm=false)`, reset.
3. Assert B is active with `confirmed=false`; probe UART shows `Swap type: test`.
4. **Command a second reset over SMP** — B works, so the host can trigger its own
   revert boot. This is the step that removes the need for power control.
5. Assert A is active and confirmed, B is present with `bootable=false`, the
   probe UART shows `Swap type: revert`, and `describe` reports A's version.
6. Assert the runtime now **refuses** to redeploy B, and exits non-zero.
7. Teardown deploys a fresh version so the board is left in a known state.

Worth adding alongside it: SIGKILL the proxy the instant `marked test,
resetting` appears, to prove a runtime crash mid-deploy cannot brick a board.

### What the gate must never do

**Never deploy an image that might not enumerate.** Verified on this bench:

| Escape route | State |
|---|---|
| Software route into BOOTSEL | **absent** — 0 `CONFIG_RETENTION`, `ROM_BOOTLOADER` or `RESET_BOOT_MODE` in either shipped image |
| `uhubctl` port power cycling | **not installed** |
| Hub per-port power switching | **not supported** by the attached hub |

If a staged image goes silent there is no SMP channel to command another reset
and no way to cut power. The board sits wedged until someone physically holds
BOOTSEL. **One such run stops all hardware work until that specific person is at
that specific desk.** Keep the true no-boot case out, and say so in the PASS
text — a green run must not be read as covering it.

Two further exclusions:

* **Never `cat` the log node while a container is resident.** The kernel hands
  each byte to exactly one reader, so a concurrent read steals lines out of the
  container's own stdout. Assert on the container's stdout — the channel the
  product actually promises.
* **Never auto-retry.** The anti-storm guard means retrying the same image is
  strictly worse than failing. The remedy is a new release, not a rerun.

### Two gaps to close before writing the gate

**1. `ping.rs` is not an assertion tool.** It exits 1 only on `open` failure —
`echo failed` and `image list failed` both exit **0**, merely printing the error.
Its slot line prints active/confirmed/pending/version/hash but **not
`bootable`**, which is precisely the field the revert assertion needs (the field
exists on `ImageSlot`; ping just doesn't print it). Needs a small checked-in tool
with real exit codes and machine-readable output, modelled on
`scripts/flash-inspect.py`.

**2. Teardown has no precedent in this repo.** The sim gates use
`trap 'kill %1 …' EXIT`, which works only because exactly one process is
backgrounded. The proxy is not a shell job at all — `create` spawns it in its own
process group and exits. Any `fail` between `create` and the inline
`delete --force` leaks a resident proxy holding the mgmt tty, and the *next* run
won't collide, because `--root` is a fresh `mktemp` each time and the occupancy
lock is keyed under it. Two SMP writers then share one CDC channel and the
corruption gets blamed on whatever changed.

So a hardware gate needs a **fixed** `--root` (e.g. under `/run/user/$(id -u)`)
so the runtime's own occupancy lock actually excludes a concurrent run, a real
teardown function in the trap that polls the pid until it is gone, and a
precondition that `pgrep`s for a resident proxy and fails loudly.

### Resolved: the RP2040 "swap bug" was a malformed test image

Kept as a record because it consumed a day and produced five wrong hypotheses,
all of which fit the evidence.

**The symptom.** A staged image plus a reset never swapped. The board either
locked up (`xpsr` exception 3, `SP = 0xffffffe0`, i.e. SP was zero) or booted
the old image with the pending flag silently cleared. It looked exactly like a
bootloader defect, and the fact that RP2040 MCUboot swap had no public evidence
of ever working made that story easy to believe.

**The cause.** The hand-signed test image was malformed. An application built to
run under MCUboot sets `CONFIG_ROM_START_OFFSET=0x200` and already reserves its
header space, so signing it with `imgtool --pad-header` prepends a *second*
0x200 header. The image then declares `hdr_size=0x200` while its vector table
sits at 0x400. MCUboot jumps to `image + 0x200`, finds the padding, loads
`SP = 0` and `PC = 0`, and locks up. `imgtool verify` reports the image as
perfectly valid, because by its own accounting it is.

**MCUboot swap on RP2040 works.** With a correctly signed image the full cycle
completes: upload, mark test, reset, swap, confirm, new firmware boots and its
logs reach container stdio. Verified end to end, digest matched independently.

#### What this cost, and the lesson

The wrong turns, in order: a system workqueue stack overflow; the idle
application specifically; an undrained log channel starving the workqueue;
`hw-flow-control` on both CDC channels (which broke enumeration outright and
cost a flash cycle); and a stopped SysTick. Each fit every symptom then visible.

Two things would have caught it far sooner. **Validate your own test artefacts
before suspecting the platform** -- the malformed image was created in the first
ten minutes and never questioned, because `imgtool verify` passed. And **read
flash only at a `reset halt`**: reads taken while the core is locked up or
inside a bootrom flash routine run with XIP disabled and return garbage, which
produced one confidently wrong conclusion about corrupted flash.

The genuine upstream finding that survived is unrelated to the deploy path: see
[`MCUBOOT_SWAP_BUG.md`](https://github.com/shaunmulligan/runtt-boards/blob/main/NOTES.md).

### Staging

**Minimum viable.** `scripts/hardware-e2e.sh`, run by hand, same four-line
preamble and `ok:`/`FAIL:`/`PASS:` vocabulary as the sim gates: preflight (board
enumerates, no resident proxy, no unbootable slot), re-sign with a rolling
version, deploy through podman, assert the active digest changed via `image
list` before and after, assert logs reached container stdout, teardown, leave
the board confirmed. Print the exact recovery command as the last line on
failure.

**Then.** The revert scenario above, plus the SIGKILL-mid-deploy case.

**Before any unattended ambition.** A switchable USB hub is the single
highest-leverage purchase in the project; enabling the retention/boot-mode path
so a script can command BOOTSEL is the highest-leverage firmware change. The
software route already exists in Zephyr itself
(`zephyr/soc/raspberrypi/rpi_pico/common/rom_bootloader.c`, in a workspace
assembled from runtt-boards' manifest — there is no vendored tree here any
more), it is simply not enabled.

### ⚠️ If this ever becomes a CI job

A self-hosted runner attached to `on: push` / `pull_request` — which is what
`ci.yml` currently uses — gives **any fork's pull request arbitrary shell on the
bench machine and USB write access to the board.** And because everything is
signed with MCUboot's published development private key, there is no trust root
to stop it pushing firmware the board will accept.

Put the hardware job in a separate workflow gated on `workflow_dispatch` plus
protected branches, or collaborator-restricted label gating. Resolve the signing
key before unattended running, not after — rotating it later means re-flashing
every board over SWD.



---

# Walking the native_sim flow by hand

> Was `docs/MANUAL_VERIFICATION.md`.

`scripts/native-sim-e2e.sh` automates this. This is the same thing step by step,
so you can watch each part and poke at it.

Every block of output below was captured from a real run, not reconstructed.
Your pty numbers and digests will differ.

### Why bother, given the script exists

The script asserts. This lets you *look*. Two things are worth seeing with your
own eyes because they're the load-bearing claims:

- the uploaded image is physically present in the simulated flash, byte for byte,
  checked from **outside** the device rather than by asking it;
- the runtime marks the image **test** and only ever confirms afterwards — the
  ordering that makes a broken image unable to confirm itself.

### Setup

```bash
cd ~/runtt
export PATH="$HOME/.cargo/bin:$HOME/.local/bin:$PATH"
export ZEPHYR_BASE="$PWD/zephyr" ZEPHYR_TOOLCHAIN_VARIANT=host

cargo build                      # the runtime
./scripts/build-native-sim.sh    # the firmware

mkdir -p /tmp/manual && cd /tmp/manual
```

If `build-native-sim.sh` fails, the west workspace probably isn't populated:
`west update --narrow -o=--depth=1` from the repo root.

---

### 1. Sign an image

The device validates the MCUboot header, so a raw binary is rejected. This is the
device being correct, not an obstacle.

```bash
head -c 8192 /dev/urandom > payload.bin

python3 ~/runtt/bootloader/mcuboot/scripts/imgtool.py sign \
  --key ~/runtt/bootloader/mcuboot/root-ec-p256.pem \
  --header-size 0x200 --pad-header --align 4 \
  --version 2.0.0 --slot-size 0x69000 \
  payload.bin app.signed.bin
```

`--pad-header` is required when signing a raw binary: without it imgtool refuses
with *"Header padding was not requested and image does not start with zeros"*,
because it expects the input to have already reserved header space.

The payload is random bytes on purpose. native_sim can never *execute* an image
regardless (see §9), so what matters is that the bytes form a valid MCUboot image.

### 2. Note the digest the device will report

```bash
python3 ~/runtt/bootloader/mcuboot/scripts/imgtool.py verify \
  --key ~/runtt/bootloader/mcuboot/root-ec-p256.pem app.signed.bin
```

```
Image was correctly validated
Image version: 2.0.0+0
Image digest: a1ae4888bc70c7ca663c6d5f8d4bb8f125b25cdf6a9ab7ccb92a3eb899f47ff4
```

**Keep that digest.** It is the one thing to watch for in step 7.

It is *not* the SHA-256 of the file — check for yourself with `sha256sum
app.signed.bin` and you'll get something different. The file hash is what the
upload's `sha` field carries, for transfer integrity. The **image digest**, stored
in the image's TLV area, is the identity the device reports in `image list` and
expects in `set_state`. Passing the wrong one gets you
`IMG_MGMT_ERR_HASH_NOT_FOUND`, and that error does not hint at why.

### 3. Build an OCI bundle

Exactly what a container engine hands a runtime: a `config.json` and a rootfs.

```bash
mkdir -p bundle/rootfs
cp app.signed.bin bundle/rootfs/

cat > bundle/config.json <<'JSON'
{
  "ociVersion": "1.2.0",
  "process": {
    "user": { "uid": 0, "gid": 0 },
    "args": ["app.signed.bin"],
    "cwd": "/",
    "terminal": false
  },
  "root": { "path": "rootfs", "readonly": true },
  "annotations": { "dev.runtt.target": "tty:/tmp/manual/mgmt" }
}
JSON
```

Two fields carry all the meaning:

- `process.args` — the entrypoint names the firmware, which is how the runtime
  finds it inside the rootfs. Same convention as `arm/remoteproc-runtime`.
- `annotations` — the placement label. `tty:` is what makes a simulator or a
  probe's UART bridge addressable; on real hardware you'd use `usb:3-6`.

### 4. Start native_sim

```bash
~/runtt/build/zephyr/zephyr.exe \
  --uart_attach_uart_cmd='ln -sf %s /tmp/manual/mgmt' \
  --uart_1_attach_uart_cmd='ln -sf %s /tmp/manual/log' \
  --flash=/tmp/manual/flash.bin &
```

```
uart connected to pseudotty: /dev/pts/284
uart_1 connected to pseudotty: /dev/pts/285
*** Booting Zephyr OS build dccb09599635 ***
[00:00:00.000,000] <inf> app: runtt template app 0.1.0 starting on native_sim/native/64
```

Two things to notice.

**The announcements are keyed by devicetree *node* name, not label.** The nodes are
`uart` and `uart_1`; the labels are `uart0`/`uart1`. The node name is what appears
here and in the per-instance CLI options (`--uart_1_attach_uart_cmd`).

**The symlinks are not a convenience.** pty numbers change on every reset, and
`os reset` re-execs the simulator — so anything that parsed `/dev/pts/284` would
be holding a stale path after the reboot in step 9. The `%s` in
`--uart_attach_uart_cmd` is substituted with the pty path, so the runtime opens a
fixed name instead:

```bash
ls -l mgmt log
```

```
log -> /dev/pts/285
mgmt -> /dev/pts/284
```

> **Do not add `--flash_erase`.** `os reset` re-execs the process *preserving
> argv*, so it would fire again on every reboot and wipe exactly the state the
> reset is meant to preserve. To start from a clean device, delete `flash.bin`
> first — that erases once instead of on every boot.

### 5. Confirm logs arrive on the log channel

```bash
timeout 5 cat log
```

```
*** Booting Zephyr OS build dccb09599635 ***
[00:00:00.000,000] <inf> app: runtt template app 0.1.0 starting on native_sim/native/64
[00:00:00.000,000] <inf> app: alive, tick 0
[00:00:02.010,000] <inf> app: alive, tick 1
```

This is the second contract channel, and it's the mechanism behind the headline
feature: the runtime forwards this to container stdio, so it becomes
`docker logs`.

Read the **channel**, not the simulator's stdout. native_sim also mirrors console
output to its own stdout and that cannot be disabled — the board does
`select POSIX_ARCH_CONSOLE`, and a `select` can't be overridden by a conf
fragment. So a check against the simulator's stdout would pass even if the
channel were dead.

### 6. Look at the flash before deploying

```bash
~/runtt/scripts/flash-inspect.py flash.bin
```

```
partition      offset       used  notes
--------------------------------------------------------------
boot       0x00000000          0  erased
slot0      0x0000c000          0  erased
slot1      0x00075000          0  erased
scratch    0x000de000          0  erased
storage    0x000fc000          0  erased
```

All erased. Note the layout is Zephyr's stock native_sim devicetree — `boot`,
`slot0`, `slot1`, `scratch` already exist, so no partition overlay was needed.

### 7. Deploy

The two verbs a container engine would call. `create` forks the resident proxy and
exits; `start` releases it to do the work.

```bash
cd ~/runtt
BIN=./target/debug/runtt
$BIN --root /tmp/manual/state create \
     --bundle /tmp/manual/bundle --pid-file /tmp/manual/pid manual \
     > /tmp/manual/rt.log 2>&1
```

Redirect the output rather than piping it. The proxy inherits stdio, so a pipe
won't see EOF until the container exits — under an engine that pipe *is* the
container log, which is the point, but it makes interactive use awkward.

Between `create` and `start`, nothing has been flashed yet:

```bash
$BIN --root /tmp/manual/state state manual | python3 -m json.tool
```

```json
{
    "ociVersion": "1.1.0",
    "id": "manual",
    "status": "created",
    "pid": 147145,
    "bundle": "/tmp/manual/bundle",
    "annotations": {
        "dev.runtt.firmware-path": "/tmp/manual/bundle/rootfs/app.signed.bin",
        "dev.runtt.target": "tty:/tmp/manual/mgmt"
    }
}
```

That `pid` is the container as far as the engine is concerned. Now start it:

```bash
$BIN --root /tmp/manual/state start manual >> /tmp/manual/rt.log 2>&1
sleep 10
cat /tmp/manual/rt.log
```

```
INFO runtt::proxy: resolved target mgmt=/tmp/manual/mgmt log=None
mcu: single channel; application logs share the management link
INFO runtt::flash: deploying firmware target="tty:/tmp/manual/mgmt" bytes=8854
     version=2.0.0+0 digest="a1ae4888bc70c7ca663c6d5f8d4bb8f125b25cdf6a9ab7ccb92a3eb899f47ff4"
mcu: uploading 8854/8854 bytes (100%)
WARN mcumgr_toolkit::client: Device did not perform image checksum verification
mcu: image staged and marked test, resetting
runtt: the image is staged and marked pending, but nothing swapped it in: no image
  is active after the reset. On a target with no bootloader this is expected and
  swap/confirm are unreachable by construction (native_sim cannot chain-load MCUboot).
  On real hardware it means MCUboot did not run -- check it is actually flashed, and
  that its swap mode matches the mode the image was built for.
```

Four things to check here:

1. **The digest matches what imgtool told you in step 2.** If it matched
   `sha256sum app.signed.bin` instead, the runtime would be using the wrong hash.
2. **`version=2.0.0+0`** — parsed out of the image header, so the TLV parse is
   working.
3. **`log=None` and "single channel"** is correct for a `tty:` target: that label
   names one device. Channel discovery by interface descriptor only applies to
   `usb:`. On native_sim the log channel exists but the runtime isn't told where
   it is, which is why you read it yourself in step 5.
4. **The final error is the expected outcome**, not a failure of the flow. It is
   also the one message worth reading carefully — it distinguishes "nothing
   swapped it in" from "the wrong image is running", because on hardware those
   have opposite remedies.

The `Device did not perform image checksum verification` warning is expected and
benign: `CONFIG_IMG_ENABLE_IMAGE_CHECK` is deliberately off because it pulls in
mbedtls, whose `tf-psa-crypto` git submodule `west update --narrow` doesn't fetch.
The runtime does a stronger check anyway — see step 8.

### 8. Verify the upload actually landed

The claim worth checking independently. `flash.bin` is a plain host file:

```bash
~/runtt/scripts/flash-inspect.py /tmp/manual/flash.bin \
  --expect-image /tmp/manual/app.signed.bin
```

```
partition      offset       used  notes
--------------------------------------------------------------
boot       0x00000000          0  erased
slot0      0x0000c000          0  erased
slot1      0x00075000       8361  MCUboot image header, trailer magic set (marked)
scratch    0x000de000          0  erased
storage    0x000fc000          0  erased

slot 1 matches /tmp/manual/app.signed.bin byte for byte (8854 bytes)
```

Three separate facts:

- **slot 1 is populated, slot 0 is still erased.** The upload went to the staging
  slot and did not touch the running one. That is what makes the operation safe
  to interrupt.
- **byte-for-byte match** against the file we signed — the transfer is intact,
  verified outside the device.
- **trailer magic set.** MCUboot writes `77c295f3 60d2ef7f 3552500f 2cb67980` at
  the end of a slot to mark its trailer valid. Its presence is the physical
  evidence that `set_state(test)` did something.

The runtime also cross-checks this itself: after uploading it reads `image list`
and compares the device-reported digest against the one it parsed from the image's
own TLV area, and refuses to mark anything bootable if they disagree.

### 9. Confirm the reset really happened

```bash
grep -E "Swap type|Restarting|Booting" /tmp/manual/sim.out
```

```
*** Booting Zephyr OS build dccb09599635 ***
<inf> mcuboot_util: Image index: 0, Swap type: none
<inf> mcuboot_util: Image index: 0, Swap type: none
<inf> mcuboot_util: Image index: 0, Swap type: test      <-- set_state(test) took effect
native_sim_reboot: Restarting process.                   <-- os reset
*** Booting Zephyr OS build dccb09599635 ***             <-- it came back
<inf> mcuboot_util: Image index: 0, Swap type: test      <-- and the trailer survived
```

`Swap type: none` → `test` is the device's own view of the trailer changing. Then
`os reset` re-execs the process, and the second banner proves it came back. The
trailer surviving shows the simulated flash persisted across the reboot.

Note the two boot banners:

```bash
grep -c "Booting Zephyr OS" /tmp/manual/sim.out    # 2
```

### 10. Clean up

```bash
./target/debug/runtt --root /tmp/manual/state delete --force manual
kill %1      # the simulator
```

`delete` waits for the proxy to actually exit rather than returning as soon as the
signal is sent — otherwise the device would still be open when a replacement
container tried to claim it, which is exactly what a restart policy would do.

---

### Doing it through a container engine instead

The steps above call the OCI verbs by hand, which is the clearest way to see each
one. But it skips the other half of the picture: that the firmware is really
delivered *as a container image*. For that:

```bash
./scripts/native-sim-engine-e2e.sh            # podman, no setup needed
ENGINE=docker ./scripts/native-sim-engine-e2e.sh
```

That builds a `FROM scratch` image holding the signed firmware, runs it through
the engine with the placement label as an OCI annotation, and checks the deploy
happened — including that the bytes in the simulated flash match the image that
was shipped inside the container image.

The container is **expected to exit non-zero**: native_sim cannot swap, so the
runtime correctly refuses to confirm. That non-zero exit is what drives a restart
policy on a real device.

> podman is the default because it needs no daemon config and no root. It is not
> a complete substitute for Docker: containerd passes global flags podman does
> not (`--root`, `--log`, `--log-format`), sends `kill` differently, and calls
> `delete` twice. The script header lists the differences; `docs/OCI_COMPLIANCE.md`
> has the real traces.
>
> If the Docker path fails with `IMG_MGMT_ERR_HASH_NOT_FOUND` or a missing
> `version=`, the registered binary is stale — Docker runs
> `/usr/local/bin/runtt`, not what you just built. The script detects this
> and tells you; re-run `sudo scripts/register-docker.sh`.

### What this does and doesn't prove

Proved above: the OCI verbs, two contract channels as ptys, SMP framing against a
real Zephyr MCUmgr server, upload landing physically in the staging slot, the
trailer write, `os reset`, and reconnection.

**Not proved, and unprovable here: swap, revert and confirm.** MCUboot cannot
chain-load on native_sim — its POSIX path computes `flash_base + offset` and calls
it as a function pointer, and "flash" here is an `mmap`'d data file with no
`PROT_EXEC` holding an image for another architecture. The Zephyr issue
([#86185](https://github.com/zephyrproject-rtos/zephyr/issues/86185)) is closed as
"not planned", and the reason is architectural rather than a missing build fix.

That coverage comes from MCUboot's own Rust simulator, which compiles the real
`bootutil` C sources over a NOR-flash model with injectable failures:

```bash
cd bootloader/mcuboot/sim
cargo test --features sig-ecdsa -- basic_revert norevert
```

If it fails to build, it needs git submodules west doesn't fetch:

```bash
git -C bootloader/mcuboot submodule update --init --depth 1 ext/mbedtls ext/tinycrypt
```

---

### The fast path: skip Zephyr entirely

For error paths, the mock is quicker and can inject faults on demand:

```bash
./target/debug/runtt-mock --symlink /tmp/mock-tty &
# then point a bundle at tty:/tmp/mock-tty and deploy as above

./target/debug/runtt-mock --help          # the available faults
./target/debug/runtt-mock --fault bad-hash --symlink /tmp/mock-tty
```

Unlike native_sim the mock *does* model swap and revert, so it can show you an
unconfirmed image rolling back — which is the case real hardware is needed for
otherwise. `cargo test -p runtt-mock` covers that state machine directly.

---

### Troubleshooting

Each of these was hit for real while building this.

**`IMG_MGMT_ERR_INVALID_IMAGE_HEADER_MAGIC`** — the image isn't signed. Run
step 1. The device validates the header before accepting bytes.

**`IMG_MGMT_ERR_HASH_NOT_FOUND` on set-state** — the wrong hash was used for
image identity. It must be the MCUboot digest from the TLV area, not the file's
SHA-256. See step 2.

**`uart_mcumgr: Insufficient buffers, fragment dropped`** on the device, and a
timeout on the host — the transport MTU and `NETBUF_SIZE` disagree. The
MCUmgr-parameters command reports `NETBUF_SIZE`, and the client sizes its frames
from that, but the UART transport enforces its own MTU (default 256). They're
pinned equal in `runtt.conf`; if you change one, change both. Also note each
received *line* holds a whole RX buffer until reassembly, so `RX_BUF_COUNT` has to
cover lines-per-packet — not merely satisfy the documented
`COUNT * SIZE >= MTU`.

**Slot 1 empty after a reset** — `--flash_erase` was passed. `os reset` preserves
argv, so it re-fires every reboot. Delete `flash.bin` instead.

**`Device or resource busy` opening the pty** — something still holds it. A
previous proxy may be orphaned: `pgrep -af 'runtt.* proxy'`, then kill it.
(`TIOCEXCL` is deliberately disabled in the runtime for a related reason — see
`docs/OCI_COMPLIANCE.md`.)

**`target ... is already claimed by another service`** — the occupancy lock is
working. A previous container's proxy still holds it; `delete --force` the old
container, or kill the orphan.

**Build fails with `CONFIG_MCUMGR_TRANSPORT_LOG_LEVEL undeclared`** — `zcbor` is
missing from the west workspace. MCUmgr's root Kconfig depends on it, and without
it `CONFIG_MCUMGR` silently doesn't exist rather than failing loudly.
`west update`.



---

# Verifying the single-channel log demux by hand

> Was `docs/MANUAL_LOG_DEMUX.md`.

What this checks: on a target where the application's console output shares the
SMP management link, those log lines still reach the container's stdout — and
the deploy still works while they do.

Before the demux existed, a single-channel container deployed fine and printed
**nothing**. `mcumgr-toolkit`'s frame reader scans forward for the framing
marker and silently discards everything it steps over, so the application's
output was dropped on the floor. Worse, the runtime announced that logs were
sharing the link, so the gap looked like intended behaviour.

Three routes below, cheapest first. Each is self-contained.

> **Paths below are from the monorepo layout these transcripts were recorded
> in.** The repositories have since been split, so `firmware/app` is now
> `boards/app-test` (or `boards/idle`) in
> [`runtt-boards`](https://github.com/shaunmulligan/runtt-boards),
> `firmware/bringup` is `boards/bringup`, and `firmware/runtt` is its own
> repository, checked out by west at `modules/runtt`. The commands are left as
> they were run rather than retyped untested; `-DZEPHYR_EXTRA_MODULES` in
> particular is no longer needed at all, because west now registers the module
> from the manifest. See `runtt-boards`' `scripts/build-feather.sh` (or
> `build-pico.sh` / `build-pico2w.sh`) for the current invocation.

---

### 1. Two minutes, no Zephyr: the mock

`runtt-mock --chatter` emits application log lines on the same link it serves SMP
on, which is exactly the single-channel shape.

```bash
cargo build --workspace

# A device that talks SMP *and* prints.
./target/debug/runtt-mock --symlink /tmp/mcu-tty --chatter '<inf> app: alive tick' &
PTS=$(readlink -f /tmp/mcu-tty)

# A bundle pointing at it. tty: targets are single-channel by definition.
mkdir -p /tmp/demo/rootfs
cp crates/smp-client/tests/fixtures/app.signed.bin /tmp/demo/rootfs/
cat > /tmp/demo/config.json <<JSON
{ "ociVersion": "1.2.0",
  "process": { "user": {"uid":0,"gid":0}, "args": ["app.signed.bin"],
               "cwd": "/", "terminal": false },
  "root": { "path": "rootfs", "readonly": true },
  "annotations": { "dev.runtt.target": "tty:$PTS" } }
JSON

./target/debug/runtt --root /tmp/demo-state create --bundle /tmp/demo one
./target/debug/runtt --root /tmp/demo-state start one
sleep 8
./target/debug/runtt --root /tmp/demo-state delete --force one
kill %1
```

**What to look for.** `create` prints to its own stdout, so you will see both
the deploy narration and the device's chatter interleaved:

```
mcu: single channel; application logs are demultiplexed from the management link
mcu: uploading 8856/8856 bytes (100%)
<inf> app: alive tick 12
mcu: image staged and marked test, resetting
<inf> app: alive tick 13
mcu: image confirmed
```

**The failure this catches:** without the demux you still get every `mcu:` line
and the deploy still succeeds — but no `<inf> app:` line ever appears.

---

### 2. Real Zephyr firmware on a genuine single-channel link

The mock is a mock. This runs actual Zephyr with the console and the SMP server
sharing one UART, which is what an ESP32-C3 class part or a probe-UART bring-up
looks like.

`CONFIG_RUNTT_CHANNELS=1` alone is **not** enough — it is declarative, and
only changes the number `describe` reports. The channel count is a devicetree
fact, so move the console onto the management UART:

```bash
cat > /tmp/single-channel.overlay <<'DTS'
/* One link for everything: console/log output shares the SMP management UART. */
/ {
	chosen {
		zephyr,console = &uart0;
		zephyr,shell-uart = &uart0;
	};
};
DTS

export ZEPHYR_BASE="$PWD/zephyr" ZEPHYR_TOOLCHAIN_VARIANT=host
west build -p always -b native_sim/native/64 --snippet runtt firmware/app \
  -d /tmp/build-1ch -- \
  -DZEPHYR_EXTRA_MODULES="$PWD/firmware/runtt" \
  -DEXTRA_DTC_OVERLAY_FILE=/tmp/single-channel.overlay \
  -DCONFIG_RUNTT_CHANNELS=1
```

Run it, and point the runtime at the management pty only:

```bash
rm -f /tmp/1ch-mgmt /tmp/1ch-flash.bin
/tmp/build-1ch/zephyr/zephyr.exe \
  --uart_attach_uart_cmd='ln -sf %s /tmp/1ch-mgmt' \
  --flash=/tmp/1ch-flash.bin &
sleep 1
PTS=$(readlink -f /tmp/1ch-mgmt)
```

Then build a bundle against `tty:$PTS` exactly as in §1 and run
`create` / `start`.

**Observed output**, with the device's own logs interleaved with the deploy:

```
mcu: single channel; application logs are demultiplexed from the management link
mcu: device is native_sim/native/64 running 0.1.0 (contract 2.0.0, 1 channel)
*** Booting Zephyr OS build dccb09599635 ***
[00:00:00.000,000] <inf> app: runtt template app 0.1.0 starting on native_sim/native/64
[00:00:00.000,000] <inf> app: alive, tick 0
mcu: uploading 8856/8856 bytes (100%)
[00:00:00.500,000] <inf> mcuboot_util: Image index: 0, Swap type: test
mcu: image staged and marked test, resetting
*** Booting Zephyr OS build dccb09599635 ***
```

Three things worth noticing:

* `describe` reports **1 channel**, and the runtime says so.
* Zephyr's own subsystem logs (`mcuboot_util`) come through too, not just the
  application's — anything on the console backend does.
* The second `*** Booting Zephyr OS ***` is the device coming back after the
  reset, so logs survive the reconnect rather than stopping at the first deploy.

It ends with `nothing swapped it in` — that is native_sim having no bootloader,
not a demux failure. See `docs/MANUAL_VERIFICATION.md`.

> The snippet's overlay still enables `uart1`, so the simulator announces two
> ptys. The second is simply unused here; the console genuinely shares `uart0`,
> which is what `describe` reporting 1 channel confirms.

---

### 3. Building the firmware image with Docker

The firmware service is an ordinary container image: Zephyr toolchain in stage
one, `FROM scratch` carrying only the signed image in stage two.

The build environment comes from a **builder image**, built once, which is what
lets an application directory be self-contained:

```bash
# builder/ lives in runtt-boards; run this from a checkout of it
podman build -f builder/Dockerfile -t runtt-builder:v4.4.2 .
```

Then build the application from inside its own directory:

```bash
cd app1        # in runtt-examples
podman build --build-arg BOARD=rpi_pico/rp2040/mcuboot -t mcu-fw:pico .
```

Then deploy it exactly like any other image:

```bash
podman --runtime="$PWD/target/debug/runtt" run --rm \
  --annotation dev.runtt.target=usb:3-4 \
  mcu-fw:pico
```

Inspect what actually shipped — it should be one layer holding one file:

```bash
podman run --rm --entrypoint="" mcu-fw:pico ls -l /   # no shell in scratch; use:
podman image inspect mcu-fw:pico --format '{{.RootFS.Layers}}'
podman save mcu-fw:pico | tar -t | head
```

> **Two things the application Dockerfile needs that are easy to miss**, both
> handled in the examples but worth understanding if you write your own:
>
> * `-DZEPHYR_EXTRA_MODULES=/ws/runtt`. The module lives *inside* the
>   manifest repo, and west only auto-discovers a `module.yml` at a project's
>   root — ours is nested, so without this the module and its snippet simply are
>   not there. Both build scripts carry the same line.
> * `-Dapp_SNIPPET=` rather than `-S`. Under sysbuild a top-level snippet
>   applies to **every** image, including MCUboot. See [`FIRMWARE_GUIDE.md`](https://github.com/shaunmulligan/runtt-zephyr-module/blob/main/docs/FIRMWARE_GUIDE.md).

**Expect the builder image to be slow the first time** — the Zephyr CI base is
tens of gigabytes, and it then fetches Zephyr and its modules. That cost is paid
once; application builds afterwards are quick. For iterating on firmware,
`scripts/build-pico.sh` against a local west workspace is quicker still; the
Docker path is for producing the artefact you actually ship.

See [`WALKTHROUGH.md`](https://github.com/shaunmulligan/runtt-examples/blob/main/docs/WALKTHROUGH.md) for this build path end to end on hardware.

---

### 4. On hardware

All three boards we ship are **two-channel**, so they take the plain path and the
demux is not involved. To exercise it on real hardware, address the management
channel directly with a `tty:` target, which makes the host treat it as
single-channel:

```bash
MGMT=$(readlink -f /dev/runtt/*-mgmt)
podman --runtime="$PWD/target/debug/runtt" run --rm \
  --annotation dev.runtt.target="tty:$MGMT" \
  mcu-fw:pico
```

Note this only proves the demux does not disturb SMP on a real link — the
board's logs go to its *other* channel, so nothing gets demultiplexed. A true
hardware test needs firmware built with the overlay from §2.

**Deploying the same digest twice is a no-op.** `skip-if-same-hash` defaults on,
so re-running with an unchanged image skips the upload entirely and proves
nothing. Re-sign with a new version each time:

```bash
python3 bootloader/mcuboot/scripts/imgtool.py sign \
  --key bootloader/mcuboot/root-rsa-2048.pem \
  --header-size 0x200 --align 4 \
  --version 0.4.0 --slot-size 0xd0000 \
  build-pico-mcuboot/app/zephyr/zephyr.bin /tmp/v040.signed.bin   # ./scripts/build-pico.sh
```

> ### ⚠️ No `--pad-header` for a hardware image
>
> An application built to run under MCUboot sets `CONFIG_ROM_START_OFFSET=0x200`
> and therefore **already reserves** the header space -- its `zephyr.bin` begins
> with 0x200 zero bytes. `--pad-header` tells imgtool to prepend *another* one.
>
> The result passes `imgtool verify` and looks perfectly healthy: the header
> still declares `hdr_size=0x200`, while the real image now starts at 0x400.
> MCUboot dutifully jumps to `image + 0x200`, lands on the padding, loads
> `SP = 0` and `PC = 0`, and the core ends in an unrecoverable lockup that
> looks exactly like a bootloader bug.
>
> Check before deploying anything you signed by hand -- the word at `hdr_size`
> should be a RAM address (`0x2xxxxxxx`), not zero:
>
> ```bash
> python3 -c "
> import struct,pathlib,sys
> d=pathlib.Path(sys.argv[1]).read_bytes()
> h=struct.unpack('<H',d[8:10])[0]; v=struct.unpack('<I',d[h:h+4])[0]
> print(f'hdr={h:#x} sp={v:#010x}', 'OK' if v>>24==0x20 else 'MALFORMED')" /tmp/v040.signed.bin
> ```
>
> `--pad-header` **is** correct for native_sim, where `ROM_START_OFFSET=0` and
> nothing reserves the space. That is why the sim gates use it and hardware
> must not.

`--slot-size 0xd0000` is the RP2040 partition size, **not** native_sim's
`0x69000`. See `docs/HARDWARE_GATE.md` for why this matters and what else bites.

---

### Troubleshooting

**`Unable to acquire exclusive lock on serial port`** — something still holds
the device. `serialport` takes an `flock(LOCK_EX)` on open, so a leftover proxy
from a failed run keeps it. `pgrep -af runtt` and
`fuser -v /dev/ttyACM*`, then `delete --force` the container.

**The deploy works but no log lines appear** — check the runtime actually said
`single channel`. If it resolved two channels (`log=Some(...)`) it took the
plain path and the demux was never involved.

**Nothing at all on hardware, and USB still enumerates** — the board's firmware
can be wedged while its USB stack still answers control transfers, so `lsusb`
looks healthy and both SMP and logs are silent. There is no software route into
BOOTSEL on RP2040 in the images we ship, so recovery is a physical replug.



---

# Research: what a micro-ROS use case would need

> Was `docs/MICROROS.md`.

**Status: research only. Nothing here is built, and the plan and architecture
are unchanged.** This records what a robotics use case would need — ROS 2 in a
Linux container, ROS nodes on several Zephyr MCUs, all on ROS topics — while
this runtime manages firmware over SMP and pipes MCU logs to container stdio.

Findings are marked **[verified]** (read in source, measured, or built),
**[likely]** (strong evidence, one inferential step), or **[unverified]** (needs
a bench test before anyone plans around it). Treat the unverified ones as open.

### The short answer

**Logging does not have to be dropped, and there is no lock conflict.** The
obstacle is not resource contention — it is that micro-ROS's Zephyr module is
bound to the USB stack this project deliberately does not use, and does not yet
support this project's Zephyr version. Both are bounded problems with a known
path through them; neither is architectural.

### The contention that isn't there

The intuition is that the runtime holds the UART and micro-ROS cannot have it.
Neither side takes an exclusive claim:

* **The agent takes no lock at all.** No `TIOCEXCL`, no `O_EXCL`, no
  `flock`/`lockf` anywhere in Micro-XRCE-DDS-Agent v3.0.1. The serial transport
  opens with exactly `O_RDWR | O_NOCTTY`
  ([`TermiosAgentLinux.cpp:91`](https://github.com/eProsima/Micro-XRCE-DDS-Agent/blob/v3.0.1/src/cpp/transport/serial/TermiosAgentLinux.cpp#L91)).
  A search of that repo's issues for exclusivity returns nothing — it has never
  been requested. **[verified]**
* **Our occupancy lock is not device-shaped.** `lock::acquire`
  (`crates/runtt/src/lock.rs:33`) flocks a regular file named
  `occupancy-<label>.lock` under `--root`. It touches no device node.
  **[verified]**

#### The hazard this creates, which is the real one

Because nothing takes a lock, **nothing stops a misconfigured agent opening the
management channel and corrupting an SMP upload mid-flash.** Every micro-ROS
tutorial says `--dev /dev/ttyACM0`, and on a composite device that is as likely
to be `runtt-mgmt` as the ROS channel.

The only available defence is the one the contract already uses: a distinct
interface string descriptor and its own udev rule. Note the ordering constraint
— **the udev rule must ship before firmware advertising the new interface**, or
the channel gets no `ID_MM_DEVICE_IGNORE=1` and ModemManager will AT-probe it.
`docs/WIRE_CONTRACT.md` already calls that line load-bearing. **[verified]**

### Two blockers, both certain, both bounded

**1. The USB transport is bound to the deprecated stack.**
`CONFIG_MICROROS_TRANSPORT_SERIAL_USB` does `select USB_DEVICE_STACK` — the
legacy stack, which is `select DEPRECATED` in Zephyr 4.4.2 and **removed in
4.5**. This project mandates `USB_DEVICE_STACK_NEXT`. Enabling both produces a
hard link failure, reproduced during this research:

```
multiple definition of `__device_dts_ord_79'; zephyr/libzephyr.a(cdc_acm.c.obj)
```

That transport file was last touched in **October 2022**. **[verified]**

**2. The module does not support our Zephyr.** It officially supports up to
**v4.1**; we pin v4.4.2. Maintainer statement in
[micro_ros_zephyr_module#158](https://github.com/micro-ROS/micro_ros_zephyr_module/issues/158):
*"At the moment, micro-ROS for Zephyr officially supports up to Zephyr 4.1."*
The breakage is POSIX header relocation in 4.2+. micro-ROS is in
[maintenance mode](https://github.com/micro-ROS/micro_ros_zephyr_module/issues/138)
— no new features, community contributions only, and Zephyr bumps have
historically lagged around three years. **[verified]**

> Not attempted: an actual build against 4.4.2. The incompatibility rests on the
> maintainer statement and the error text in PR #163, not a compile. **[likely]**

### The path that works

**The plain `MICROROS_TRANSPORT_SERIAL` is completely clean of the USB stack** —
it selects only `RING_BUFFER` and includes no USB headers. The USB coupling is
confined to the other transport's `select` plus one `usb_enable(NULL)` call.
**[verified]**

And device_next CDC-ACM
[presents the standard Zephyr UART driver API](https://docs.zephyrproject.org/latest/services/connectivity/usb/device_next/cdc_acm.html)
— *"the user or application interface is the UART driver API"*. The plain
transport uses nothing but `uart_irq_callback_set` / `uart_irq_rx_enable` /
`uart_fifo_read` / `uart_poll_out`, all stack-agnostic.

**So a third CDC-ACM channel needs no new transport code** — only a `UART_NODE`
pointing at the new node, plus the app-side `usbd_*` init this project already
has in runtt-zephyr-module's `src/usbd.c`.

The one obstacle is that `UART_NODE` is hardcoded:

```c
#define UART_NODE DT_NODELABEL(usart1)   /* an STM32 label RP2040 does not have */
```

[PR #163](https://github.com/micro-ROS/micro_ros_zephyr_module/pull/163) (open,
unmerged, community) adds exactly the hook needed:

```c
#if DT_NODE_EXISTS(DT_ALIAS(microros_serial))
#define UART_NODE DT_ALIAS(microros_serial)
#else
#define UART_NODE DT_NODELABEL(usart1)
#endif
```

That alias points equally at a hardware UART or a `zephyr,cdc-acm-uart` node.
Cherry-pick the ~30-line serial hunk rather than the PR, whose diff is noisy
(based against `jazzy`, targeted at `kilted`). **[verified]**

> **Upstream bug to carry a patch for.** Both transports contain
> `ring_buf_init(&in_ringbuf, sizeof(uart_in_buffer), uart_out_buffer)` — the RX
> ring buffer backed by the **TX** buffer. Present identically on humble, jazzy,
> kilted and rolling. In the USB variant the two ring buffers genuinely alias the
> same 2 KB store. PR #163 fixes it. **[verified]**

#### If a custom transport is wanted instead

`rmw_uros_set_custom_transport(bool framing, void *args, open_cb, close_cb,
write_cb, read_cb)`. All three built-in transports are *already* custom
transports — `libmicroros.mk` sets `UCLIENT_PROFILE_CUSTOM_TRANSPORT=ON`
unconditionally, and the Kconfig choice only picks which `.c` file compiles.

Best reference implementation is the official RP2040 one,
[`pico_uart_transport.c`](https://github.com/micro-ROS/micro_ros_raspberrypi_pico_sdk/blob/kilted/pico_uart_transport.c)
— **72 lines total**, of which the four callbacks are ~50. Use `framing=true`
for any byte stream. **[verified]**

#### Hardware UART as a fallback

`uart1` on the Pico is `status = "disabled"` in the stock devicetree and
therefore free (`uart0` is the physical pins, already enabled). On a robot where
the MCU is wired to an SBC rather than plugged into it, a hardware UART sidesteps
the USB questions below entirely. **[verified]**

### Third channel feasibility

**Endpoint budget fits.** Measured on the live two-channel composite: **4 IN +
2 OUT** (per CDC-ACM instance: one interrupt IN at 16 bytes for notifications,
plus a bulk IN/OUT pair at 64). A third instance takes it to **6 IN + 3 OUT** —
comfortable on RP2040's 16 bidirectional, and fitting nRF52840's 7 IN + 7 OUT,
though a *fourth* CDC channel would not fit the nRF. **[verified]**

**Whether three instances actually enumerate is unknown.** Endpoint assignment
happens at runtime in `usbd_init()`/`assign_ep_addr()`, not at compile time, so a
successful build proves nothing. No upstream Zephyr sample declares more than
**two** `cdc-acm-uart` nodes, and no upstream test declares more than one. This
needs a hardware test on both parts — and that test must re-verify that the
existing mgmt and log channels still get stable `ID_PATH`s, since interface
numbering shifts. **[unverified]**

**Expect workqueue tuning.** `CONFIG_USBD_CDC_ACM_WORKQUEUE` is not set in the
current build, so all instances submit to the system workqueue, whose stack is
pinned at `CONFIG_SYSTEM_WORKQUEUE_STACK_SIZE=2560`. Budget for setting that
Kconfig and raising `UDC_BUF_COUNT`/`UDC_BUF_POOL_SIZE` (currently 16/1024), and
expect SMP upload throughput to degrade while micro-ROS is streaming. Also worth
tracking: [zephyr#103324](https://github.com/zephyrproject-rtos/zephyr/issues/103324),
a live device_next endpoint-allocator defect. **[verified]**

### Memory, which is the thin ice

No upstream figure exists for Cortex-M0+ or RP2040. The commonly quoted
"32 KB RAM / 256 KB flash" is third-party, not upstream. The official
[memory profiling page](https://micro.ros.org/docs/concepts/benchmarking/memo_prof/)
is measured on ESP32/FreeRTOS/UDP and reports only marginal per-entity costs.

Measured directly during this research, via `arm-zephyr-eabi-size` on the shipped
`libmicroros.a` for `armv6s-m`:

| | bytes | |
|---|---|---|
| text | 167,552 | |
| data | 10,511 | |
| bss | 35,595 | `rmw_microxrcedds` 20,152 (static entity pools) + `rcutils` 14,485 |
| **flash** | **178,063** | ≈ 174 KiB |
| **static RAM** | **46,106** | ≈ 45 KiB |

**Caveats, which matter:** object-granularity upper bound *before*
`--gc-sections`, so the real link is smaller. Excludes all message typesupport
(a `std_msgs/Int32` publisher adds ~1.1 KB), the transport, the application, and
stack/heap — and the Pico example sets `CONFIG_MAIN_STACK_SIZE=25000`. Against
RP2040's 264 KB SRAM there is headroom, but not comfort. **[verified as a
measurement, but an upper bound]**

Two related facts: **RP2040 is an officially supported micro-ROS target — via
the Pico SDK, not Zephyr.** The Zephyr-supported boards are Cortex-M4 class
(the module's CI builds one board, `disco_l475_iot1`). No RP2040-on-Zephyr
micro-ROS port exists upstream. And building `libmicroros` shells out to
`make -f libmicroros.mk`, which builds ROS 2 from source with colcon — so the
two-stage firmware Dockerfile would need a full ROS 2 toolchain and network
access at firmware-build time. **[verified]**

### Do not share the SMP link

Zephyr's `uart_mcumgr` receive path has **no start-marker check** — it consumes
and destroys bytes that are not part of an SMP frame. **[verified]**

This was steelmanned rather than assumed. Re-escaping micro-ROS frames to be
`0x0A`-free and newline-terminated does make them invisible to the line
assembler: SMP passed **20/20** where raw framing gave 0/20. But that depends
entirely on whole frames never interleaving. Feed the identical, correctly
escaped frames so they land mid-SMP-frame and it drops back to **0/20** — one
injected `0x0A` splits the SMP frame into two lines that both fail the `0x0609`
marker check and are **silently discarded**. No error, no log, just a management
command that never completes. **[verified by test]**

That scheme tests green on a bench with one carefully sequenced sender and then
fails intermittently in the field the first time a write is split across two
`write()` calls or traffic simply arrives asynchronously — which is the normal
case. The symptom would be *"flashing occasionally hangs"*, on the one channel
that carries firmware updates.

If sharing is ever pursued it needs a real mux with TX arbitration on both ends
(CMUX or similar), not a cooperative escaping convention.
`MCUMGR_TRANSPORT_SHELL` is the only sanctioned shared-link path upstream, and
its `0x06` latch behaviour makes it unsafe for binary co-traffic. **[verified]**

### Operational findings for a robotics deployment

**The ROS graph is too stable, not too volatile — the opposite of the intuition.**
The agent never reaps a client on transport loss: the reap branch
(`Processor.cpp:1002`, `:1028`) is gated on `has_hard_liveliness_check()`, which
requires building with `UCLIENT_HARD_LIVELINESS_CHECK` — still `option(... OFF)`
upstream. DDS liveliness is asserted by the **agent**, not the MCU.

Across a reflash that is what you want: nodes stay continuously present rather
than flapping. But when a board is **unplugged, bricked, or mid-swap**,
`ros2 node list` and publisher-matched counts still show the node alive,
indefinitely. The only observable signal is absence of data, which is
indistinguishable from a sensor with nothing to report.

Our runtime side *is* legible — `stay_resident` echoes every 5 s and bails after
two consecutive failures, so the proxy exits non-zero in ~10–15 s and the restart
policy fires — but that legibility does not reach ROS. **Anything safety-relevant
needs its own staleness check.** **[verified]**

**Transport reconnect is reliable and documented; graph reconnect is neither.**
`TermiosAgent::handle_error` is literally `return fini() && init();`
(`TermiosAgentLinux.cpp:179-183`), retried every 500 ms forever with no attempt
cap (`Server.cpp:293-311`). `init()` busy-waits on `access(dev, W_OK)` with no
timeout, so the agent survives resets and can be started before the MCU has ever
been flashed — upstream states that is the supported case. What is *not*
documented anywhere is what happens to the XRCE session, the DDS entities or the
ROS graph across a client reboot; that is only visible in source. **Test graph
semantics across a reflash on hardware; do not look them up.** **[verified]**

**Prefer one agent process per MCU.** `multiserial` erases a pending device
unconditionally after an open *attempt*
(`MultiTermiosAgentLinux.cpp:66-143`), so a port that fails its first open is
silently dropped for the life of the process, and ports cannot be added
dynamically. One-agent-per-MCU also matches this runtime's one-service-one-MCU
model and gives per-MCU restart granularity. **[verified]**

**Invocation**, for reference:

```bash
ros2 run micro_ros_agent micro_ros_agent serial --dev /dev/runtt/<tag>-ros
# or, standalone:
MicroXRCEAgent serial --dev /dev/runtt/<tag>-ros -b 921600
```

Transports accepted: `udp4 udp6 tcp4 tcp6 canfd serial multiserial
pseudoterminal`. Under `ros2 launch`, all agent arguments must be packed into a
**single string token** or the parser truncates them. Note `canfd` is compiled in
by default (`UAGENT_SOCKETCAN_PROFILE`), which is worth remembering given CAN is
already this project's planned second transport. **[verified]**

### The biggest risk

Not any of the above: **`ttyACM` renumbering across the deploy reset.** The Linux
cdc-acm driver releases a minor only from `acm_port_destruct` — the `tty_port`
destruct callback — so a minor stays allocated while any process holds the tty
open across a USB disconnect, and `acm_alloc_minor` takes the lowest free minor.
A micro-ROS agent still holding a third channel when the board detaches therefore
makes renumbering materially more likely **for all three channels, not just its
own**.

A udev symlink fixes the agent's half, because its `--dev` string is re-resolved
on every reconnect. It does **not** fix ours: `resolve.rs:129` and `:178`
canonicalize symlinks away at resolve time, and the runtime resolves once at
proxy startup. The failure would be silent and post-first-deploy — the ROS side
goes quiet with no error while the container still looks healthy. **[verified]**

This is the item to design for before a third channel exists, and it is
independent of everything micro-ROS-specific.

### If this is picked up, test in this order

1. Three CDC-ACM instances enumerating on RP2040 and nRF52840 under device_next,
   with `ID_PATH` stability for the existing two channels re-verified.
2. `libmicroros` linked for `cortex-m0plus` with `--gc-sections` and a real
   message set, for an honest RAM number.
3. Whether the runtime's resolve survives a deploy reset with a third holder
   attached — the renumbering risk above.
4. Only then: PR #163's alias hunk, pointed at the third CDC-ACM node.



---

*Co-authored with Claude*
