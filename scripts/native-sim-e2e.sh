#!/usr/bin/env bash
# Phase-1 gate: drive the real runtime against Zephyr native_sim.
#
# Asserts the parts of the lifecycle native_sim CAN prove:
#   * two contract channels appear as host ptys, identified by node name
#   * the image uploads over SMP and physically lands in slot 1
#   * marking it TEST writes the MCUboot trailer (swap type becomes "test")
#   * `os reset` re-execs the simulator, preserving argv, and it comes back
#
# It does NOT assert swap or confirm. Those are unreachable on native_sim by
# construction: MCUboot's POSIX path computes flash_base + offset and calls it
# as a function pointer, and "flash" here is an mmap'd data file with no
# PROT_EXEC holding an image for another architecture. Swap/revert/confirm
# correctness is covered by MCUboot's own Rust simulator instead.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"

RUNTIME="${RUNTIME:-$REPO/target/debug/mcu-runtime}"
SIM="${SIM:-$REPO/build/zephyr/zephyr.exe}"
WORK="$(mktemp -d)"
trap 'kill %1 2>/dev/null || true; rm -rf "$WORK"' EXIT

[[ -x "$RUNTIME" ]] || { echo "build the runtime first: cargo build" >&2; exit 1; }
[[ -x "$SIM" ]]     || { echo "build the firmware first: see scripts/build-native-sim.sh" >&2; exit 1; }

fail() { echo "FAIL: $*" >&2; exit 1; }
ok()   { echo "  ok: $*"; }

# --- a properly signed image, because img_mgmt validates the header ----------
# An unsigned binary is rejected with IMG_MGMT_ERR_INVALID_IMAGE_HEADER_MAGIC,
# which is the device being correct, not a bug.
head -c 8192 /dev/urandom > "$WORK/payload.bin"
python3 bootloader/mcuboot/scripts/imgtool.py sign \
  --key bootloader/mcuboot/root-ec-p256.pem \
  --header-size 0x200 --pad-header --align 4 --version 2.0.0 --slot-size 0x69000 \
  "$WORK/payload.bin" "$WORK/app.signed.bin" >/dev/null
ok "signed a test image"

EXPECTED_DIGEST=$(python3 bootloader/mcuboot/scripts/imgtool.py verify \
  --key bootloader/mcuboot/root-ec-p256.pem "$WORK/app.signed.bin" \
  | awk '/Image digest/ {print $3}')
[[ -n "$EXPECTED_DIGEST" ]] || fail "could not read the image digest from imgtool"

# --- an OCI bundle pointing at the simulator's management pty ----------------
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
  "annotations": { "io.balena.mcu.target": "tty:$WORK/mgmt" }
}
JSON

# --- start the simulator ----------------------------------------------------
# Note what is NOT passed here: --flash_erase.
#
# `os reset` on native_sim re-execs the process PRESERVING ARGV, so
# --flash_erase would fire again on every reboot and wipe the flash each time --
# destroying exactly the state the reset is supposed to preserve. We get a clean
# device by starting from a fresh file instead (mktemp gives us one), which
# erases once rather than on every boot.
#
# The symlinks matter more than they look, for the same reason: pty numbers
# change on every reset, so any parsed /dev/pts/N goes stale after a reboot.
"$SIM" \
  --uart_attach_uart_cmd="ln -sf %s $WORK/mgmt" \
  --uart_1_attach_uart_cmd="ln -sf %s $WORK/log" \
  --flash="$WORK/flash.bin" \
  > "$WORK/sim.out" 2>&1 &

for _ in $(seq 1 50); do [[ -e "$WORK/mgmt" && -e "$WORK/log" ]] && break; sleep 0.2; done
[[ -e "$WORK/mgmt" ]] || fail "simulator never announced its management pty"
[[ -e "$WORK/log"  ]] || fail "simulator never announced its log pty"
ok "two channels: mgmt=$(readlink -f "$WORK/mgmt") log=$(readlink -f "$WORK/log")"

# The announcement is keyed by devicetree NODE name (uart, uart_1), not label.
grep -q "^uart connected to pseudotty:"   "$WORK/sim.out" || fail "no 'uart' announcement"
grep -q "^uart_1 connected to pseudotty:" "$WORK/sim.out" || fail "no 'uart_1' announcement"
ok "channels announced by devicetree node name"

# --- application output must arrive on the LOG channel, not just stdout -----
# Asserting on the simulator's own stdout would pass even if the channel were
# silent, because native_sim mirrors the console there and that cannot be
# disabled from Kconfig (the board does `select POSIX_ARCH_CONSOLE`).
timeout 4 cat "$WORK/log" > "$WORK/logch.txt" 2>/dev/null || true
grep -q "template app" "$WORK/logch.txt" \
  || fail "no application output on the log channel (got: $(head -c 200 "$WORK/logch.txt"))"
ok "application logs arrive on the log channel"

# --- deploy -----------------------------------------------------------------
STATE="$WORK/state"
set +e
"$RUNTIME" --root "$STATE" create --bundle "$WORK/bundle" --pid-file "$WORK/pid" nsim \
  > "$WORK/rt.log" 2>&1
CREATE_RC=$?
set -e
[[ $CREATE_RC -eq 0 ]] || fail "create failed: $(cat "$WORK/rt.log")"
"$RUNTIME" --root "$STATE" start nsim >> "$WORK/rt.log" 2>&1 || true

# Wait for the runtime to reach a terminal state. Waiting only for "resetting"
# races: the reset has been requested but not yet performed, and tearing the
# container down here would kill the proxy mid-reboot.
for _ in $(seq 1 80); do
  grep -qE "mcu-runtime: |image confirmed" "$WORK/rt.log" && break
  sleep 0.5
done
# Give the simulator a moment to finish re-execing and print its second banner.
sleep 2

"$RUNTIME" --root "$STATE" delete --force nsim >/dev/null 2>&1 || true

# --- assertions -------------------------------------------------------------
RT=$(sed 's/\x1b\[[0-9;]*m//g' "$WORK/rt.log")
SIMOUT=$(sed 's/\x1b\[[0-9;]*m//g' "$WORK/sim.out")

grep -q "$EXPECTED_DIGEST" <<<"$RT" \
  || fail "runtime did not use the MCUboot image digest ($EXPECTED_DIGEST). \
Image identity is the TLV digest, not the file hash."
ok "runtime used the MCUboot image digest, not the file hash"

grep -q "uploading" <<<"$RT" || fail "no upload progress reported"
grep -q "marked test, resetting" <<<"$RT" || fail "image was never staged and marked test"
ok "uploaded and marked test"

# The bytes must be physically present in slot 1, checked from the host.
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
ok "os reset re-execed the simulator, preserving argv"

# And it came back: a second boot banner after the restart.
[[ $(grep -c "Booting Zephyr OS" <<<"$SIMOUT") -ge 2 ]] \
  || fail "the simulator did not boot again after the reset"
ok "simulator came back after reset"

# The runtime must then say precisely why it cannot finish, rather than hanging
# or reporting something vague.
grep -q "no bootloader" <<<"$RT" \
  || fail "expected the runtime to name the missing-bootloader case; got: $RT"
ok "runtime correctly names swap/confirm as unreachable without a bootloader"

echo
echo "PASS: native_sim proved everything it can prove."
echo "      Swap, revert and confirm are covered by MCUboot's own simulator."
