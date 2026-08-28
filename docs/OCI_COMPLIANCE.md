# OCI runtime compliance

`mcu-runtime` is an OCI runtime that does not run a container. It deploys
firmware to an attached microcontroller and then stays resident as the
container process. This document is the honest register of what it implements,
what it deliberately does not, and what the container engine was *observed* to
require — measured, not inferred.

## Implemented

| Verb | Notes |
|---|---|
| `create <id>` | Reads `config.json`, resolves the target from annotations, locates the firmware in the rootfs, claims exclusive occupancy, forks the resident proxy, writes `--pid-file`, exits 0. |
| `start <id>` | Signals the proxy (`SIGUSR1`) to begin work. Refuses any status but `created`. |
| `state <id>` | Prints OCI `State` JSON to stdout, reconciling recorded status against whether the proxy is actually alive. |
| `kill <id> [SIGNAL]` | Forwards the signal to the proxy. Accepts `TERM/KILL/INT/USR1/HUP/QUIT` by name, `SIG`-prefixed, or by number. |
| `delete <id> [--force]` | Refuses a running container without `--force`. Idempotent under `--force`, because containerd calls it more than once. |

## Deliberately not implemented

There is no namespace, cgroup, mount, seccomp, AppArmor or capability handling,
and no `exec`, `ps`, `pause`, `resume`, `update`, `events`, `checkpoint` or
`restore`. A firmware service has no processes to enter and no resources to
limit. `--console-socket` is accepted and ignored: containerd only passes it for
TTY containers, and `docker run -t` against a firmware service is meaningless.

Unlike `arm/remoteproc-runtime`, which drops stdio entirely because "firmware has
no standard I/O channels", **we keep stdio** — piping MCU log output to container
stdout is the point of the exercise.

## What the engine actually passes

Observed from Docker 28.5.2 / containerd `442cb34` / runc 1.3.3 on Ubuntu 24.04,
via `--mcu-trace`. Every invocation carried these globals:

```
--root /var/run/docker/runtime-runc/moby
--log  /run/containerd/io.containerd.runtime.v2.task/moby/<id>/log.json
--log-format json
--systemd-cgroup
```

Per-verb, as seen on the wire:

```
create --bundle <bundle> --pid-file <bundle>/init.pid <id>
start <id>
kill --all <id> 9
delete <id>
delete --force <id>
```

Three findings that a runtime must handle or fail opaquely:

1. **`--systemd-cgroup` is present on most calls but absent from the final
   `delete --force` cleanup call.** Do not require it.
2. **`kill` passes the signal as a bare number positionally** (`9`), with
   `--all`. Parse numeric signals, and accept-and-ignore `--all`.
3. **`delete` is called twice** — once normally, then again with `--force` on
   containerd's cleanup path. `delete --force` must therefore succeed when there
   is no state left to delete.

`state` was never invoked in an ordinary run-to-exit lifecycle; containerd
tracks the process itself. It must still work, but it is not on the hot path.

## Verified behaviour

Against real Docker, with a `FROM scratch` + `ADD app.signed.bin /` +
`ENTRYPOINT ["app.signed.bin"]` image:

- **Placement annotation arrives.** `docker run --annotation io.balena.mcu.target=usb:3-6`
  reaches `spec.annotations`. (On balena the supervisor delivers this from the
  target-state API instead; either way the runtime only reads the spec.)
- **Firmware resolves from the entrypoint**, inside the real overlay rootfs.
- **`process.terminal` is unset**, so stdio is pipes and whatever the proxy
  writes to fd 1/2 appears in `docker logs`. Confirmed across restarts.
- **A non-zero proxy exit drives the restart policy.** With
  `--restart on-failure:3`, `RestartCount=3` and `ExitCode=1`. This is the
  mechanism the whole "detach = non-zero exit" design depends on.
- **Exclusive occupancy holds.** The `flock` is owned by the open file
  description and inherited by the proxy, so it survives `create` exiting and is
  released by the kernel when the proxy dies. A second service targeting the same
  MCU is refused at `create`.

## Known gaps

- The SMP transport is not implemented yet; the proxy exits non-zero with a clear
  message. Everything above is the lifecycle skeleton around it.
- An **orphaned** proxy (parent died without the engine reaping it) holds its
  occupancy lock until killed. Under an engine the shim reaps it; standalone use
  needs manual cleanup.
- `--preserve-fds` is parsed but not acted on.

## Device acquisition: why `TIOCEXCL` is off

`serialport` enables `TIOCEXCL` by default. We turn it off, deliberately.

`TIOCEXCL` is a flag on the **terminal**, not on the file descriptor, and it is
only cleared once *every* fd to that tty has closed. So whenever anything else
holds the device open — native_sim holding its own pty, a log pump sharing a
single-channel link, a test harness, the mock — a process that exits leaves the
flag set, and the next open fails with `EBUSY`.

That failure mode is worst exactly where it hurts most: on a restart-policy
cycle, the engine immediately creates the replacement container, which cannot
open the device, exits non-zero, and restarts again. One crash becomes a
permanently unstartable service.

Nothing is lost by dropping it, because the protection it appeared to give is
provided properly elsewhere:

- **One service per MCU** comes from our own `flock` occupancy lock, which is
  held by the resident proxy's inherited file description.
- **Keeping ModemManager out of an in-flight upload** comes from
  `ID_MM_DEVICE_IGNORE=1` in the udev rules.

`TIOCEXCL` was protecting against neither, while adding a sticky failure mode.

Relatedly, `delete` waits (bounded) for the proxy to actually exit after
`SIGKILL` rather than returning as soon as the signal is delivered. Signalling is
asynchronous; releasing the device is what `delete` is promising.

And it kills the proxy whenever one is **alive**, not only when the container is
recorded as `running`. A container that was created but never started still has a
live proxy — blocked waiting for `start` — and that proxy holds the occupancy lock
on its MCU. Gating the kill on the recorded status leaked one process, and one
claimed device, per create-then-delete cycle. `--force` governs whether we refuse
to delete a *running* container; it does not govern cleanup.
