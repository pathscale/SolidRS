#!/usr/bin/env bash
# Build the guest to a real .wasm and run it under wasmi.
#
# Also asserts the hard rule: `wasmi` must never appear in solid_rs's own
# dependency graph. That is guaranteed structurally by the `exclude` in
# SolidRS/Cargo.toml, and re-checked here so a future edit that "helpfully"
# adds it as a dev-dependency fails loudly instead of silently.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"

echo "=== rule check: wasmi must not be in solid_rs's graph ==="
for scope in "" "--all-features"; do
  # shellcheck disable=SC2086
  if cargo tree --manifest-path "$root/Cargo.toml" -e all $scope 2>/dev/null | grep -qi wasmi; then
    echo "FAIL: wasmi reachable from solid_rs (${scope:---default-features})" >&2
    exit 1
  fi
done
echo "ok: not reachable with default features, nor with --all-features"
echo

target="wasm32-unknown-unknown"
for cfg in "" "--features dev"; do
  label="${cfg:-shipping (no default features)}"
  echo "=== guest build: $label ==="
  # shellcheck disable=SC2086
  cargo build --release --target "$target" -p solidrs-wasm-smoke-guest $cfg
  wasm="$here/target/$target/release/solidrs_wasm_smoke_guest.wasm"
  echo
  echo "=== wasmi run: $label ==="
  cargo run --release -q -p solidrs-wasm-smoke-host -- "$wasm"
  echo
done
