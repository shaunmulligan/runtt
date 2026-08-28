# The balena MCU wire contract

**Version 1.0.0**

The runtime and the firmware ship from different parties. The runtime is a host
binary delivered with the OS; the firmware is a container image the customer
builds. They version independently, and nothing forces them to agree — so this
document, not the code, is the interface.

Everything here is implemented and verified against Zephyr v4.4.2 and
`mcumgr-toolkit` 0.16.0 unless explicitly marked otherwise.

---

## 1. What a developer has to do

The honesty test for this contract: a competent Zephyr developer with existing
firmware should get onto the platform by adding a module and one build flag.

```bash
west build -b <board> --sysbuild -S balena-mcu app/
```

That is the whole developer surface. The module supplies the SMP server, the two
channels, the image group and the `describe` command. **No source changes, and no
mandatory C API.**

Opt-in Kconfig:

| Symbol | Default | Meaning |
|---|---|---|
| `BALENA_MCU_CHANNELS` | 2 | 1 for single-serial targets (see §3) |
| `BALENA_MCU_IMG_MGMT` | follows `slot1_partition` | the update half of the contract |
| `BALENA_MCU_SMP_DESCRIBE` | y | the identity command (§6) |
| `BALENA_MCU_CONTRACT_VERSION` | `"1.0.0"` | what `describe` reports |
| `BALENA_MCU_HEALTH` | **n** | application liveness (§7) |

The only C API is one function, and it is optional:

```c
void balena_mcu_health_feed(void);
```

> With `--sysbuild`, pass the snippet as `-Dapp_SNIPPET=balena-mcu` rather than
> `--snippet`. Sysbuild applies `--snippet` to **every** image, which would
> enable MCUmgr, this module and a dual-CDC composite inside MCUboot.

## 2. Contract versioning

`describe` reports a semver string. **The runtime refuses to write to a device
whose contract MAJOR differs from the one it implements.** Minor and patch
differences are tolerated in both directions.

A device that does not answer `describe` at all is *tolerated with a warning*,
because the `os` and `img` groups are standard MCUmgr and such a device is still
manageable — just unidentified. What is refused is a device that positively
declares an incompatible major.

## 3. Transport and channel identity

Two channels, and **channel identity comes from the USB interface string
descriptor**:

| Channel | Interface string | Carries |
|---|---|---|
| management | `balena-mcu-mgmt` | SMP |
| log | `balena-mcu-log` | application output, verbatim |

Set declaratively in devicetree; the `zephyr,cdc-acm-uart` binding's `label`
becomes `iInterface`:

```dts
&zephyr_udc0 {
	cdc_acm_mgmt: cdc_acm_mgmt {
		compatible = "zephyr,cdc-acm-uart";
		label = "balena-mcu-mgmt";
	};
	cdc_acm_log: cdc_acm_log {
		compatible = "zephyr,cdc-acm-uart";
		label = "balena-mcu-log";
	};
};
/ {
	chosen {
		zephyr,uart-mcumgr = &cdc_acm_mgmt;
		zephyr,console     = &cdc_acm_log;
	};
};
```

**Why string descriptors and not VID/PID or interface numbers.** Customers may
ship their own VID, so VID/PID is not ours to specify. Interface *numbers* are
not contractual either: `ID_PATH` is interface-suffixed, so the two channels of
one composite device land on different `ID_PATH`s, and the numbering is an
artefact of how the composite happens to be assembled. The string descriptor is
the part of the USB identity the firmware contract genuinely controls.

### Single-channel variant

`BALENA_MCU_CHANNELS=1` is legitimate for targets with one serial interface
(ESP32-C3 class) and for bring-up over a debug probe's UART bridge. Log output
then shares the management link, which the console framing (§4) tolerates —
lines that are not SMP frames are log text.

**Do not use the raw UART transport** (`CONFIG_MCUMGR_TRANSPORT_RAW_UART`) for a
contract device. It is faster, but it cannot share a line with log output, so it
forecloses the single-channel variant.

## 4. SMP framing

Standard Zephyr SMP over the console transport. Restated because the details are
easy to invert and expensive to debug:

```
line := marker || base64( len_be16 || body || crc16_be16 ) || '\n'
body := smp_header(8) || cbor_payload
```

* `MARKER_START = 0x06 0x09` on the first line of a packet;
  `MARKER_CONT = 0x04 0x14` on every continuation.
* **127 bytes maximum per line**, marker and newline included.
* CRC16-XMODEM (poly `0x1021`, init `0x0000`) covers **the body only** — not the
  length prefix — and is itself covered by the length.
