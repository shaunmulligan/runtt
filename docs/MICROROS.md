# micro-ROS coexistence — research, for a future cycle

**Status: research only. Nothing here is built, and the plan and architecture
are unchanged.** This records what a robotics use case would need — ROS 2 in a
Linux container, ROS nodes on several Zephyr MCUs, all on ROS topics — while
this runtime manages firmware over SMP and pipes MCU logs to container stdio.

Findings are marked **[verified]** (read in source, measured, or built),
**[likely]** (strong evidence, one inferential step), or **[unverified]** (needs
a bench test before anyone plans around it). Treat the unverified ones as open.

## The short answer

**Logging does not have to be dropped, and there is no lock conflict.** The
obstacle is not resource contention — it is that micro-ROS's Zephyr module is
bound to the USB stack this project deliberately does not use, and does not yet
support this project's Zephyr version. Both are bounded problems with a known
path through them; neither is architectural.

## The contention that isn't there

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

### The hazard this creates, which is the real one

Because nothing takes a lock, **nothing stops a misconfigured agent opening the
management channel and corrupting an SMP upload mid-flash.** Every micro-ROS
tutorial says `--dev /dev/ttyACM0`, and on a composite device that is as likely
to be `runtt-mgmt` as the ROS channel.

The only available defence is the one the contract already uses: a distinct
interface string descriptor and its own udev rule. Note the ordering constraint
— **the udev rule must ship before firmware advertising the new interface**, or
the channel gets no `ID_MM_DEVICE_IGNORE=1` and ModemManager will AT-probe it.
`docs/WIRE_CONTRACT.md` already calls that line load-bearing. **[verified]**

## Two blockers, both certain, both bounded

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

## The path that works

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

### If a custom transport is wanted instead

`rmw_uros_set_custom_transport(bool framing, void *args, open_cb, close_cb,
write_cb, read_cb)`. All three built-in transports are *already* custom
transports — `libmicroros.mk` sets `UCLIENT_PROFILE_CUSTOM_TRANSPORT=ON`
unconditionally, and the Kconfig choice only picks which `.c` file compiles.

Best reference implementation is the official RP2040 one,
[`pico_uart_transport.c`](https://github.com/micro-ROS/micro_ros_raspberrypi_pico_sdk/blob/kilted/pico_uart_transport.c)
— **72 lines total**, of which the four callbacks are ~50. Use `framing=true`
for any byte stream. **[verified]**

### Hardware UART as a fallback

`uart1` on the Pico is `status = "disabled"` in the stock devicetree and
therefore free (`uart0` is the physical pins, already enabled). On a robot where
the MCU is wired to an SBC rather than plugged into it, a hardware UART sidesteps
the USB questions below entirely. **[verified]**

## Third channel feasibility

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

## Memory, which is the thin ice

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

## Do not share the SMP link

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

## Operational findings for a robotics deployment

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

## The biggest risk

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

## If this is picked up, test in this order

1. Three CDC-ACM instances enumerating on RP2040 and nRF52840 under device_next,
   with `ID_PATH` stability for the existing two channels re-verified.
2. `libmicroros` linked for `cortex-m0plus` with `--gc-sections` and a real
   message set, for an honest RAM number.
3. Whether the runtime's resolve survives a deploy reset with a third holder
   attached — the renumbering risk above.
4. Only then: PR #163's alias hunk, pointed at the third CDC-ACM node.

---

*Co-authored with Claude*
