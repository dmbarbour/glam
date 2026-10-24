//! Pure handlers (`pure-handler-core`): every branch kept, failed ones with
//! their state, and a user's state and a handler's threaded through each.

use super::*;

/// Compiles `source` after its prelude and checks its `check`, a pure test
/// that evaluates to "pass".
#[track_caller]
fn check(source: &str) {
    let module = module(source);
    assert_eq!(
        String::from_utf8(output_bytes(&fully_evaluated_value(
            resolved_value_at_path(&module, &["check"])
        )))
        .expect("a check's result is text"),
        "pass",
        "{source}"
    );
}

#[track_caller]
fn module(source: &str) -> Value {
    let parsed = parse(&format!(
        "language g0\nimport 'std\nsearch operations start effect = list.search operations {{}} start effect\nvalues result = list.map (\\o -> o.value) result.ok\n{source}"
    ));
    assert_eq!(parsed.diagnostics, [], "{source}");
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, [], "{source}");
    evaluated_module_value(&context, &lowered)
}

/// Compiles `source` and checks that evaluating its `failing` fails with a
/// message containing `expected`.
#[track_caller]
fn check_failure(source: &str, expected: &str) {
    let module = module(source);
    let failing = value_at_atom_path(&module, &["failing"]).expect("the source has `failing`");
    let error = fully_evaluated_error(failing).to_string();
    assert!(error.contains(expected), "{source}: {error}");
}

#[test]
fn a_run_keeps_every_success_in_search_order() {
    check(concat!(
        "found = search {} 0 (.alt (.r 1) (.alt (.r 2) (.r 3)))\n",
        "check = if values found == [1, 2, 3] and list.len found.failed == 0 then \"pass\" else \"fail\"\n",
    ));
}

#[test]
fn a_failure_keeps_its_state() {
    check(concat!(
        "found = search {} 0 (.set ['n] 1 =>> .fail)\n",
        "check = if list.len found.ok == 0 and (list.head found.failed).user.n == 1 then \"pass\" else \"fail\"\n",
    ));
}

#[test]
fn a_continuation_follows_each_success() {
    check(concat!(
        "found = search {} 0 (.alt (.r 1) (.r 2) >>= (\\x -> (x == 2) =>> .r (x * 10)))\n",
        "check = if values found == [20] and list.len found.failed == 1 then \"pass\" else \"fail\"\n",
    ));
}

#[test]
fn each_branch_keeps_its_own_state() {
    check(concat!(
        // The second branch never set `n` (`dict-empty-is-a-value`).
        "found = search {} 0 (.alt (.set ['n] 1) (.r ()) =>> .get [] >>= (\\s -> .r (dict.default ['n] 'unset s)))\n",
        "whole = search {} 0 (.set [] {a:1} =>> .get [])\n",
        "check = if values found == [1, 'unset] and values whole == [{a:1}] then \"pass\" else \"fail\"\n",
    ));
}

#[test]
fn a_handler_s_operations_thread_its_state() {
    check(concat!(
        "next s = [{value: s.handler, state: (s with { handler ::= \\h -> h + 1 })}]\n",
        "stop s = [{failed: (s with { handler := \"stopped\" })}]\n",
        "pick s = [{value: 1, state: s}, {value: 2, state: s}]\n",
        "ops = {next:next, stop:stop, pick:pick}\n",
        "twice = .next >>= (\\a -> .next >>= (\\_ -> .r a))\n",
        "pair = search ops 10 (.next >>= (\\a -> .next >>= (\\b -> .r [a, b])))\n",
        "stopped = search ops 10 (.stop)\n",
        "picked = search ops 0 (.pick)\n",
        "doubled = search ops 10 twice\n",
        "check = if values pair == [[10, 11]] and (list.head pair.ok).handler == 12",
        " and list.len stopped.ok == 0 and (list.head stopped.failed).handler == \"stopped\"",
        " and values picked == [1, 2]",
        " and values doubled == [10] and (list.head doubled.ok).handler == 12",
        " then \"pass\" else \"fail\"\n",
    ));
}

#[test]
fn a_cut_keeps_its_first_success_and_the_failures_before_it() {
    check(concat!(
        "pick s = [{value: 1, state: s}, {value: 2, state: s}, {value: 3, state: s}]\n",
        "ops = {pick:pick}\n",
        "first = search ops 0 (.cut (.alt (.fail) (.alt (.r 1) (.r 2))))\n",
        "after = search ops 0 (.cut (.alt (.r 1) (.fail)))\n",
        "later = search ops 0 (.cut (.pick >>= (\\x -> (x == 2) =>> .r x)))\n",
        "check = if values first == [1] and list.len first.failed == 1",
        " and values after == [1] and list.len after.failed == 0",
        " and values later == [2] and list.len later.failed == 1",
        " then \"pass\" else \"fail\"\n",
    ));
}

