#!/usr/bin/env bash
#
# Workspace verification entry point.
#
#   scripts/check.sh [fast|all|full]
#
# Levels are cumulative; each runs everything in the level before it:
#
#   fast  Formatting, workspace Clippy, and the workspace test suite at default
#         features. The quick inner-loop gate.
#
#   all   (default) Adds the collector's own check suite (glam-gc all-features
#         tests, persistent-edge codegen latch, unsafe-site audit), the G0
#         semantic regressions, and the interaction-net profiling fixtures.
#         This is the pre-commit gate for ordinary code changes.
#
#   full  Adds the expensive, periodic tier: the aggressive-GC verification
#         pass over the whole workspace, the cursor-stress and million-edge
#         scale proofs, and -- when a nightly toolchain is installed -- Miri and
#         the sanitizers. Run before performance work and on a periodic basis.
#
# The project pins only a stable toolchain (rust-toolchain.toml), so Miri and
# the sanitizers are skipped with a notice unless a nightly is available.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

level="${1:-all}"

usage() {
  sed -n '3,21p' "$0" | sed 's/^#\{0,1\} \{0,1\}//'
}

case "$level" in
  fast | all | full) ;;
  -h | --help | help)
    usage
    exit 0
    ;;
  *)
    echo "usage: $(basename "$0") [fast|all|full]" >&2
    exit 2
    ;;
esac

step() { printf '\n==> %s\n' "$*"; }

run_fast() {
  step "fmt (workspace)"
  cargo fmt --check
  step "clippy (workspace, all targets, all features)"
  cargo clippy --workspace --all-targets --all-features -- -D warnings
  step "test (workspace)"
  cargo test --workspace
}

run_all() {
  run_fast
  step "glam-gc check suite (all-features tests, codegen latch, unsafe audit)"
  crates/glam-gc/scripts/check.sh
  step "G0 semantic regressions"
  crates/glam-gc/scripts/check-g0-semantics.sh
  step "interaction-net profiling fixtures"
  scripts/check-interaction-net-profiling.sh
}

run_nightly_checks() {
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

run_full() {
  run_all
  step "aggressive-GC verification (entire workspace)"
  cargo test --workspace -q --features aggressive-gc-verification
  step "cursor-stress proofs"
  scripts/check-cursor-stress.sh
  step "glam-gc million-edge scale proofs"
  crates/glam-gc/scripts/check-scale.sh
  run_nightly_checks
}

case "$level" in
  fast) run_fast ;;
  all) run_all ;;
  full) run_full ;;
esac

step "check.sh $level: OK"
