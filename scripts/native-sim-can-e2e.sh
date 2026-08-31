#!/usr/bin/env bash
# Phase-1 gate, second transport: drive the real runtime over CAN.
#
# The serial gate's twin. Same firmware, same runtime, same OCI bundle shape --
# only the placement label changes, from `tty:<pty>` to `can:<iface>/<node-id>`.
# That is the point: if the transport seam is real, this script differs from
# native-sim-can-e2e's sibling in the annotation and almost nothing else.
#
# Asserts what CAN can prove without hardware:
#   * a can: placement label resolves and opens an ISO-TP socket
#   * the image uploads over SMP-on-ISO-TP and physically lands in slot 1
#   * marking it TEST writes the MCUboot trailer (swap type becomes "test")
#   * `os reset` re-execs the simulator and it comes back on the bus
#   * the device's console arrives as raw frames on node_id + 2
#
# It does NOT assert swap or confirm, for the same reason the serial gate does
# not: MCUboot cannot chain-load on native_sim.
#
# One further gap, stated because it is easy to misread this script as covering
# it: the log assertion reads the bus directly, via the can-logs example, NOT
# through the runtime's own pump. The runtime only pumps logs from
# stay_resident(), which is reached after a successful confirm -- unreachable
# here without a bootloader. So this proves the DEVICE emits and the HOST
# transport receives; the runtime's wiring between them is covered by unit tests
# and by hardware. The serial gate has exactly the same gap for the same reason.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"

IFACE="${IFACE:-vcan0}"
# DELIBERATELY NOT the firmware's compile-time default of 0x42. The whole point
# of the identity record is that one image serves a fleet, so the gate has to run
# through the provisioned path -- if this matched the built-in default, the test
# would pass just as well with the record ignored entirely.
NODE_ID="${NODE_ID:-0x45}"
RUNTIME="${RUNTIME:-$REPO/target/debug/runtt}"
BUILD_DIR="${BUILD_DIR:-$REPO/build-can}"
SIM="${SIM:-$BUILD_DIR/zephyr/zephyr.exe}"
WORK="$(mktemp -d)"
trap 'kill %1 2>/dev/null || true; rm -rf "$WORK"' EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }
ok()   { echo "  ok: $*"; }
skip() { echo "SKIP: $*"; exit 0; }

# --- preconditions ----------------------------------------------------------
# A missing bus is a skip, not a failure: it means the host has not had
# `modprobe vcan can-isotp` run, which is a setup step and not a regression.
[[ -x "$RUNTIME" ]] || fail "build the runtime first: cargo build"
ip link show "$IFACE" >/dev/null 2>&1 \
  || skip "no interface $IFACE. Run: sudo modprobe vcan can-isotp && sudo ip link add dev $IFACE type vcan && sudo ip link set $IFACE up"
grep -qw can_isotp /proc/modules \
  || skip "the can-isotp kernel module is not loaded. Run: sudo modprobe can-isotp"
# native_sim's CAN driver hardcodes the interface name "zcan0", so the bus needs
# that altname. Without it the simulator opens nothing and every SMP call times
# out with no clue as to why.
ip -d link show "$IFACE" | grep -q "altname zcan0" \
  || skip "$IFACE has no zcan0 altname. Run: sudo ip link property add dev $IFACE altname zcan0"
ok "bus $IFACE is up, can-isotp loaded, zcan0 altname present"

if [[ ! -x "$SIM" ]]; then
  echo "  building CAN-enabled native_sim firmware into $BUILD_DIR ..."
  BUILD_DIR="$BUILD_DIR" ./scripts/build-native-sim.sh \
    -DEXTRA_CONF_FILE="$REPO/firmware/bringup/native-sim-can.conf" \
    -DEXTRA_DTC_OVERLAY_FILE="$REPO/firmware/bringup/native-sim-can.overlay" \
    >"$WORK/build.log" 2>&1 || { tail -30 "$WORK/build.log"; fail "firmware build failed"; }
  ok "built CAN-enabled firmware"
