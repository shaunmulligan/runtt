#!/usr/bin/env bash
# Register runtt with the local Docker daemon as a runc-style runtime.
#
# This is the phase-0 milestone check: Docker's containerd-shim-runc-v2 invokes
# our binary over the same path any production engine will use, so if it works here
# it works there.
#
# Run with sudo. Idempotent; keeps a backup of any existing daemon.json.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_SRC="$REPO/target/debug/runtt"
BIN_DST="/usr/local/bin/runtt"
CONF="/etc/docker/daemon.json"

[[ $EUID -eq 0 ]] || { echo "must run as root (sudo $0)" >&2; exit 1; }
[[ -x "$BIN_SRC" ]] || { echo "build it first: cargo build" >&2; exit 1; }

install -m 0755 "$BIN_SRC" "$BIN_DST"
echo "installed $BIN_DST"

# Binaries this script installed under previous names, for the same reason.
for legacy in /usr/local/bin/mcu-runtime; do
  [[ -e "$legacy" ]] && { rm -f "$legacy"; echo "removed stale $legacy"; }
done

mkdir -p /etc/docker
if [[ -f "$CONF" ]]; then
  cp -a "$CONF" "$CONF.bak.$(date +%s)"
else
  echo '{}' > "$CONF"
fi

# Merge rather than overwrite: the daemon may already have unrelated settings.
python3 - "$CONF" "$BIN_DST" <<'PY'
import json, sys
conf_path, bin_path = sys.argv[1], sys.argv[2]
try:
    conf = json.load(open(conf_path))
except Exception:
    conf = {}
runtimes = conf.setdefault("runtimes", {})

# Drop registrations this script made under the project's previous names. It only
# ever removes entries it could have created itself, never anyone else's runtime,
# and leaving them behind would point Docker at a binary that no longer exists.
for legacy in ("mcu-runtime",):
    if runtimes.pop(legacy, None) is not None:
        print(f"removed the stale {legacy} registration")

runtimes["runtt"] = {
    "path": bin_path,
    # Diagnostic for phase 0: the engine does not forward a user shell's
    # environment, so the trace path has to come in as a runtime arg.
    "runtimeArgs": ["--mcu-trace", "/tmp/mcu-trace-docker.jsonl"],
}
json.dump(conf, open(conf_path, "w"), indent=2)
print(f"registered runtt in {conf_path}")
PY

systemctl restart docker
sleep 2
echo
docker info 2>/dev/null | grep -iA3 "^ Runtimes" || true
echo
echo "Done. Now (as your normal user) run:"
echo "  scripts/smoke-docker.sh"
