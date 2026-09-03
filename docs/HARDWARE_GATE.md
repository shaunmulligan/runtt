# Hardware CI gate — design, and why it doesn't exist yet

**Status: not built. Deliberately.** CI is simulated-only, and that is a
considered choice rather than a gap we haven't got to.

This document exists so that whoever builds the hardware gate doesn't rediscover
the traps from scratch. Everything below was verified against this repo and a
Pico on the bench; where something is inferred rather than run, it says so.

## What CI covers today, without any hardware

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

## What only hardware can prove

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

## The trap that makes a naive gate worthless

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

### The fix, which solves several problems at once

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

## The second trap: exit-code polarity inverts

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

## Testing revert without a human, and without power control

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

## What the gate must never do

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

## Two gaps to close before writing the gate

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

## Resolved: the RP2040 "swap bug" was a malformed test image

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

### What this cost, and the lesson

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
`docs/MCUBOOT_SWAP_BUG.md`.

## Staging

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

## ⚠️ If this ever becomes a CI job

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

*Co-authored with Claude*