fi

# --- a properly signed image ------------------------------------------------
head -c 8192 /dev/urandom > "$WORK/payload.bin"
python3 bootloader/mcuboot/scripts/imgtool.py sign \
  --key bootloader/mcuboot/root-ec-p256.pem \
  --header-size 0x200 --pad-header --align 4 --version 2.0.0 --slot-size 0x69000 \
  "$WORK/payload.bin" "$WORK/app.signed.bin" >/dev/null
EXPECTED_DIGEST=$(python3 bootloader/mcuboot/scripts/imgtool.py verify \
  --key bootloader/mcuboot/root-ec-p256.pem "$WORK/app.signed.bin" \
  | awk '/Image digest/ {print $3}')
[[ -n "$EXPECTED_DIGEST" ]] || fail "could not read the image digest from imgtool"
ok "signed a test image"

# --- an OCI bundle whose placement is a CAN address -------------------------
mkdir -p "$WORK/bundle/rootfs"
cp "$WORK/app.signed.bin" "$WORK/bundle/rootfs/app.signed.bin"
cat > "$WORK/bundle/config.json" <<JSON
{
  "ociVersion": "1.2.0",
  "process": {
    "user": { "uid": 0, "gid": 0 },
    "args": ["app.signed.bin"],
    "cwd": "/",
    "terminal": false
  },
  "root": { "path": "rootfs", "readonly": true },
  "annotations": { "dev.runtt.target": "can:$IFACE/$NODE_ID" }
}
JSON

# --- provision the board before it boots ------------------------------------
# Writes an identity record into storage_partition, which on native_sim is at
# 0xfc000. This is the same 32 bytes make-identity.py would put on real hardware
# over SWD; only the delivery differs.
./scripts/make-identity.py --can-node-id "$NODE_ID" --serial sim-gate-01 \
  -o "$WORK/identity.bin" 2>/dev/null
python3 - "$WORK/flash.bin" "$WORK/identity.bin" <<'PY'
import sys
flash = bytearray(b'\xff' * (2 * 1024 * 1024))
flash[0xfc000:0xfc000 + 32] = open(sys.argv[2], 'rb').read()
open(sys.argv[1], 'wb').write(flash)
PY
ok "provisioned the simulated board with node id $NODE_ID"

# --- start the simulator ----------------------------------------------------
# No --flash_erase, for the same reason as the serial gate: `os reset` re-execs
# preserving argv, so it would wipe the flash on every reboot.
"$SIM" --flash="$WORK/flash.bin" > "$WORK/sim.out" 2>&1 &

for _ in $(seq 1 50); do grep -q "SMP over ISO-TP" "$WORK/sim.out" 2>/dev/null && break; sleep 0.2; done
grep -q "SMP over ISO-TP" "$WORK/sim.out" \
  || fail "the simulator never announced its CAN transport: $(head -c 300 "$WORK/sim.out")"
ok "simulator listening on the bus: $(grep -o 'receiving on .*' "$WORK/sim.out" | head -1)"

# The firmware must be on the PROVISIONED id, not the one it was compiled with.
# This is the assertion that makes the record load-bearing rather than decorative.
grep -q "receiving on $NODE_ID" "$WORK/sim.out" \
  || fail "the board is not on its provisioned id $NODE_ID -- the identity record was \
not read. Compiled default is 0x42; got: $(grep -o 'receiving on .*' "$WORK/sim.out" | head -1)"
grep -q "provisioned: can node id $NODE_ID" "$WORK/sim.out" \
  || fail "the firmware did not report reading an identity record"
ok "board took its node id from flash, not from the build"

