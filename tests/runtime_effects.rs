//! Effects a turn runs, whose requests a provider answers
//! (`api-effect-providers`, `effect-families-pass-through`).

use glam::reflection::EffectRequestSpec;
use glam::{
    Answer, EffectFamilies, EffectProvider, Error, ErrorKind, Machine, Runtime, RuntimeConfig,
    ValueKind,
};

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

/// Passes every standard family through and answers nothing itself.
struct Standard;

impl EffectProvider for Standard {
    type Request = ();

    fn requests(&self) -> Vec<EffectRequestSpec<()>> {
        Vec::new()
    }

    fn answer(&mut self, _m: &mut Machine<'_, '_>, _request: &()) -> Result<Answer, Error> {
        Err(Error::new("no request of its own"))
    }
}

#[test]
fn state_passes_through_to_the_machine_s_locals() {
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    let (seen, deep, count) = rt
        .run("m", |m| {
            m.int_push(1);
            m.local_set("seen")?;
            effect(
                m,
                ".get ['seen] >>= (\\seen -> .set ['count] 5 =>> .set ['nested, 'deep] seen =>> .get ['nested, 'deep] >>= (\\deep -> .r [seen, deep]))",
            )?;
            m.effect_run(&mut Standard)?;
            m.list_unpack(2)?;
            let deep = m.int_pop()?;
            let seen = m.int_pop()?;
            m.local_get("count");
            Ok((seen, deep, m.int_pop()?))
        })
        .expect("the turn should succeed");
    assert_eq!((seen, deep, count), (1, 1, 5));
}

#[test]
fn the_empty_path_names_the_whole_of_the_locals() {
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    let (restored, seen) = rt
        .run("m", |m| {
            m.int_push(1);
            m.local_set("seen")?;
            effect(
                m,
                ".get [] >>= (\\whole -> .set [] { restored:whole.seen } =>> .r ())",
            )?;
            m.effect_run(&mut Standard)?;
            m.discard(1)?;
            m.local_get("restored");
            let restored = m.int_pop()?;
            m.local_get("seen");
            Ok((restored, m.kind()?))
        })
        .expect("the turn should succeed");
    assert_eq!(
        (restored, seen),
        (1, ValueKind::Dict),
        "the set replaced every local"
    );
}

/// Defines `.get` itself, passing only the heap through.
struct OwnGet;

impl EffectProvider for OwnGet {
    type Request = ();

    fn passes_through(&self) -> EffectFamilies {
        EffectFamilies::HEAP
    }

    fn requests(&self) -> Vec<EffectRequestSpec<()>> {
        vec![EffectRequestSpec::new(
            "get",
            ["own_get_test", "request", "get"],
            1,
            (),
        )]
    }

    fn answer(&mut self, m: &mut Machine<'_, '_>, _request: &()) -> Result<Answer, Error> {
        m.discard(1)?;
        m.int_push(7);
        Ok(Answer::Return)
    }
}

#[test]
fn a_provider_defines_a_family_it_does_not_pass_through() {
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    let (got, seen) = rt
        .run("m", |m| {
            m.int_push(1);
            m.local_set("seen")?;
            effect(m, ".heap.set ['k] 2 =>> .get ['seen]")?;
            m.effect_run(&mut OwnGet)?;
            let got = m.int_pop()?;
            m.local_get("seen");
            Ok((got, m.int_pop()?))
        })
        .expect("the turn should succeed");
    assert_eq!((got, seen), (7, 1), "the provider's `.get`, not the locals");
}

#[test]
fn the_heap_passes_through_to_the_shared_heap() {
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    let (read, shared) = rt
        .run("m", |m| {
            effect(
                m,
                ".heap.set ['shared, 'n] 3 =>> .heap.rewrite ['shared, 'n] (\\n -> [n, n]) =>> .heap.get ['shared, 'n]",
            )?;
            m.effect_run(&mut Standard)?;
            m.list_unpack(2)?;
            m.discard(1)?;
            let read = m.int_pop()?;
            m.heap_get(["shared", "n"]);
            m.list_unpack(2)?;
            m.discard(1)?;
            Ok((read, m.int_pop()?))
        })
        .expect("the turn should succeed");
    assert_eq!((read, shared), (3, 3));
}

#[test]
fn promises_pass_through_to_the_runtime_s_promises() {
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    let (before, after, value) = rt
        .run("m", |m| {
            effect(
                m,
                ".promise.new >>= (\\p -> .promise.status p.resolver >>= (\\before -> .promise.fulfill p.resolver 4 =>> .promise.status p.resolver >>= (\\after -> .r [before, after, p.promise])))",
            )?;
            m.effect_run(&mut Standard)?;
            m.list_unpack(3)?;
            let value = m.int_pop()?;
            let after = m.atom_pop()?;
            Ok((m.atom_pop()?, after, value))
        })
        .expect("the turn should succeed");
    assert_eq!(
        (before.as_str(), after.as_str(), value),
        ("pending", "fulfilled", 4)
    );
}

#[test]
fn a_log_goes_to_the_machine_s_diagnostics_output() {
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    let before = rt
        .run((), |root| Ok(root.port_available("diagnostics")))
        .expect("a turn");
    rt.run("m", |m| {
        effect(m, ".log 'warn { msg:{ text:\"careful\" } }")?;
        m.effect_run(&mut Standard)?;
        m.discard(1)
    })
    .expect("the turn should succeed");
    let text = rt
        .run((), |root| {
            assert_eq!(root.port_available("diagnostics"), before + 1);
            root.port_read("diagnostics", before + 1)?;
            root.dict_get_path(["emission", "msg", "text"])?;
            root.text_pop()
        })
        .expect("the root reads its diagnostics input");
    assert_eq!(text, "careful");
}

#[test]
fn the_environment_passes_through_to_the_machine_s() {
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    let greeting = rt
        .run("m", |m| {
            m.dict_pack(Vec::<&str>::new())?;
            m.script_object(
                ["shares"],
                "g",
                "language g0\nobject share with\n    greeting = \"hello\"\n",
            )?;
            m.dict_get("share")?;
            m.env_mixin_set()?;
            effect(m, ".env ['greeting]")?;
            m.effect_run(&mut Standard)?;
            m.text_pop()
        })
        .expect("the turn should succeed");
    assert_eq!(greeting, "hello");
}
