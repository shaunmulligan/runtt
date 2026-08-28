# Verifying the native_sim flow by hand

`scripts/native-sim-e2e.sh` automates this. This is the same thing step by step,
so you can watch each part and poke at it.

Every block of output below was captured from a real run, not reconstructed.
Your pty numbers and digests will differ.

## Why bother, given the script exists

The script asserts. This lets you *look*. Two things are worth seeing with your
own eyes because they're the load-bearing claims:

- the uploaded image is physically present in the simulated flash, byte for byte,
  checked from **outside** the device rather than by asking it;
- the runtime marks the image **test** and only ever confirms afterwards — the
  ordering that makes a broken image unable to confirm itself.

## Setup

```bash
cd ~/mcu-runtime
export PATH="$HOME/.cargo/bin:$HOME/.local/bin:$PATH"
export ZEPHYR_BASE="$PWD/zephyr" ZEPHYR_TOOLCHAIN_VARIANT=host

cargo build                      # the runtime
./scripts/build-native-sim.sh    # the firmware

mkdir -p /tmp/manual && cd /tmp/manual
```

If `build-native-sim.sh` fails, the west workspace probably isn't populated:
`west update --narrow -o=--depth=1` from the repo root.

---

## 1. Sign an image

The device validates the MCUboot header, so a raw binary is rejected. This is the
device being correct, not an obstacle.

```bash
head -c 8192 /dev/urandom > payload.bin

python3 ~/mcu-runtime/bootloader/mcuboot/scripts/imgtool.py sign \
  --key ~/mcu-runtime/bootloader/mcuboot/root-ec-p256.pem \
  --header-size 0x200 --pad-header --align 4 \
  --version 2.0.0 --slot-size 0x69000 \
  payload.bin app.signed.bin
```

`--pad-header` is required when signing a raw binary: without it imgtool refuses
with *"Header padding was not requested and image does not start with zeros"*,
because it expects the input to have already reserved header space.

The payload is random bytes on purpose. native_sim can never *execute* an image
regardless (see §9), so what matters is that the bytes form a valid MCUboot image.

## 2. Note the digest the device will report

```bash
python3 ~/mcu-runtime/bootloader/mcuboot/scripts/imgtool.py verify \
  --key ~/mcu-runtime/bootloader/mcuboot/root-ec-p256.pem app.signed.bin
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

## 3. Build an OCI bundle

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
  "annotations": { "io.balena.mcu.target": "tty:/tmp/manual/mgmt" }
}
JSON
```

Two fields carry all the meaning:

- `process.args` — the entrypoint names the firmware, which is how the runtime
  finds it inside the rootfs. Same convention as `arm/remoteproc-runtime`.
- `annotations` — the placement label. `tty:` is what makes a simulator or a
  probe's UART bridge addressable; on real hardware you'd use `usb:3-6`.

## 4. Start native_sim

```bash
~/mcu-runtime/build/zephyr/zephyr.exe \
  --uart_attach_uart_cmd='ln -sf %s /tmp/manual/mgmt' \
  --uart_1_attach_uart_cmd='ln -sf %s /tmp/manual/log' \
  --flash=/tmp/manual/flash.bin &
```

```
uart connected to pseudotty: /dev/pts/284
uart_1 connected to pseudotty: /dev/pts/285
*** Booting Zephyr OS build dccb09599635 ***
[00:00:00.000,000] <inf> app: balena-mcu template app 0.1.0 starting on native_sim/native/64
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

## 5. Confirm logs arrive on the log channel

```bash
timeout 5 cat log
```

```
*** Booting Zephyr OS build dccb09599635 ***
[00:00:00.000,000] <inf> app: balena-mcu template app 0.1.0 starting on native_sim/native/64
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

## 6. Look at the flash before deploying

```bash
~/mcu-runtime/scripts/flash-inspect.py flash.bin
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

## 7. Deploy

The two verbs a container engine would call. `create` forks the resident proxy and
exits; `start` releases it to do the work.

```bash
cd ~/mcu-runtime
BIN=./target/debug/mcu-runtime
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
        "io.balena.mcu.firmware-path": "/tmp/manual/bundle/rootfs/app.signed.bin",
        "io.balena.mcu.target": "tty:/tmp/manual/mgmt"
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
INFO mcu_runtime::proxy: resolved target mgmt=/tmp/manual/mgmt log=None
mcu: single channel; application logs share the management link
INFO mcu_runtime::flash: deploying firmware target="tty:/tmp/manual/mgmt" bytes=8854
     version=2.0.0+0 digest="a1ae4888bc70c7ca663c6d5f8d4bb8f125b25cdf6a9ab7ccb92a3eb899f47ff4"
mcu: uploading 8854/8854 bytes (100%)
WARN mcumgr_toolkit::client: Device did not perform image checksum verification
mcu: image staged and marked test, resetting
mcu-runtime: the image is staged and marked pending, but nothing swapped it in: no image
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

## 8. Verify the upload actually landed

The claim worth checking independently. `flash.bin` is a plain host file:

```bash
~/mcu-runtime/scripts/flash-inspect.py /tmp/manual/flash.bin \
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

## 9. Confirm the reset really happened

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

## 10. Clean up

```bash
./target/debug/mcu-runtime --root /tmp/manual/state delete --force manual
kill %1      # the simulator
```

`delete` waits for the proxy to actually exit rather than returning as soon as the
signal is sent — otherwise the device would still be open when a replacement
container tried to claim it, which is exactly what a restart policy would do.

---

## What this does and doesn't prove

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

## The fast path: skip Zephyr entirely

For error paths, the mock is quicker and can inject faults on demand:

```bash
./target/debug/smp-mock --symlink /tmp/mock-tty &
# then point a bundle at tty:/tmp/mock-tty and deploy as above

./target/debug/smp-mock --help          # the available faults
./target/debug/smp-mock --fault bad-hash --symlink /tmp/mock-tty
```

Unlike native_sim the mock *does* model swap and revert, so it can show you an
unconfirmed image rolling back — which is the case real hardware is needed for
otherwise. `cargo test -p smp-mock` covers that state machine directly.

---

## Troubleshooting

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
pinned equal in `balena-mcu.conf`; if you change one, change both. Also note each
received *line* holds a whole RX buffer until reassembly, so `RX_BUF_COUNT` has to
cover lines-per-packet — not merely satisfy the documented
`COUNT * SIZE >= MTU`.

**Slot 1 empty after a reset** — `--flash_erase` was passed. `os reset` preserves
argv, so it re-fires every reboot. Delete `flash.bin` instead.

**`Device or resource busy` opening the pty** — something still holds it. A
previous proxy may be orphaned: `pgrep -af 'mcu-runtime.* proxy'`, then kill it.
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

*Co-authored with Claude*
