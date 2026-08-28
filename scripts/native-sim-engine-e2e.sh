#!/usr/bin/env bash
# The full stack: a container engine, a real container image, and native_sim.
#
# scripts/native-sim-e2e.sh invokes the OCI verbs directly, which proves
# runtime <-> device. This proves the other seam at the same time:
#
#   podman/docker -> containerd shim -> mcu-runtime -> SMP -> Zephyr native_sim
#
# So the firmware really is delivered as a container image, resolved from the
# entrypoint inside a real rootfs, with the placement label arriving as an OCI
# annotation and the device's output landing in the container's log.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"

RUNTIME="${RUNTIME:-$REPO/target/debug/mcu-runtime}"
SIM="${SIM:-$REPO/build/zephyr/zephyr.exe}"
IMAGE="${IMAGE:-mcu-fw:native-sim-e2e}"
WORK="$(mktemp -d)"
trap 'kill %1 2>/dev/null || true; rm -rf "$WORK"' EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }
ok()   { echo "  ok: $*"; }

[[ -x "$RUNTIME" ]] || fail "build the runtime first: cargo build"
[[ -x "$SIM" ]]     || fail "build the firmware first: ./scripts/build-native-sim.sh"

# --- pick an engine ---------------------------------------------------------
# podman is the default: it takes --runtime=<path> directly, with no daemon
# config, no root and no stale-binary problem, which makes it the right gate to
# run on every commit.
#
# It is NOT a complete substitute for Docker, and it is worth being precise
# about what it leaves untested, because this is where opaque failures live:
#
#   * containerd passes --root, --log and --log-format json on every
#     invocation; podman passes only --systemd-cgroup. Rejecting an unknown
#     global flag would pass here and fail on a device.
#   * containerd sends `kill --all <id> 9`; podman sent 18 (SIGCONT) during
#     teardown. Different signal surfaces -- an earlier whitelist accepted
#     containerd's and rejected podman's.
#   * containerd calls `delete` and then `delete --force`; podman calls it once.
#   * Docker goes through containerd-shim-runc-v2 and runs the runtime as root;
#     podman rootless invokes it directly in a user namespace.
#
# Those differences are covered by the Docker path (ENGINE=docker) and are
# recorded from real traces in docs/OCI_COMPLIANCE.md. Run the Docker path
# before believing anything about on-device behaviour.
ENGINE="${ENGINE:-}"
if [[ -z "$ENGINE" ]]; then
  if command -v podman >/dev/null; then
    ENGINE=podman
  elif command -v docker >/dev/null && docker info 2>/dev/null | grep -q 'mcu-runtime'; then
    ENGINE=docker
  else
    fail "no usable engine: install podman, or register with Docker via scripts/register-docker.sh"
  fi
fi
echo "engine: $ENGINE"

# Docker invokes the binary registered in daemon.json, NOT the one you just
# built. Rebuilding does not update it, and a stale binary fails in confusing
# ways -- an older one lacking the MCUboot TLV parser reports
# IMG_MGMT_ERR_HASH_NOT_FOUND, which looks like a device problem.
if [[ "$ENGINE" == docker ]]; then
  REGISTERED="${REGISTERED:-/usr/local/bin/mcu-runtime}"
  [[ -e "$REGISTERED" ]] || fail "$REGISTERED does not exist. Run: sudo scripts/register-docker.sh"
  if ! cmp -s "$RUNTIME" "$REGISTERED"; then
    fail "the registered runtime is stale.
  built:      $RUNTIME ($(stat -c %y "$RUNTIME" | cut -c1-19))
  registered: $REGISTERED ($(stat -c %y "$REGISTERED" | cut -c1-19))
Docker runs the registered copy, so re-install it after every rebuild:
  sudo install -m 0755 $RUNTIME $REGISTERED
(or re-run: sudo scripts/register-docker.sh)"
  fi
  ok "registered binary matches the build"
fi

# --- a signed image, in a real container image -------------------------------
head -c 8192 /dev/urandom > "$WORK/payload.bin"
python3 bootloader/mcuboot/scripts/imgtool.py sign \
  --key bootloader/mcuboot/root-ec-p256.pem \
  --header-size 0x200 --pad-header --align 4 --version 2.0.0 --slot-size 0x69000 \
  "$WORK/payload.bin" "$WORK/app.signed.bin" >/dev/null

EXPECTED_DIGEST=$(python3 bootloader/mcuboot/scripts/imgtool.py verify \
  --key bootloader/mcuboot/root-ec-p256.pem "$WORK/app.signed.bin" \
  | awk '/Image digest/ {print $3}')
[[ -n "$EXPECTED_DIGEST" ]] || fail "could not read the image digest from imgtool"
ok "signed an image (digest ${EXPECTED_DIGEST:0:16}...)"

