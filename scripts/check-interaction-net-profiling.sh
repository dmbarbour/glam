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
  eval::net::driver_tests::current_callable_profile_counts_only_terminal_call_rewrites
  eval::net::driver_tests::callable_profile_keeps_immediate_and_cached_paths_checkpoint_free
  eval::net::driver_tests::callable_profile_records_checkpoint_install_resume_replace_and_terminalize
  eval::net::driver_tests::callable_profile_records_exact_dependency_retry_and_stale_admission
  eval::net::driver_tests::unsupported_checkpoint_boundary_terminalizes_the_exact_generation
  eval::value::w4_tests::object_checkpoint_does_not_replay_mixin_stages_after_route_loss
  eval::value::w4_tests::public_pure_construction_survives_route_loss_without_repeating_effect_or_continuation
  eval::value::w4_tests::public_pure_construction_retains_both_selector_observations_across_route_loss
  eval::value::w4_tests::public_pure_construction_retains_exposed_port_demand_across_route_loss
  eval::value::w4_tests::public_construction_waits_for_first_result_before_ready_right_branch
  eval::value::w4_tests::public_construction_retains_first_result_while_second_promise_blocks
  eval::value::w4_tests::public_construction_exposed_port_promise_retains_one_context_after_route_loss
  eval::value::w4_tests::public_construction_builder_operand_promise_resumes_same_program
  g_syntax::tests::interaction_net_construction_backtracks_and_requires_one_result
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
