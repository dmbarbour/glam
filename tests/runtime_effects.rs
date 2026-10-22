//! Effects a turn runs, whose requests a provider answers
//! (`api-effect-providers`, `effect-families-pass-through`).

use glam::reflection::EffectRequestSpec;
use glam::{Answer, EffectProvider, Error, ErrorKind, Machine, Runtime, RuntimeConfig, ValueKind};

#[derive(Clone)]
enum Ask {
    Next,
    Calls,
}

/// Answers `.next n` with `n + 1`, counting its calls in the machine's
/// local state, and `.calls` with the count; a negative `n` fails.
struct Counter;

impl EffectProvider for Counter {
    type Request = Ask;

    fn requests(&self) -> Vec<EffectRequestSpec<Ask>> {
        vec![
            EffectRequestSpec::new("next", ["counter_test", "request", "next"], 1, Ask::Next),
            EffectRequestSpec::new("calls", ["counter_test", "request", "calls"], 0, Ask::Calls),
        ]
    }

    fn answer(&mut self, m: &mut Machine<'_, '_>, request: &Ask) -> Result<Answer, Error> {
        match request {
            Ask::Next => {
                // The provider runs outside the effect's evaluation, so it
                // may evaluate its argument.
                let n = m.int_pop()?;
                if n < 0 {
                    return Err(m.fail());
                }
                let calls = calls(m)?;
                m.int_push(calls + 1);
                m.local_set("calls")?;
                m.int_push(n + 1);
            }
            Ask::Calls => {
                let calls = calls(m)?;
                m.int_push(calls);
            }
        }
        Ok(Answer::Return)
    }
}

/// The calls counted in the machine's local state.
fn calls(m: &mut Machine<'_, '_>) -> Result<i64, Error> {
    m.local_get("calls");
    if m.kind()? == ValueKind::Dict {
        m.discard(1)?;
        return Ok(0);
    }
    m.int_pop()
}

fn effect(m: &mut Machine<'_, '_>, source: &str) -> Result<(), Error> {
    m.dict_pack(Vec::<&str>::new())?;
    m.script_object(
        ["effects"],
        "g",
        format!("language g0\neffect = {source}\n"),
    )?;
    m.dict_get("effect")
}

#[test]
fn a_provider_answers_an_effects_requests_on_the_turns_thread() {
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    let (result, calls) = rt
        .run("m", |m| {
            effect(
                m,
                ".next 41 >>= (\\x -> .next x) >>= (\\y -> .calls >>= (\\n -> .r [y, n]))",
            )?;
            m.effect_run(&mut Counter)?;
            m.list_unpack(2)?;
            let calls = m.int_pop()?;
            Ok((m.int_pop()?, calls))
        })
        .expect("the turn should succeed");
    assert_eq!((result, calls), (43, 2), "two requests, answered in order");
}

#[test]
fn a_failed_request_fails_the_effect_as_a_backtrack() {
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    rt.run("m", |m| {
        effect(m, ".next (0 - 1)")?;
        let error = m.effect_run(&mut Counter).expect_err("the request fails");
        assert_eq!(error.kind(), ErrorKind::Backtrack, "{error}");
        Ok(())
    })
    .expect("the turn should succeed");
}
