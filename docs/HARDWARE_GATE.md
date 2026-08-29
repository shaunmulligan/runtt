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
| `test` | fmt, clippy, the Rust suite, and a podman run of a real `FROM scratch` image against `smp-mock` over a `tty:` target — including that **mark-test precedes confirm**, asserted by line order, not just by both appearing |
| `native-sim` | the real Zephyr SMP server, two channels, upload, trailer write, reset, reconnect — driven directly *and* through podman |
| `mcuboot-sim` | swap, revert and confirm under **injected power failures**, using MCUboot's own Rust simulator over the real `bootutil` sources |

That is a lot of the risk surface, and none of it needs a board. The gap is
genuinely narrow — but it is not empty.

> **Note:** as of this writing the repo has **no git remote and no tags**, so
> `ci.yml` has never actually executed anywhere. It is a well-formed file, not a
> running system. Any plan that starts "add a hardware job to CI" is really a
> three-step plan: push to a remote, get the three headless jobs green on hosted
> runners, and only then attach a runner to a desk machine.

## What only hardware can prove

* USB enumeration, and interface-string-descriptor matching — the board
  enumerates as `2fe3:0004 NordicSemiconductor balena MCU device`, an RP2040
  claiming to be Nordic on Zephyr's VID. Nothing about that is findable by
  VID/PID; the descriptor is the whole identity.
* Port-path → target label resolution (`usb:3-4`).
* Real MCUboot swap on RP2040 over QSPI, and its timing.
* Actual link throughput, and whether SMP timeouts hold at real speed.
* Replug and device-disappears-mid-operation behaviour.
* **Revert.** Asserted nowhere in this repo today: `DigestAlreadyFailed` is
  exercised only inside `smp-mock`'s own unit test, never through the runtime.

## The trap that makes a naive gate worthless

**A hardware gate is not idempotent, and the obvious version silently no-ops
from the second run onwards.**

`skip_if_same` defaults to **true** (`crates/mcu-runtime/src/verbs.rs:96`,
`.unwrap_or(true)`), and rebuilding from an unchanged tree produces a
byte-identical `zephyr.signed.bin`. So `crates/mcu-runtime/src/flash.rs:102-109`
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
for RP2040 (`build-pico-mcuboot/app/zephyr/zephyr.dts:100` — *not* native_sim's
`0x69000`).

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

## Open bug: a flash write followed by `os reset` wedges an RP2040

Found while trying to run the gate's own happy path by hand, 2026-08-29. **This
is unresolved**, and it blocks every hardware deploy on the Pico.

### What is established

Isolated by bisecting the deploy sequence on a freshly provisioned board, each
step run on its own:

| Sequence | Result |
|---|---|
| `os reset` alone | **reboots cleanly** — USB re-enumerates, board returns healthy |
| `set_state(test)` alone | **fine** — `pending=true` reads straight back, board stays responsive |
| `set_state` → `os reset` | **wedges** |
| upload → `set_state` → `os reset` | **wedges** |

So neither operation is faulty on its own. It is **a flash write followed by a
reset**, and it reproduces every time.

### What the wedged state looks like

The confusing part, and worth knowing before you chase it:

* The device **does not re-enumerate** — same USB device number throughout, so
  the reboot never happened.
* USB **control transfers keep working**. `lsusb -v` reads `iProduct` and
  `bcdDevice` live from the device, so it looks present and healthy.
* Both CDC channels are dead: no SMP, no log output.
* MCUboot never ran, so nothing swaps, and `pending` is clear on the next power
  cycle — which makes it look from the host like `set_state` silently failed,
  when in fact it succeeded and the reboot never came.
* Recovery is a **physical replug**. There is no software route back.

Interrupts are evidently still being serviced, since USB answers; it is the
threads that have stopped. That rules out a simple `irq_lock()` leak in the
flash driver's write path, which would have killed USB too.

### What has been ruled out

* **Not the log demux, and not anything in this cycle's work.** It reproduces on
  the two-channel path, where `demux_logs` is false and the code is byte-for-byte
  what it was before.
* **Not a system workqueue stack overflow.** That hypothesis fit every symptom —
  CDC-ACM work and MCUmgr's reset handler share the system workqueue, USB
  interrupts run on their own stack, and only `balena-mcu-idle` hit it because
  `firmware/app/prj.conf` happens to raise the stack to 2560. Raising it in the
  module changed nothing. The fit was a coincidence.
* **Not the reset command being rejected.** `reset()` returns success; the
  failure surfaces later, in `reconnect()`.

### Where to look next

The next instrument is **SWD, not another guess**. Four boards' worth of replugs
went into narrowing this by inference; attaching the Debug Probe and reading
where the core actually sits would settle in one go what the host cannot see.

Two starting points worth checking:

* RP2040's reset is the generic Cortex-M `NVIC_SystemReset` — there is no
  SoC-specific `sys_arch_reboot` under `zephyr/soc/raspberrypi`. A core-only
  reset does **not** reset the external QSPI flash chip, so any mode the flash
  was left in by `flash_range_program` survives into the bootrom's first reads.
* `CONFIG_MCUMGR_GRP_OS_RESET_MS` is 250 ms by default. If the reboot work is
  racing the flash driver rather than being blocked by it, that value is the
  cheapest thing to vary.

Until this is understood, **a hardware gate cannot run its happy path on
RP2040**, which is a good argument for keeping the gate a bench script that a
human starts rather than anything automated.

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
software route already exists in the vendored tree
(`zephyr/soc/raspberrypi/rpi_pico/common/rom_bootloader.c`), it is simply not
enabled.

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
