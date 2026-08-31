# The runtt wire contract

**Version 2.0.0**

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
west build -b <board> --sysbuild app/ -- -Dapp_SNIPPET=runtt
```

That is the whole developer surface. The module supplies the SMP server, the two
channels, the image group and the `describe` command. **No source changes, and no
mandatory C API.**

Opt-in Kconfig:

| Symbol | Default | Meaning |
|---|---|---|
| `RUNTT_CHANNELS` | 2 | 1 for single-serial targets (see §3) |
| `RUNTT_IMG_MGMT` | follows `slot1_partition` | the update half of the contract |
| `RUNTT_SMP_DESCRIBE` | y | the identity command (§6) |
| `RUNTT_CONTRACT_VERSION` | `"2.0.0"` | what `describe` reports |
| `RUNTT_HEALTH` | **n** | application liveness (§7) |

The only C API is one function, and it is optional:

```c
void runtt_health_feed(void);
```

> With `--sysbuild`, pass the snippet as `-Dapp_SNIPPET=runtt` rather than
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
| management | `runtt-mgmt` | SMP |
| log | `runtt-log` | application output, verbatim |

Set declaratively in devicetree; the `zephyr,cdc-acm-uart` binding's `label`
becomes `iInterface`:

```dts
&zephyr_udc0 {
	cdc_acm_mgmt: cdc_acm_mgmt {
		compatible = "zephyr,cdc-acm-uart";
		label = "runtt-mgmt";
	};
	cdc_acm_log: cdc_acm_log {
		compatible = "zephyr,cdc-acm-uart";
		label = "runtt-log";
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

`RUNTT_CHANNELS=1` is legitimate for targets with one serial interface
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
board without one (`RUNTT_IMG_MGMT=n`) is a **bring-up configuration**: it
can be identified and its logs read, but it cannot receive an update. It is not
a shippable configuration.

Enable `MCUMGR_GRP_ENUM` (group 10) so a client can discover group 64 rather
than probing blindly.

## 6. `describe`

Group **64** (`MGMT_GROUP_ID_PERUSER`, the first id reserved for applications),
command **0**, op **read**. Request is an empty CBOR map. Response:

| Key | Type | Meaning |
|---|---|---|
| `contract` | tstr | this document's version, e.g. `"2.0.0"` |
| `board` | tstr | `CONFIG_BOARD_TARGET`, e.g. `rpi_pico/rp2040` |
| `app_version` | tstr | the application's own version |
| `channels` | uint | 1 or 2, per §3 |
| `img` | bool | whether the image group is implemented, i.e. whether this device can be updated at all |
| `idle` | bool | true only for `runtt-idle`, the provisioning placeholder — the board is working but has never received firmware |
| `app_healthy` | bool | **present only if** `RUNTT_HEALTH=y` (§7) |

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
off, so a host can distinguish *unhealthy* from *does not report health*. The
same reasoning applies to `img` across contract versions: absent means the
firmware predates the field, not that the device lacks image management.

A device reporting `img: false` is refused **before** any upload is attempted,
with a message naming the remedy. It is a bring-up configuration (§5), not a
broken one.

> Future: role-name identity belongs here, so a service can target "left wheel
> motor controller" rather than `usb:1-1.2`. Not in this version.

## 7. Liveness

SMP echo proves the **kernel** is scheduling. It does not prove the application
thread is alive — an app deadlocked in its own logic answers echo perfectly.

`RUNTT_HEALTH=y` plus calling `runtt_health_feed()` from the
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
dev.runtt.target: usb:3-6            # kernel USB port path
dev.runtt.target: usb:feather-01     # ...or the board's own serial
dev.runtt.target: tty:/dev/ttyACM0   # bare serial, a simulator's pty, a probe's bridge
dev.runtt.target: can:vcan0/0x42     # SocketCAN interface and ISO-TP node id
```

### The two `usb:` forms, and how they are told apart

A `usb:` label takes either a kernel port path or a board serial. They are
distinguished **by shape, not by guessing**: a port path is strictly
`<bus>-<port>[.<port>]*` — digits, hyphens and dots and nothing else — so the two
sets are disjoint and the classification is total. `scripts/make-identity.py`
**refuses to write a serial of that shape**, which is what keeps them disjoint in
fact rather than by convention.

Both are legitimate and they answer different questions:

| Form | Means | Right when |
|---|---|---|
| `usb:3-6` | the board in this physical position | boards are replaceable and **position defines the role** — swap a failed controller and the replacement inherits the job untouched |
| `usb:feather-01` | this specific board, wherever it is | inventory is fixed and **identity defines the role** |

The serial form is the only one that makes a compose file portable between
machines: a port path encodes one host's USB tree, so the same file on another
device targets a different port. It also removes a hazard the port-path form
carries — re-cable a hub and a port-path label still resolves, but now to a
different MCU.

**Resolution by serial never talks to the device.** The firmware publishes its
provisioned serial as the **USB serial string descriptor**, so a host reads it
from sysfs before opening anything. The alternative — opening each candidate tty
and asking `describe` — would mean sending SMP frames to boards owned by other
containers, and a frame landing mid-upload is exactly the corruption
`ID_MM_DEVICE_IGNORE` exists to prevent.

A board with no identity record still publishes a hardware-derived serial, so two
identical boards stay distinguishable before either is named. What provisioning
changes is that the value becomes something a human chose and a compose file can
name.

**CAN has no equivalent form and does not need one.** A node id is already an
identity that travels with the board rather than with the cabling, so
`can:can0/0x45` has none of the port-path fragility. A serial-based CAN label
would require discovering which id answers to which serial, and sweeping 2046
identifiers is not viable; a broadcast enumeration protocol would be new contract
surface for no current benefit.

An unprefixed label is an **error**, not a guess. `dev.runtt.skip-if-same-hash`
(default on) suppresses redeploying an image the device already runs, confirmed.

### Board identity, and why the node id is not a build setting

**A board's CAN node id is read from flash at boot, not compiled in.** This is a
contract-level fact rather than an implementation detail, because the alternative
breaks the delivery model: firmware ships as an OCI image, so a per-board Kconfig
symbol makes the *service image* per-board, and a fleet of N boards becomes N
images in the registry with deltas computed against the wrong baselines.

The record lives at **offset 0 of `storage_partition`**, which every supported
board declares upstream and which sits **outside both MCUboot slots** — so a
firmware update cannot cost a board its address. It is not code and is
deliberately not covered by MCUboot's signature; nothing in it is trusted for
anything but addressing.

32 bytes, little-endian, no implicit padding:

| Offset | Size | Field | |
|---|---|---|---|
| 0 | 4 | `magic` | `0x616e6c62` — `"blna"` |
| 4 | 1 | `version` | `1` |
| 5 | 3 | — | reserved, zero |
| 8 | 2 | `can_node_id` | `0xffff` = unassigned |
| 10 | 2 | — | reserved, zero |
| 12 | 16 | `serial` | NUL-padded ASCII; all-zero = none |
| 28 | 4 | `crc` | CRC32-IEEE over bytes 0..27 |

Extend only by claiming reserved space and bumping `version`; never by
reordering. Write one with `scripts/make-identity.py`.

**Absent and damaged are different, and are handled differently:**

* **No record** (erased flash, magic mismatch) — the board uses its built-in
  defaults. This is the factory state, and falling back is required: a fresh
  board must still answer `describe` so the idle app can report itself.
* **A record that is present but fails its CRC or version check** — a CAN
  transport **refuses to bind**. Falling back would put the board on the default
  id, where a correctly provisioned neighbour may already be answering, and two
  ISO-TP responders on one identifier is a failure that damages a working board.
  One board missing is a better symptom than two boards fighting. Recovery is the
  same SWD path provisioning already uses.

`describe` reports `provisioned`, `serial` where assigned, and `can_node_id` — the
id the board is *actually* answering on. Firmware predating these fields omits
them, which a host must read as "unknown" rather than "no".

### CAN addressing: one number, three identifiers

A `can:` label carries a single node id, and the device derives two more from it:

| Identifier | Direction | Carries |
|---|---|---|
| `node_id` | host → device | SMP requests, over ISO-TP |
| `node_id + 1` | device → host | SMP responses, over ISO-TP |
| `node_id + 2` | device → host | the application console, as **raw** CAN frames |

One number rather than three settings, so the label cannot drift out of step with
the firmware. **All three are reserved whether or not the firmware was built with
`CONFIG_RUNTT_CAN_LOG`**, so enabling logging later cannot collide with a
neighbour already on the bus. Give each board on a bus an id at least three apart.

Standard 11-bit identifiers only — the device filters with `.std_id` and the host
sets no `CAN_EFF_FLAG`. The usable range is therefore `0x000`–`0x7fd`; a label
above that is refused when it is parsed rather than being silently masked onto a
different id on the wire.

The console is **raw frames, not ISO-TP, and that is contractual.** ISO-TP waits
for the receiver's flow-control frame, so a device logging over it with no host
attached would block — and a blocking log backend deadlocks boot. Raw frames are
fire-and-forget and are dropped when the controller is busy. Consequences a
consumer must accept:

* **The channel is lossy under backpressure, by design.** A dropped frame appears
  as a mangled line.
* **There is no backlog.** Frames sent before a host attaches are gone. Boot-time
  output is therefore not reliably observable over CAN.
* Ordering is safe without sequence numbers: CAN delivers frames of one
  identifier from one sender in order, and this identifier has one sender.
* Being the highest of the three ids, the console has the **lowest arbitration
  priority**, so an upload in progress wins the bus against chatty logs.

A CAN target may instead name a serial console explicitly with
`dev.runtt.log-target: tty:/dev/ttyACM0`, which overrides the bus channel —
a board managed over CAN whose console comes back over a wire.

### Sharing the bus with application data

**The runtime does not own the CAN interface, and application code may use it at
the same time.** This is the one place CAN is materially simpler than USB: a
`can0` is a *network interface*, not a character device, so any number of sockets
may bind to it concurrently, each with its own kernel-side filters. CAN is a
broadcast bus — every node already sees every frame and filters locally — so
there is nothing to contend for. Contrast `docs/MICROROS.md`, which exists
because sharing one `/dev/ttyACM*` needed a careful argument.

Verified on `vcan0` with four concurrent sockets — the runtime's ISO-TP socket
carrying SMP, its raw socket carrying the console, and an application pair
exchanging their own ids — during a live deploy:

```
SMP   echo -> "runtt";  describe -> contract 2.0.0
APP   sent=179 received=179          <- no loss
LOGS  console lines streaming throughout
```

**The application's own protocol is not our business.** Raw frames, ISO-TP,
CANopen, anything. The contract constrains identifiers and nothing else.

#### What an application must respect

| Identifiers | |
|---|---|
| `node_id`, `node_id + 1`, `node_id + 2` | **reserved** — see the table above |
| everything else in `0x000`–`0x7ff` | the application's |

With the default `0x42` that leaves all of `0x000`–`0x041` and `0x045`–`0x7ff`.

#### The container needs host networking

A CAN interface lives in a network namespace, so a container in its own namespace
cannot see it. This is not a permissions problem and `--device` or `--privileged`
will not fix it:

```console
$ docker run --network host alpine ip link | grep can
18: vcan0: <NOARP,UP,LOWER_UP> mtu 72 qdisc noqueue state UNKNOWN qlen 1000

$ docker run alpine ip link | grep can        # bridge networking: nothing
```

So an application container that talks to the MCU needs `--network host`. Moving
the CAN device into the container's namespace instead would hide it from the
runtime, so host networking is the arrangement that works for both.

#### Arbitration is a design lever

CAN arbitration is **lowest identifier wins**, which makes the choice of
application ids a scheduling decision rather than an arbitrary one:

* Latency-critical application traffic — motor commands, an e-stop — belongs
  **below** `node_id`, so it preempts firmware management traffic.
* If upload speed matters more than control-loop jitter, put it above.

An application at `0x100` with a node at `0x42` yields to management, so a
firmware upload will visibly slow it. If that is the wrong trade for a given
robot, move the application ids down or the node id up. Bus bandwidth is shared
either way: an upload saturates the bus and application traffic gets what
arbitration leaves it.

#### On the device

The Zephyr application adds its own filters with `can_add_rx_filter()` on the
same controller the SMP transport uses; the driver multiplexes. The budget is 16
concurrent filters by default on both supported controllers
(`CAN_MCP2515_MAX_FILTERS`, `CAN_SJA1000_MAX_FILTERS`), each raisable to 32.

One asymmetry between controllers is worth knowing. Zephyr's SJA1000 driver — the
ESP32 TWAI — notes that the chip *"only supports one full-width RX filter,
filtering of received CAN frames are done in software"*, so on ESP32 a busy bus
costs MCU cycles for every frame whether or not the application wants it. The
MCP25625 on the Adafruit Feather has hardware filters. Neither is a correctness
issue; it is a CPU budget one.

How the label reaches the runtime is out of scope here: it arrives as an OCI
annotation on the container spec, and who puts it there is the engine's business.

## 10. udev

Rules key off the interface string descriptors, never VID/PID, and are numbered
after systemd's `60-serial.rules` because they consume its `ID_PATH_TAG`:

```udev
SUBSYSTEM=="tty", ATTRS{interface}=="runtt-mgmt", \
  ENV{ID_MM_DEVICE_IGNORE}="1", SYMLINK+="runtt/$env{ID_PATH_TAG}-mgmt"
```

`ID_MM_DEVICE_IGNORE=1` is load-bearing, not defensive. ModemManager probes new
CDC-ACM devices with AT commands; one probe landing mid-upload is a corrupted
transfer and a genuine heisenbug. It has been observed tagging a contract-shaped
device `ID_MM_CANDIDATE=1` on an ordinary Ubuntu workstation.

The resulting `/dev/runtt/` tree doubles as the runtime's discovery
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
| 2.0.0 | **Breaking: the project was renamed from `balena-mcu` to `runtt`, and the contract carries the name in three places.** USB interface string descriptors are now `runtt-mgmt` / `runtt-log`; OCI annotations moved from `io.balena.mcu.*` to `dev.runtt.*`; the identity record magic changed from `"blna"` to `"rntt"`. Also adds board identity in flash (§9) and the CAN transport with its three-identifier addressing. A v1 board under a v2 host fails at *resolution* — "none advertising the runtt-mgmt interface descriptor" — rather than misbehaving, which is the right symptom; reflash and re-provision it. |
| 1.2.0 | `describe` gains `idle`, so a freshly provisioned board reports as such instead of looking like unrecognised firmware. Additive and backward compatible. |
| 1.1.0 | `describe` gains `img`. Additive and backward compatible: a host seeing contract 1.0.0 firmware finds the field absent and should treat that as unknown rather than false. Added after a real board reported a bare `MGMT_ERR_ENOTSUP` where it could have explained itself. |
| 1.0.0 | Initial contract: dual CDC-ACM with string-descriptor identity, SMP over console framing, os/img groups, `describe` at group 64, optional health. |

---

*Co-authored with Claude*