# The canonical firmware-service image: nothing but the signed blob and an
# entrypoint naming it. This is what the runtime resolves inside the rootfs.
cp "$WORK/app.signed.bin" "$WORK/app.signed.bin.ctx"
cat > "$WORK/Dockerfile" <<'DOCKER'
FROM scratch
ADD app.signed.bin /
ENTRYPOINT ["app.signed.bin"]
DOCKER
"$ENGINE" build -q -t "$IMAGE" "$WORK" >/dev/null
ok "built the container image ($IMAGE)"

# --- start native_sim -------------------------------------------------------
# No --flash_erase: os reset re-execs preserving argv, so it would wipe flash on
# every reboot. A fresh mktemp path is already clean.
"$SIM" \
  --uart_attach_uart_cmd="ln -sf %s $WORK/mgmt" \
  --uart_1_attach_uart_cmd="ln -sf %s $WORK/log" \
  --flash="$WORK/flash.bin" \
  > "$WORK/sim.out" 2>&1 &

for _ in $(seq 1 50); do [[ -e "$WORK/mgmt" ]] && break; sleep 0.2; done
[[ -e "$WORK/mgmt" ]] || fail "simulator never announced its management pty"
MGMT="$(readlink -f "$WORK/mgmt")"
ok "native_sim up on $MGMT"

# --- run it through the engine ----------------------------------------------
# The container is EXPECTED to exit non-zero: native_sim cannot swap, so the
# runtime correctly refuses to confirm. We assert on what it did before that.
#
# Docker's daemon runs as root and resolves the pty in the host namespace, so
# pass the resolved /dev/pts path rather than the symlink.
RUN_ARGS=(run --rm --annotation "io.balena.mcu.target=tty:$MGMT")
if [[ "$ENGINE" == docker ]]; then
  # A firmware service has no business holding a network namespace.
  RUN_ARGS=(run --rm --runtime mcu-runtime --network none
            --annotation "io.balena.mcu.target=tty:$MGMT")
  set +e
  timeout 120 docker "${RUN_ARGS[@]}" "$IMAGE" > "$WORK/container.log" 2>&1
  RC=$?
  set -e
else
  set +e
  timeout 120 podman --runtime="$RUNTIME" "${RUN_ARGS[@]}" "$IMAGE" \
    > "$WORK/container.log" 2>&1
  RC=$?
  set -e
fi

CLOG=$(sed 's/\x1b\[[0-9;]*m//g' "$WORK/container.log")
SIMOUT=$(sed 's/\x1b\[[0-9;]*m//g' "$WORK/sim.out")

echo "  (container exit code $RC; non-zero is expected here)"

# --- assertions -------------------------------------------------------------
# The annotation survived the whole engine -> shim -> runtime path.
grep -q "tty:$MGMT" <<<"$CLOG" \
  || fail "the placement annotation did not reach the runtime. Output:
$CLOG"
ok "placement annotation arrived via the engine"

# The firmware was found by resolving the entrypoint inside the image's rootfs,
# which is a different path from the loose bundle the direct test uses.
grep -qE "bytes=[0-9]+ version=2\.0\.0" <<<"$CLOG" \
  || fail "runtime did not parse the image from the container rootfs:
$CLOG"
ok "firmware resolved from the container image's entrypoint"

grep -q "$EXPECTED_DIGEST" <<<"$CLOG" \
  || fail "runtime did not use the MCUboot image digest ($EXPECTED_DIGEST):
$CLOG"
ok "used the MCUboot image digest"

grep -q "uploading" <<<"$CLOG"                || fail "no upload progress in the container log"
grep -q "marked test, resetting" <<<"$CLOG"   || fail "image was never staged and marked test"
ok "uploaded and marked test, visible in the container log"

# The device's own view, from the other side.
grep -q "Swap type: test" <<<"$SIMOUT" \
  || fail "the device never reported 'Swap type: test'"
grep -q "native_sim_reboot: Restarting process" <<<"$SIMOUT" \
  || fail "os reset did not re-exec the simulator"
ok "device confirms the trailer write and the reset"

# And the bytes really are in the staging slot.
./scripts/flash-inspect.py "$WORK/flash.bin" --expect-image "$WORK/app.signed.bin" \
  | tail -1 | grep -q "byte for byte" \
  || fail "slot 1 does not match the image we shipped in the container"
ok "slot 1 matches the image shipped inside the container image"

[[ $RC -ne 0 ]] || fail "container exited 0, but native_sim cannot complete a swap \
-- the runtime should have refused to confirm"
ok "container exited non-zero, which is what drives a restart policy"

echo
echo "PASS: engine -> $ENGINE -> mcu-runtime -> SMP -> native_sim, end to end."