# --- the console must arrive on the bus, not only on the simulator's stdout --
# Asserting on the simulator's own stdout would pass even with the CAN log
# backend entirely broken, because native_sim mirrors the console there and that
# cannot be disabled from Kconfig. This is the assertion that actually failed
# while the backend was emitting one frame per character.
LOG_ID=$(printf '0x%x' $(( NODE_ID + 2 )))
timeout 6 cargo run -q -p runtt-transport --example can-logs -- "$IFACE" "$NODE_ID" \
  > "$WORK/canlog.txt" 2>/dev/null || true
# Match a RECURRING line, not the boot banner. A raw CAN channel has no backlog:
# frames sent before this listener attached are gone, by design, because the
# device must never block waiting for someone to listen. Asserting on a one-shot
# startup message would be asserting on a race.
grep -q "alive, tick" "$WORK/canlog.txt" \
  || fail "no application output on CAN id $LOG_ID (got: $(head -c 200 "$WORK/canlog.txt"))"
ok "application logs arrive as raw CAN frames on $LOG_ID"

# --- deploy -----------------------------------------------------------------
STATE="$WORK/state"
set +e
"$RUNTIME" --root "$STATE" create --bundle "$WORK/bundle" --pid-file "$WORK/pid" ncan \
  > "$WORK/rt.log" 2>&1
CREATE_RC=$?
set -e
[[ $CREATE_RC -eq 0 ]] || fail "create failed: $(cat "$WORK/rt.log")"
"$RUNTIME" --root "$STATE" start ncan >> "$WORK/rt.log" 2>&1 || true

for _ in $(seq 1 80); do
  grep -qE "runtt: |image confirmed" "$WORK/rt.log" && break
  sleep 0.5
done
sleep 2
"$RUNTIME" --root "$STATE" delete --force ncan >/dev/null 2>&1 || true

# --- assertions -------------------------------------------------------------
RT=$(sed 's/\x1b\[[0-9;]*m//g' "$WORK/rt.log")
SIMOUT=$(sed 's/\x1b\[[0-9;]*m//g' "$WORK/sim.out")

grep -q "can:$IFACE" <<<"$RT" || fail "the runtime never reported a CAN target; got: $RT"
ok "runtime resolved the can: placement label"

grep -q "$EXPECTED_DIGEST" <<<"$RT" \
  || fail "runtime did not use the MCUboot image digest ($EXPECTED_DIGEST); got: $RT"
ok "runtime used the MCUboot image digest, not the file hash"

grep -q "uploading" <<<"$RT" || fail "no upload progress reported"
grep -q "marked test, resetting" <<<"$RT" || fail "image was never staged and marked test"
ok "uploaded over ISO-TP and marked test"

python3 - "$WORK/flash.bin" <<'PY'
import sys
d = open(sys.argv[1], 'rb').read()
slot1 = d[0x75000:0x75000 + 0x69000]
nonff = sum(1 for b in slot1 if b != 0xff)
if nonff == 0:
    sys.exit("FAIL: slot 1 is empty; the upload did not reach the simulated flash")
if slot1[:4].hex() != '3db8f396':
    sys.exit(f"FAIL: slot 1 does not start with the MCUboot magic (got {slot1[:4].hex()})")
print(f"  ok: slot 1 holds {nonff} bytes starting with the MCUboot magic")
PY

grep -q "Swap type: test" <<<"$SIMOUT" \
  || fail "the device never reported 'Swap type: test'; the trailer was not written"
ok "MCUboot trailer written: device reports swap type 'test'"

grep -q "native_sim_reboot: Restarting process" <<<"$SIMOUT" \
  || fail "os reset did not re-exec the simulator"
[[ $(grep -c "Booting Zephyr OS" <<<"$SIMOUT") -ge 2 ]] \
  || fail "the simulator did not boot again after the reset"
ok "os reset re-execed the simulator, and it came back on the bus"

echo
echo "PASS: the full deploy path works over CAN, with no hardware."
echo "      Same runtime, same firmware, same bundle -- only the placement label differs."
