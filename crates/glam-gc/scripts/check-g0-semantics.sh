#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/../../.." && pwd)"
cd "$root"

tests=(
  public_value_factories_reject_foreign_composite_members
  terminal_lazy_cache_releases_its_shared_source_after_active_snapshots
  a_lazy_task_that_waits_on_itself_is_poisoned_as_a_cycle
  promise_only_cycle_remains_blocked_without_poisoning_its_assignment
  workers_force_sparks_and_poll_ready_reflection_tasks
  compiled_function_values_reuse_one_shared_interaction_net
  ready_settlement_publishes_exited_once_and_retains_exit_errors
)

for test_name in "${tests[@]}"; do
  echo "G0 semantic regression: $test_name"
  cargo test --quiet --lib "$test_name"
done
