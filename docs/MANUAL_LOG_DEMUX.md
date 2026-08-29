# Verifying the single-channel log demux by hand

What this checks: on a target where the application's console output shares the
SMP management link, those log lines still reach the container's stdout — and
the deploy still works while they do.

Before the demux existed, a single-channel container deployed fine and printed
**nothing**. `mcumgr-toolkit`'s frame reader scans forward for the framing
marker and silently discards everything it steps over, so the application's
output was dropped on the floor. Worse, the runtime announced that logs were
sharing the link, so the gap looked like intended behaviour.

Three routes below, cheapest first. Each is self-contained.

---

## 1. Two minutes, no Zephyr: the mock

`smp-mock --chatter` emits application log lines on the same link it serves SMP
on, which is exactly the single-channel shape.

```bash
cargo build --workspace

# A device that talks SMP *and* prints.
./target/debug/smp-mock --symlink /tmp/mcu-tty --chatter '<inf> app: alive tick' &
PTS=$(readlink -f /tmp/mcu-tty)

# A bundle pointing at it. tty: targets are single-channel by definition.
mkdir -p /tmp/demo/rootfs
cp crates/smp-client/tests/fixtures/app.signed.bin /tmp/demo/rootfs/
cat > /tmp/demo/config.json <<JSON
{ "ociVersion": "1.2.0",
  "process": { "user": {"uid":0,"gid":0}, "args": ["app.signed.bin"],
               "cwd": "/", "terminal": false },
  "root": { "path": "rootfs", "readonly": true },
  "annotations": { "io.balena.mcu.target": "tty:$PTS" } }
JSON

./target/debug/mcu-runtime --root /tmp/demo-state create --bundle /tmp/demo one
./target/debug/mcu-runtime --root /tmp/demo-state start one
sleep 8
./target/debug/mcu-runtime --root /tmp/demo-state delete --force one
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

## 2. Real Zephyr firmware on a genuine single-channel link

The mock is a mock. This runs actual Zephyr with the console and the SMP server
sharing one UART, which is what an ESP32-C3 class part or a probe-UART bring-up
looks like.

`CONFIG_BALENA_MCU_CHANNELS=1` alone is **not** enough — it is declarative, and
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
west build -p always -b native_sim/native/64 --snippet balena-mcu firmware/app \
  -d /tmp/build-1ch -- \
  -DZEPHYR_EXTRA_MODULES="$PWD/firmware/balena-mcu" \
  -DEXTRA_DTC_OVERLAY_FILE=/tmp/single-channel.overlay \
  -DCONFIG_BALENA_MCU_CHANNELS=1
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
mcu: device is native_sim/native/64 running 0.1.0 (contract 1.2.0, 1 channel)
*** Booting Zephyr OS build dccb09599635 ***
[00:00:00.000,000] <inf> app: balena-mcu template app 0.1.0 starting on native_sim/native/64
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

## 3. Building the firmware image with Docker

The firmware service is an ordinary container image: Zephyr toolchain in stage
one, `FROM scratch` carrying only the signed image in stage two.

The build environment comes from a **builder image**, built once, which is what
lets an application directory be self-contained:

```bash
docker build -f firmware/builder/Dockerfile -t balena-mcu-builder:v4.4.2 firmware/
```

Then build the application from inside its own directory:

```bash
cd firmware/examples/app1
podman build --build-arg BOARD=rpi_pico/rp2040/mcuboot -t mcu-fw:pico .
```

Then deploy it exactly like any other image:

```bash
podman --runtime="$PWD/target/debug/mcu-runtime" run --rm \
  --annotation io.balena.mcu.target=usb:3-4 \
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
> * `-DZEPHYR_EXTRA_MODULES=/ws/balena-mcu`. The module lives *inside* the
>   manifest repo, and west only auto-discovers a `module.yml` at a project's
>   root — ours is nested, so without this the module and its snippet simply are
>   not there. Both build scripts carry the same line.
> * `-Dapp_SNIPPET=` rather than `-S`. Under sysbuild a top-level snippet
>   applies to **every** image, including MCUboot. See `docs/FIRMWARE_GUIDE.md`.

**Expect the builder image to be slow the first time** — the Zephyr CI base is
tens of gigabytes, and it then fetches Zephyr and its modules. That cost is paid
once; application builds afterwards are quick. For iterating on firmware,
`scripts/build-pico.sh` against a local west workspace is quicker still; the
Docker path is for producing the artefact you actually ship.

See `docs/WALKTHROUGH.md` for this build path end to end on hardware.

---

## 4. On hardware

Both boards we ship are **two-channel**, so they take the plain path and the
demux is not involved. To exercise it on real hardware, address the management
channel directly with a `tty:` target, which makes the host treat it as
single-channel:

```bash
MGMT=$(readlink -f /dev/balena-mcu/*-mgmt)
podman --runtime="$PWD/target/debug/mcu-runtime" run --rm \
  --annotation io.balena.mcu.target="tty:$MGMT" \
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

## Troubleshooting

**`Unable to acquire exclusive lock on serial port`** — something still holds
the device. `serialport` takes an `flock(LOCK_EX)` on open, so a leftover proxy
from a failed run keeps it. `pgrep -af mcu-runtime` and
`fuser -v /dev/ttyACM*`, then `delete --force` the container.

**The deploy works but no log lines appear** — check the runtime actually said
`single channel`. If it resolved two channels (`log=Some(...)`) it took the
plain path and the demux was never involved.

**Nothing at all on hardware, and USB still enumerates** — the board's firmware
can be wedged while its USB stack still answers control transfers, so `lsusb`
looks healthy and both SMP and logs are silent. There is no software route into
BOOTSEL on RP2040 in the images we ship, so recovery is a physical replug.

---

*Co-authored with Claude*
