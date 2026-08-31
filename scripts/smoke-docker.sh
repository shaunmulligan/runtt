#!/usr/bin/env bash
# Phase-0 smoke test: drive runtt through Docker and capture exactly what
# the shim passes us. Run as a normal user, after scripts/register-docker.sh.
set -euo pipefail

TRACE="${MCU_RUNTIME_TRACE:-/tmp/mcu-trace-docker.jsonl}"
IMAGE="mcu-fw:test"
BUILD_DIR="$(mktemp -d)"
trap 'rm -rf "$BUILD_DIR"' EXIT

# The canonical firmware-image pattern: FROM scratch + a signed blob + entrypoint.
head -c 4096 /dev/urandom > "$BUILD_DIR/app.signed.bin"
cat > "$BUILD_DIR/Dockerfile" <<'DOCKER'
FROM scratch
ADD app.signed.bin /
ENTRYPOINT ["app.signed.bin"]
DOCKER
docker build -q -t "$IMAGE" "$BUILD_DIR" >/dev/null
echo "built $IMAGE"

# The trace file must be writable by the *daemon*, which runs as root.
sudo rm -f "$TRACE" 2>/dev/null || rm -f "$TRACE" 2>/dev/null || true

echo
echo "── docker run --runtime runtt ──"
set +e
docker run --rm \
  --runtime runtt \
  --network none \
  --annotation dev.runtt.target=usb:3-6 \
  "$IMAGE"
echo "container exit code: $?"
set -e
