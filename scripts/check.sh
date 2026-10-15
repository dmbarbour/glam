#!/usr/bin/env bash
#
# Glam's verification entry point.
#
#   scripts/check.sh [fast|all|full]
#
# Levels are cumulative; each runs everything in the level before it:
#
#   fast  Formatting (workspace), Clippy and the test suite for the `glam`
#         package. The quick inner-loop gate.
#
#   all   (default) Adds the G0 semantic regressions and the glam-prof
#         profiling fixtures. The pre-commit gate for ordinary code changes.
#
#   full  Adds the expensive tier: the aggressive-GC verification pass over
#         Glam's tests and the cursor-stress proofs. Run before performance
#         work and after changes to runtime or collector behavior (see
#         docs/AgentContext.md, "Verification").
#
# glam-gc is a separate crate with its own gate,
# `crates/glam-gc/scripts/check.sh [all|full|dynamic]`; run it as well, at
# the matching level, only when changing glam-gc.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

level="${1:-all}"

usage() {
  sed -n '3,22p' "$0" | sed 's/^#\{0,1\} \{0,1\}//'
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
  step "clippy (glam, all targets, all features)"
  cargo clippy --package glam --all-targets --all-features -- -D warnings
  step "test (glam)"
  cargo test --package glam
}

run_all() {
  run_fast
  step "G0 semantic regressions"
  crates/glam-gc/scripts/check-g0-semantics.sh
  step "profiling fixtures (glam-prof)"
  scripts/check-profiling-fixtures.sh
}

run_full() {
  run_all
  step "aggressive-GC verification (glam)"
  cargo test --package glam -q --features aggressive-gc-verification
  step "cursor-stress proofs"
  scripts/check-cursor-stress.sh
}

case "$level" in
  fast) run_fast ;;
  all) run_all ;;
  full) run_full ;;
esac

step "check.sh $level: OK"