* `len` counts `body + 2` (the CRC) and does **not** count itself.

Byte 0 of the header is a bitfield, and not the one a casual reading suggests.
Zephyr's `struct smp_hdr` on little-endian is `nh_op:3, nh_version:2, _res1:3`:

```
op      = byte0 & 0x07          (3 bits, not 4)
version = (byte0 >> 3) & 0x03
```

Measured on the wire: a read is `0x08`, a write is `0x0a` — both **version 1
(SMP v2)**. The version must be echoed in responses or a v2 client rejects them
as unexpected.

Errors follow the request's dialect: v2 uses `{"err": {"group": g, "rc": n}}`,
v0/v1 the flat `{"rc": n}`.

### Buffer sizing

`MCUMGR_TRANSPORT_UART_MTU` **must equal** `MCUMGR_TRANSPORT_NETBUF_SIZE`. The
MCUmgr-parameters command reports `NETBUF_SIZE`, and clients size their frames
from it, but the UART transport enforces its own MTU (default 256). Mismatched,
the client sends frames the transport silently drops — visible only as
`uart_mcumgr: Insufficient buffers, fragment dropped` on the device and a
timeout on the host.

Zephyr documents the buffer constraint as `RX_BUF_COUNT * RX_BUF_SIZE >= MTU`.
That is necessary but **not sufficient**: each received *line* occupies a whole
buffer until the packet is reassembled, so `RX_BUF_COUNT` must also cover the
lines per packet — `ceil(MTU * 4/3 / 124)`.

## 5. Mandatory command groups

| Group | Id | Required commands |
|---|---|---|
| `os` | 0 | echo (0), reset (5), MCUmgr parameters (6) |
| `img` | 1 | state read/write (0), upload (1) |
| balena | 64 | `describe` (0) |

`os` echo is the heartbeat. MCUmgr parameters is required because without it
every client silently falls back to assumed frame sizes.

`img` is required only where the board has a secondary slot to stage into. A
board without one (`BALENA_MCU_IMG_MGMT=n`) is a **bring-up configuration**: it
can be identified and its logs read, but it cannot receive an update. It is not
a shippable configuration.

Enable `MCUMGR_GRP_ENUM` (group 10) so a client can discover group 64 rather
than probing blindly.

## 6. `describe`

Group **64** (`MGMT_GROUP_ID_PERUSER`, the first id reserved for applications),
command **0**, op **read**. Request is an empty CBOR map. Response:

| Key | Type | Meaning |
|---|---|---|
| `contract` | tstr | this document's version, e.g. `"1.0.0"` |
| `board` | tstr | `CONFIG_BOARD_TARGET`, e.g. `rpi_pico/rp2040` |
| `app_version` | tstr | the application's own version |
| `channels` | uint | 1 or 2, per §3 |
| `app_healthy` | bool | **present only if** `BALENA_MCU_HEALTH=y` (§7) |

The runtime calls this **after echo and before any write**, and logs the result.

Its purpose is to make failures legible. Over a serial line almost every failure
is a timeout, and a timeout is indistinguishable between wrong port, unpowered
board, wedged board, firmware without SMP, and contract skew. `describe` turns
that into a positive identification, cheaply.

The `board` field earns its place on its own: placement is a USB **port path**,
which is physical rather than an identity. Re-cable a hub and the label still
resolves while pointing at a different MCU — this is the check that stops nRF
firmware reaching an RP2040.

`app_healthy` is deliberately absent rather than `false` when the feature is
off, so a host can distinguish *unhealthy* from *does not report health*.

> Future: role-name identity belongs here, so a service can target "left wheel
> motor controller" rather than `usb:1-1.2`. Not in 1.0.0.

## 7. Liveness

SMP echo proves the **kernel** is scheduling. It does not prove the application
thread is alive — an app deadlocked in its own logic answers echo perfectly.

`BALENA_MCU_HEALTH=y` plus calling `balena_mcu_health_feed()` from the
application's main loop extends the host's confirm gate from "kernel alive" to
"application alive". Optional, and firmware that never calls it reports healthy
rather than failing closed.

## 8. Image semantics

Images are **MCUboot images**: signed, versioned, with a TLV area. A raw binary
is rejected with `IMG_MGMT_ERR_INVALID_IMAGE_HEADER_MAGIC`, which is the device
behaving correctly.

### Two hashes, and they are not interchangeable

* The upload's `sha` field is the **SHA-256 of the image file** — transfer
  integrity and resumption.
* Image **identity** is the **MCUboot digest**, from the image's SHA256 TLV,
  covering header and body only. This is what `img state` reports and what
  `set_state` matches on.

