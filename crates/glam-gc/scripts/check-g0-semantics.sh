#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/../../.." && pwd)"
cd "$root"

tests=(
  composite_construction_preserves_provenance_errors
  terminal_lazy_cache_releases_its_shared_source_after_active_snapshots
  a_lazy_task_that_waits_on_itself_is_poisoned_as_a_cycle
  promise_only_cycle_remains_blocked_without_poisoning_its_assignment
  workers_force_sparks_and_poll_ready_reflection_tasks
  compiled_function_values_reuse_one_shared_interaction_net
  ready_settlement_publishes_exited_once_and_retains_exit_errors
)

# Fail closed: a filter that matches zero tests must be an error, not a vacuous
# pass. Capture the available lib tests once and require each name to resolve to
# a real test leaf before running it.
available="$(cargo test --quiet --lib -- --list)"

for test_name in "${tests[@]}"; do
  if ! grep -qE "(^|::)${test_name}: test$" <<<"$available"; then
    echo "missing G0 semantic regression: $test_name" >&2
    exit 1
  fi
  echo "G0 semantic regression: $test_name"
  cargo test --quiet --lib "$test_name"
done
