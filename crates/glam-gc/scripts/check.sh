#!/usr/bin/env bash
#
# glam-gc's verification entry point. Run it only when changing glam-gc,
# beside Glam's `scripts/check.sh` at the matching level, since Glam builds
# on the collector.
#
#   crates/glam-gc/scripts/check.sh [all|full|dynamic]
#
# Levels are cumulative:
#
#   all      (default) Formatting, Clippy and the all-features test suite,
#            the persistent-edge codegen latch, and the unsafe-site audit.
#
#   full     Adds the million-edge scale proofs.
#
#   dynamic  Adds Miri and the address and thread sanitizers, about an hour.
#            A judgement call, for changes to unsafe code, concurrency or the
#            collector's contracts.
#
# The project pins only a stable toolchain (rust-toolchain.toml), so `dynamic`
# skips Miri and the sanitizers with a notice unless a nightly is available.
set -euo pipefail

root="$(cd "$(dirname "$0")/../../.." && pwd)"
cd "$root"

level="${1:-all}"

case "$level" in
  all | full | dynamic) ;;
  -h | --help | help)
    sed -n '3,21p' "$0" | sed 's/^#\{0,1\} \{0,1\}//'
    exit 0
    ;;
  *)
    echo "usage: $(basename "$0") [all|full|dynamic]" >&2
    exit 2
    ;;
esac

step() { printf '\n==> %s\n' "$*"; }

run_all() {
  step "fmt (glam-gc)"
  cargo fmt --check --package glam-gc
  step "clippy (glam-gc, all targets, all features)"
  cargo clippy --package glam-gc --all-targets --all-features -- -D warnings
  step "test (glam-gc, all features)"
  cargo test --package glam-gc --all-features
  step "persistent-edge codegen latch"
  crates/glam-gc/scripts/check-persistent-edge-codegen.sh
  step "unsafe-site audit"
  crates/glam-gc/scripts/audit-unsafe.sh
}

run_full() {
  run_all
  step "million-edge scale proofs"
  crates/glam-gc/scripts/check-scale.sh
}

run_dynamic() {
  run_full
  if ! command -v rustup >/dev/null 2>&1 ||
    ! rustup toolchain list 2>/dev/null | grep -q '^nightly'; then
    echo "SKIPPING Miri and sanitizers: no nightly toolchain installed." >&2
    echo "  The project pins only a stable toolchain (rust-toolchain.toml)." >&2
    echo "  Install a nightly with the miri and rust-src components, then run" >&2
    echo "  crates/glam-gc/scripts/check-miri.sh and" >&2
    echo "  crates/glam-gc/scripts/check-sanitizer.sh {address,thread} directly." >&2
    return 0
  fi
  step "Miri (glam-gc)"
  crates/glam-gc/scripts/check-miri.sh
  step "sanitizer: address (glam-gc)"
  crates/glam-gc/scripts/check-sanitizer.sh address
  step "sanitizer: thread (glam-gc)"
  crates/glam-gc/scripts/check-sanitizer.sh thread
}

case "$level" in
  all) run_all ;;
  full) run_full ;;
  dynamic) run_dynamic ;;
esac

step "glam-gc check.sh $level: OK"
