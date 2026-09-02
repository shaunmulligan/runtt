#!/usr/bin/env bash
# Hold the vendored mcumgr-toolkit patch to UPSTREAM's standards, not ours.
#
# The vendored crate is patched in via [patch.crates-io] but is NOT a workspace
# member, so `cargo clippy --workspace` and `cargo fmt` never look at it. That
# gap is not theoretical: a collapsible_if and a stray blank line in our added
# test reached a maintainer-facing PR before anyone noticed, because every local
# check we run skips this directory.
#
# It matters more here than for ordinary vendored code, because these changes are
# meant to go upstream. A patch that trips the target project's own lint and
# format gates wastes a maintainer's time and makes the rest of it look careless.
#
# Copies out to a temp dir first: the crate cannot be built in place, since Cargo
# sees the enclosing workspace and refuses.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CRATE="$REPO/third_party/mcumgr-toolkit"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

[[ -d "$CRATE" ]] || {
  echo "no vendored crate at $CRATE -- nothing to check."
  echo "If the upstream patch has landed and the vendoring is gone, delete this script."
  exit 0
}

cp -r "$CRATE" "$WORK/crate"
rm -rf "$WORK/crate/target"
cd "$WORK/crate"

echo "== rustfmt =="
cargo fmt --check

echo "== clippy, all targets, warnings as errors =="
cargo clippy --all-targets -- -D warnings

echo "== tests =="
cargo test

echo
echo "PASS: the vendored crate meets upstream's fmt, lint and test gates."