Using the file hash for `set_state` yields `IMG_MGMT_ERR_HASH_NOT_FOUND`, and
that error does not hint at why.

### The deploy sequence, which is the safety property

```
upload            -> secondary slot
set_state(TEST)      never confirm here
reset
   wait for the device to enumerate, speak SMP and heartbeat
set_state(CONFIRM)   only now
```

**Confirmation is reachable only through the contract.** An image that removed or
broke the contract can never be confirmed, because confirming requires the very
capability that was lost. If the confirm never arrives, MCUboot reverts on the
next reset. Contract loss is therefore never remotely permanent, by construction.

### Swap mode

Pin it: `SB_CONFIG_MCUBOOT_MODE_SWAP_USING_OFFSET=y`. The default changed
recently, and a bootloader built for one mode cannot boot an image built for
another ([zephyr#98050](https://github.com/zephyrproject-rtos/zephyr/issues/98050)).
Sysbuild builds both together so they always agree locally — the pin exists so a
Zephyr bump cannot silently change the on-device contract.

### Signing

> ⚠️ **The build default is not safe to ship.** Sysbuild silently selects RSA-2048
> with MCUboot's `root-rsa-2048.pem`, which is the **private** key and is
> committed in the public MCUboot repository.
>
> This fails safely-looking: the image really is signed, and `imgtool verify`
> reports "correctly validated". But flashing MCUboot is what enrols the trust
> root — the embedded key defines whose firmware the board will ever accept.
> With a publicly known private key, no trust is enrolled: anyone who can reach
> the SMP transport can push firmware the bootloader will verify and boot.
>
> Before any fleet: generate a per-fleet key pair, keep the private half out of
> the repo, and point `SB_CONFIG_BOOT_SIGNATURE_KEY_FILE` at it from the build
> environment. Note the rotation constraint — the public half is baked into
> MCUboot at provisioning time, so changing keys means re-flashing over SWD.
> Key management is a provisioning decision, not a build-time one.

## 9. Placement labels

Transport-prefixed from the outset, so other transports slot in without breaking
existing labels:

```
io.balena.mcu.target: usb:3-6            # kernel USB port path
io.balena.mcu.target: tty:/dev/ttyACM0   # bare serial, a simulator's pty, a probe's bridge
io.balena.mcu.target: can:vcan0/0x42     # named, not implemented in 1.0.0
```

An unprefixed label is an **error**, not a guess. `io.balena.mcu.skip-if-same-hash`
(default on) suppresses redeploying an image the device already runs, confirmed.

How the label reaches the runtime is out of scope here: it arrives as an OCI
annotation on the container spec, and who puts it there is the engine's business.

## 10. udev

Rules key off the interface string descriptors, never VID/PID, and are numbered
after systemd's `60-serial.rules` because they consume its `ID_PATH_TAG`:

```udev
SUBSYSTEM=="tty", ATTRS{interface}=="balena-mcu-mgmt", \
  ENV{ID_MM_DEVICE_IGNORE}="1", SYMLINK+="balena-mcu/$env{ID_PATH_TAG}-mgmt"
```

`ID_MM_DEVICE_IGNORE=1` is load-bearing, not defensive. ModemManager probes new
CDC-ACM devices with AT commands; one probe landing mid-upload is a corrupted
transfer and a genuine heisenbug. It has been observed tagging a contract-shaped
device `ID_MM_CANDIDATE=1` on an ordinary Ubuntu workstation.

The resulting `/dev/balena-mcu/` tree doubles as the runtime's discovery
inventory, and keeps arbitrary customer serial devices out of scope by
construction.

## 11. Deliberately not in the contract

No MCU-side networking. No power control — hard recovery of a wedged board is
user-wired via loom GPIO, not platform code. No wasm. No log streaming over SMP
(Zephyr has none; `MGMT_GROUP_ID_LOG` is marked unused, which is why the second
channel exists). No serial recovery for bricked boards
(`CONFIG_BOOT_SERIAL_CDC_ACM`): it is parked, and note it currently depends on
MCUboot's legacy USB stack, which Zephyr removes in 4.5
([mcuboot#2596](https://github.com/mcu-tools/mcuboot/issues/2596) is open).

Keeping MCUboot's USB disabled entirely is what makes that removal a non-event
for us.

## 12. Version history

| Version | Changes |
|---|---|
| 1.0.0 | Initial contract: dual CDC-ACM with string-descriptor identity, SMP over console framing, os/img groups, `describe` at group 64, optional health. |

---

*Co-authored with Claude*
