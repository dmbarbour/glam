#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

# Keep the profiling-only accounting and deterministic W4E work fuse in the
# routine verification set without repeating every ordinary Glam test under
# an instrumented build.
tests=(
  api::tests::interaction_net_profiles_are_runtime_local
  core_net::tests::profiling_classifies_each_committed_rule_family_exactly_once
  core_net::tests::profiling_semantic_signature_is_independent_of_ready_pair_order
  core_net::tests::profiling_counts_calls_only_when_the_claimed_rewrite_commits
  core_net::tests::profiling_counts_operator_calls_only_when_completion_commits
  eval::tests::wrapper_returning_function_then_accepts_remaining_application
  eval::tests::wrapper_application_budget_probe_yields_without_publishing_a_cache
)

available="$(
  cargo test --quiet --package glam --features interaction-net-profiling \
    --lib -- --list
)"

for test_name in "${tests[@]}"; do
  if ! rg --fixed-strings --line-regexp --quiet \
    -- "$test_name: test" <<<"$available"; then
    echo "missing interaction-net profiling regression: $test_name" >&2
    exit 1
  fi
  echo "interaction-net profiling regression: $test_name"
  cargo test --quiet --package glam --features interaction-net-profiling \
    --lib "$test_name" -- --exact
done
