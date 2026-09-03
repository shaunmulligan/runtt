# Contributing to runtt

## The short version

```bash
cargo test --workspace          # 83 tests, no hardware needed
cargo clippy --all-targets      # must be clean, not merely warning-free-ish
./scripts/native-sim-e2e.sh     # the real runtime against real Zephyr firmware
```

If those three pass, a change is in reasonable shape. Everything below is detail.

## What this project cares about

**Claims must be verified, not inferred.** A flag existing does not prove its
effect; a config symbol being set does not prove the code path runs. If a commit
message says something works, something ran. Where a claim is reasoned rather
than observed, say so — several comments in this codebase exist because an earlier
inference turned out to be wrong, and the comment is cheaper than the rediscovery.

**Comments explain why, not what.** The interesting content in this repo is the
recorded reasons: why `bs = 0` on the ISO-TP flow control, why the CAN log backend
batches into full frames, why a damaged identity record refuses to boot CAN while
an absent one does not. Those exist because each was a bug that cost real time.
Keep that habit; it is most of the value here.

**The wire contract is a contract.** `docs/WIRE_CONTRACT.md` is the interface
between two independently versioned parties — a runtime and firmware that may ship
from different people. `crates/runtt/tests/contract_version.rs` fails the build if
the document, the firmware Kconfig, the runtime's accepted major and the mock
disagree. That test is not an obstacle; it is the reason the version means
anything.

## Testing

Three layers, and they cover different things:

| Layer | Command | Covers |
|---|---|---|
| Unit and integration | `cargo test --workspace` | logic, parsing, the SMP mock's fault paths |
| `native_sim` gates | `scripts/native-sim-*-e2e.sh` | the real runtime driving real Zephyr firmware, over serial and over CAN |
| MCUboot simulator | see `.github/workflows/ci.yml` | swap, revert and power-fail, which no hardware loop of ours could reach |

The CAN gate needs a virtual bus; it **skips** rather than fails when one is
absent, so set it up if you are touching that path:

```bash
sudo modprobe vcan can-isotp
sudo ip link add dev vcan0 type vcan
sudo ip link set vcan0 up
sudo ip link property add dev vcan0 altname zcan0   # native_sim hardcodes this name
```

Hardware is not required to contribute, and most of the codebase is reachable
without it. If you do have a board, [`WALKTHROUGH.md`](https://github.com/shaunmulligan/runtt-examples/blob/main/docs/WALKTHROUGH.md) and
`docs/MANUAL_VERIFICATION.md` are the paths to walk.

## Changing something on the wire

The USB interface descriptors, the annotation namespace, the SMP group id, the
image semantics and the identity record layout are all contract. Changing any of
them means:

1. Update `docs/WIRE_CONTRACT.md`, including the version history table.
2. Bump the contract version — major if a v1 host and a v2 board can no longer
   talk, which is usually the case.
3. Update the firmware Kconfig default, the runtime's accepted major, and the mock.
4. Say in the commit message what an old board does when it meets a new host. The
   answer should be a legible failure, not a mystery.

The identity record layout has two implementations —
`include/runtt/identity.h` and `scripts/make-identity.py` in runtt-zephyr-module — and
they must agree. The CAN gate exercises both ends, so a mismatch fails there.

## Zephyr

Zephyr is pinned exactly in `runtt-boards`' `west.yml`, currently v4.4.2 — the
firmware side lives there now. Treat a bump as
a deliberate, tested step: minor releases have already moved things underneath
this project twice in ways that mattered. The `native_sim` gates are the upgrade
gate.

Prefer current APIs over deprecated ones even when the deprecated one still
compiles — `PARTITION_ID` over `FIXED_PARTITION_ID`, for instance. Deprecation
warnings are a bump that will fail later.

## Commit messages

Explain the change and the reasoning. If a bug was subtle, record what made it
subtle, because that is what stops the next person reintroducing it. If something
was measured, give the number.

## Licence

By contributing you agree that your contribution is dual licensed under
Apache-2.0 and MIT, matching the project. No CLA, no copyright assignment.
