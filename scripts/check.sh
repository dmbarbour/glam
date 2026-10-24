#!/usr/bin/env bash
#
# Glam's verification entry point.
#
#   scripts/check.sh [fast|all|full] [--record [--timeout SECONDS]]
#   scripts/check.sh --verify [fast|all|full]
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
# --record is the gate to commit by. It formats the workspace (`cargo fmt`),
# then runs the level under a timeout into a fresh target/check-<level>.log,
# which ends with `exit <status>`, and prints the log's tail. The timeout
# defaults to fast 600, all 900 and full 1500 seconds, a few times a quiet
# run. On success, with the working tree unchanged while it ran, it
# stamps target/check-<level>.stamp with the tree a commit of the whole
# working tree would record, untracked files included.
#
# --verify succeeds only when the staged tree, the one `git commit` records,
# carries the stamp of the level or of a higher one: guard each commit with
# it, so a commit holds exactly what a gate passed.
#
# glam-gc is a separate crate with its own gate,
# `crates/glam-gc/scripts/check.sh [all|full|dynamic]`; run it as well, at
# the matching level, only when changing glam-gc.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

usage() {
  sed -n '3,35p' "$0" | sed 's/^#\{0,1\} \{0,1\}//'
}

usage_line() {
  echo "usage: $(basename "$0") [fast|all|full] [--record [--timeout SECONDS]] | --verify [fast|all|full]" >&2
  exit 2
}

level=all
mode=run
timeout=
while [[ $# -gt 0 ]]; do
  case "$1" in
    fast | all | full) level="$1" ;;
    --record) mode=record ;;
    --verify) mode=verify ;;
    --timeout)
      [[ $# -ge 2 && "$2" =~ ^[0-9]+$ ]] || usage_line
      timeout="$2"
      shift
      ;;
    -h | --help | help)
      usage
      exit 0
      ;;
    *) usage_line ;;
  esac
  shift
done
[[ -z "$timeout" || "$mode" == record ]] || usage_line

# The tree a commit of the whole working tree would record: tracked and
# untracked files as they stand, ignored ones aside. A scratch index keeps
# the real one as it was.
working_tree() {
  (
    scratch="$(mktemp -d)"
    trap 'rm -rf "$scratch"' EXIT
    cp "$(git rev-parse --git-path index)" "$scratch/index"
    GIT_INDEX_FILE="$scratch/index" git add --all
    GIT_INDEX_FILE="$scratch/index" git write-tree
  )
}

if [[ "$mode" == verify ]]; then
  staged="$(git write-tree)"
  case "$level" in
    fast) levels="fast all full" ;;
    all) levels="all full" ;;
    full) levels="full" ;;
  esac
  for stamped in $levels; do
    stamp="target/check-$stamped.stamp"
    if [[ -f "$stamp" && "$(cat "$stamp")" == "$staged" ]]; then
      echo "check.sh $level: the staged tree passed the $stamped gate"
      exit 0
    fi
  done
  echo "check.sh $level: no gate at this level passed the staged tree" >&2
  exit 1
fi

if [[ "$mode" == record ]]; then
  case "$level" in
    fast) timeout="${timeout:-600}" ;;
    all) timeout="${timeout:-900}" ;;
    full) timeout="${timeout:-1500}" ;;
  esac
  log="target/check-$level.log"
  stamp="target/check-$level.stamp"
  mkdir -p target
  rm -f "$log" "$stamp"
  cargo fmt
  before="$(working_tree)"
  status=0
  timeout --kill-after=60 "$timeout" "$0" "$level" >"$log" 2>&1 || status=$?
  if [[ "$status" -eq 124 || "$status" -eq 137 ]]; then
    echo "timed out after ${timeout}s" >>"$log"
  fi
  if [[ "$status" -eq 0 && "$(working_tree)" != "$before" ]]; then
    echo "the working tree changed while the gate ran" >>"$log"
    status=1
  fi
  echo "exit $status" >>"$log"
  if [[ "$status" -eq 0 ]]; then
    echo "$before" >"$stamp"
  fi
  tail -n 3 "$log"
  exit "$status"
fi

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