#[test]
fn successes_need_no_branch_after_the_one_they_read() {
    // The second branch fails to evaluate if forced; the first success
    // never forces it.
    check(concat!(
        "boom = {eff: \\api -> api.nothing_here}\n",
        "found = search {} 0 (.alt (.r 1) boom)\n",
        "check = if (list.head found.ok).value == 1 then \"pass\" else \"fail\"\n",
    ));
}

#[test]
fn a_shift_resumes_its_reset_s_continuation() {
    check(concat!(
        "twice = search {} 0 (.reset 'k (.shift 'k (\\k -> k 1 >>= (\\a -> k 2 >>= (\\b -> .r [a, b]))) >>= (\\x -> .r (x * 10))))\n",
        "aborted = search {} 0 (.reset 'k (.shift 'k (\\_ -> .r 5) >>= (\\_ -> .r 1)) >>= (\\x -> .r (x + 1)))\n",
        "branches = search {} 0 (.reset 'k (.shift 'k (\\k -> .alt (k 1) (k 2)) >>= (\\x -> .r (x * 10))))\n",
        "check = if values twice == [[10, 20]] and values aborted == [6] and values branches == [10, 20]",
        " then \"pass\" else \"fail\"\n",
    ));
}

#[test]
fn a_shift_takes_the_resets_within_its_own() {
    check(concat!(
        "nested = search {} 0 (.reset 'outer (.reset 'inner (.shift 'outer (\\k -> k 1 >>= (\\x -> .r (x + 100)))) >>= (\\y -> .r (y * 2))))\n",
        "check = if values nested == [102] then \"pass\" else \"fail\"\n",
    ));
}

#[test]
fn the_reset_stack_lives_in_the_user_s_state() {
    check(concat!(
        "kept = search {} 0 (.reset 'p (.set ['visible] \"state\" =>> .shift 'p (\\k -> k ())) =>> .get ['visible])\n",
        "restored = search {} 0 (.reset 'p (.get [] >>= (\\saved -> .set [] {} =>> .set [] saved =>> .shift 'p (\\k -> k 7))))\n",
        "emptied = search {} 0 (.reset 'p (.r ()) =>> .get [])\n",
        "check = if values kept == [\"state\"] and values restored == [7] and values emptied == [{}]",
        " then \"pass\" else \"fail\"\n",
    ));
    check_failure(
        "failing = list.head (search {} 0 (.reset 'p (.set [] {} =>> .shift 'p (\\k -> k 7)))).ok\n",
        "not in reset scope",
    );
    check_failure(
        "failing = list.head (search {} 0 (.shift 'p (\\k -> k 7))).ok\n",
        "not in reset scope",
    );
}

#[test]
fn a_shift_leaves_the_cuts_within_its_reset() {
    // The shifting branch leaves the cut, so its results are not the cut's
    // success, and the cut's next branch still runs.
    check(concat!(
        "left = search {} 0 (.reset 'k (.cut (.alt (.shift 'k (\\k -> .alt (k 1) (k 2))) (.r 3))))\n",
        // The shifting branch reaches the outer cut first, which ends it;
        // the inner cut it left does not take it.
        "outer = search {} 0 (.cut (.alt (.reset 'k (.cut (.shift 'k (\\_ -> .r 1)))) (.r 2)))\n",
        "check = if values left == [1, 2, 3] and values outer == [1] then \"pass\" else \"fail\"\n",
    ));
}

#[test]
fn a_resumed_continuation_walks_its_cuts_anew() {
    check(concat!(
        "taken = .reset 'k (.cut (.shift 'k (\\k -> .r k) >>= (\\v -> .alt (.r v) (.r (v + 1)))))\n",
        "resumed = search {} 0 (taken >>= (\\k -> k 10))\n",
        "twice = search {} 0 (taken >>= (\\k -> k 10 >>= (\\a -> k 20 >>= (\\b -> .r [a, b]))))\n",
        "check = if values resumed == [10] and values twice == [[10, 20]] then \"pass\" else \"fail\"\n",
    ));
}

#[test]
fn a_fixpoint_sees_its_own_result_per_alternative() {
    check(concat!(
        "one = search {} 0 (.fix (\\x -> .r {a: 1, b: x.a}))\n",
        "two = search {} 0 (.fix (\\x -> .alt (.r {a: 1, b: x.a}) (.r {a: 2, b: x.a})))\n",
        "after = search {} 0 (.reset 'p (.cut (.fix (\\_ -> .r ())) =>> .shift 'p (\\k -> k 7)))\n",
        "continued = search {} 0 (.fix (\\x -> .r {a: 1, b: x.a}) >>= (\\v -> .r v.b))\n",
        "check = if list.map (\\v -> v.b) (values one) == [1] and list.map (\\v -> v.b) (values two) == [1, 2]",
        " and values after == [7] and values continued == [1]",
        " then \"pass\" else \"fail\"\n",
    ));
    check_failure(
        "failing = list.head (search {} 0 (.reset 'k (.fix (\\_ -> .shift 'k (\\k -> k 1))))).ok\n",
        "not in reset scope",
    );
}
