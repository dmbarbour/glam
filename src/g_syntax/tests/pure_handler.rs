//! Pure handlers (`pure-handler-core`): every branch kept, failed ones with
//! their state, and a user's state and a handler's threaded through each.

use super::*;

/// Compiles `source` after its prelude and checks its `check`, a pure test
/// that evaluates to "pass".
#[track_caller]
fn check(source: &str) {
    let parsed = parse(&format!(
        "language g0\nimport 'std\nsearch operations start effect = list.search operations {{}} start effect\nvalues result = list.map (\\o -> o.value) result.ok\n{source}"
    ));
    assert_eq!(parsed.diagnostics, [], "{source}");
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, [], "{source}");
    let module = evaluated_module_value(&context, &lowered);
    assert_eq!(
        String::from_utf8(output_bytes(&fully_evaluated_value(
            resolved_value_at_path(&module, &["check"])
        )))
        .expect("a check's result is text"),
        "pass",
        "{source}"
    );
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
        "found = search {} 0 (.alt (.set ['n] 1) (.r ()) =>> .get ['n])\n",
        "whole = search {} 0 (.set [] {a:1} =>> .get [])\n",
        "check = if values found == [1, {}] and values whole == [{a:1}] then \"pass\" else \"fail\"\n",
    ));
}

#[test]
fn a_handler_s_operations_thread_its_state() {
    check(concat!(
        "next s = [{value: s.handler, state: (s with { handler ::= \\h -> h + 1 })}]\n",
        "stop s = [{failed: (s with { handler := \"stopped\" })}]\n",
        "pick s = [{value: 1, state: s}, {value: 2, state: s}]\n",
        "twice = .next >>= (\\a -> .next >>= (\\_ -> .r a))\n",
        "ops = {next:next, stop:stop, pick:pick, twice:twice}\n",
        "pair = search ops 10 (.next >>= (\\a -> .next >>= (\\b -> .r [a, b])))\n",
        "stopped = search ops 10 (.stop)\n",
        "picked = search ops 0 (.pick)\n",
        "doubled = search ops 10 (.twice)\n",
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
