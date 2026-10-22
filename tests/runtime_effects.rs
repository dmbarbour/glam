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

/// Answers `.next n` with `n + 1`, counting its calls in the effect's state,
/// and `.calls` with the count; a negative `n` fails.
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
                m.swap()?;
                m.dict_insert_path(["calls"])?;
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

/// The calls counted in the state on top of the stack, which stays.
fn calls(m: &mut Machine<'_, '_>) -> Result<i64, Error> {
    m.dup(1)?;
    m.dict_get("calls")?;
    m.eval()?;
    if m.kind()? == ValueKind::Dict {
        m.discard(1)?;
        return Ok(0);
    }
    m.int_pop()
}

/// Pushes the effect that `source` describes.
fn compile(m: &mut Machine<'_, '_>, source: &str) -> Result<(), Error> {
    m.dict_pack(Vec::<&str>::new())?;
    m.script_object(
        ["effects"],
        "g",
        format!("language g0\neffect = {source}\n"),
    )?;
    m.dict_get("effect")
}

/// Pushes `{}`, the state the effect starts from, and the effect.
fn effect(m: &mut Machine<'_, '_>, source: &str) -> Result<(), Error> {
    m.dict_pack(Vec::<&str>::new())?;
    compile(m, source)
}

/// Runs the effect from `{}`, leaving only its result.
fn run(
    m: &mut Machine<'_, '_>,
    source: &str,
    provider: &mut impl EffectProvider,
) -> Result<(), Error> {
    effect(m, source)?;
    m.effect_run(provider)?;
    m.swap()?;
    m.discard(1)
}

#[test]
fn a_provider_answers_an_effects_requests_on_the_turns_thread() {
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    let (result, calls, seen, state) = rt
        .run("m", |m| {
            effect(
                m,
                ".next 41 >>= (\\x -> .next x) >>= (\\y -> .calls >>= (\\n -> .get ['calls] >>= (\\seen -> .r [y, n, seen])))",
            )?;
            m.effect_run(&mut Counter)?;
            m.list_unpack(3)?;
            let seen = m.int_pop()?;
            let calls = m.int_pop()?;
            let result = m.int_pop()?;
            m.dict_get("calls")?;
            Ok((result, calls, seen, m.int_pop()?))
        })
        .expect("the turn should succeed");
    assert_eq!(result, 43, "two requests, answered in order");
    assert_eq!(
        (calls, seen, state),
        (2, 2, 2),
        "the provider keeps its count in the effect's state"
    );
}

/// Answers `.peek` with the depth of the stack it sees, or with a stack
/// it leaves too short.
struct Peek {
    short: bool,
}

impl EffectProvider for Peek {
    type Request = ();

    fn requests(&self) -> Vec<EffectRequestSpec<()>> {
        vec![EffectRequestSpec::new(
            "peek",
            ["peek_test", "request", "peek"],
            1,
            (),
        )]
    }

    fn answer(&mut self, m: &mut Machine<'_, '_>, _request: &()) -> Result<Answer, Error> {
        let depth = m.depth() as i64;
        m.discard(if self.short { 2 } else { 1 })?;
        m.int_push(depth);
        Ok(Answer::Return)
    }
}

#[test]
fn a_provider_answers_on_a_stack_of_its_own() {
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    rt.run("m", |m| {
        m.int_push(99);
        run(m, ".peek ()", &mut Peek { short: false })?;
        assert_eq!(m.int_pop()?, 2, "the provider sees its state and argument");
        let error = run(m, ".peek ()", &mut Peek { short: true })
            .expect_err("an answer must leave a state and a result");
        assert!(
            error.to_string().contains("a state and a result"),
            "{error}"
        );
        assert_eq!(m.int_pop()?, 99, "the turn's own stack is untouched");
        assert_eq!(m.depth(), 0);
        Ok(())
    })
    .expect("the turn should succeed");
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

/// Passes every host family through and answers nothing itself.
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
fn state_is_the_effect_s_own() {
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    let (read, count, seen, local) = rt
        .run("m", |m| {
            m.int_push(1);
            m.dict_pack(["seen"])?;
            compile(
                m,
                ".get ['seen] >>= (\\seen -> .set ['count] 5 =>> .set ['nested, 'deep] seen =>> .get ['nested, 'deep] >>= (\\deep -> .r [seen, deep]))",
            )?;
            m.effect_run(&mut Standard)?;
            m.list_unpack(2)?;
            let deep = m.int_pop()?;
            let read = (m.int_pop()?, deep);
            m.dict_unpack(["count", "nested", "seen"])?;
            let seen = m.int_pop()?;
            m.discard(1)?;
            let count = m.int_pop()?;
            m.local_get("count");
            Ok((read, count, seen, m.kind()?))
        })
        .expect("the turn should succeed");
    assert_eq!(read, (1, 1), "the effect reads the state it was given");
    assert_eq!((count, seen), (5, 1), "and returns the state it left");
    assert_eq!(local, ValueKind::Dict, "the machine's locals are untouched");
}

#[test]
fn the_whole_of_the_state_holds_the_reset_stack() {
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    rt.run("m", |m| {
        run(
            m,
            ".reset 'p (.get [] >>= (\\saved -> .set [] {} =>> .set [] saved =>> .shift 'p (\\k -> k 7)))",
            &mut Standard,
        )?;
        assert_eq!(m.int_pop()?, 7, "restoring the state restores the reset");
        let cleared = run(
            m,
            ".reset 'p (.set [] {} =>> .shift 'p (\\k -> k 7))",
            &mut Standard,
        )
        .expect_err("clearing the state clears the reset");
        assert!(cleared.to_string().contains("reset scope"), "{cleared}");
        Ok(())
    })
    .expect("the turn should succeed");
}

/// Defines `.env` itself, passing only the heap through.
struct OwnEnv;

impl EffectProvider for OwnEnv {
    type Request = ();

    fn passes_through(&self) -> EffectFamilies {
        EffectFamilies::HEAP
    }

    fn requests(&self) -> Vec<EffectRequestSpec<()>> {
        vec![EffectRequestSpec::new(
            "env",
            ["own_env_test", "request", "env"],
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
    let got = rt
        .run("m", |m| {
            run(m, ".heap.set ['k] 2 =>> .env ['anything]", &mut OwnEnv)?;
            m.int_pop()
        })
        .expect("the turn should succeed");
    assert_eq!(got, 7, "the provider's `.env`, not the machine's");
}

#[test]
fn the_heap_passes_through_to_the_shared_heap() {
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    let (read, shared) = rt
        .run("m", |m| {
            run(
                m,
                ".heap.set ['shared, 'n] 3 =>> .heap.rewrite ['shared, 'n] (\\n -> [n, n]) =>> .heap.get ['shared, 'n]",
                &mut Standard,
            )?;
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
            run(
                m,
                ".promise.new >>= (\\p -> .promise.status p.resolver >>= (\\before -> .promise.fulfill p.resolver 4 =>> .promise.status p.resolver >>= (\\after -> .r [before, after, p.promise])))",
                &mut Standard,
            )?;
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
        run(m, ".log 'warn { msg:{ text:\"careful\" } }", &mut Standard)?;
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
            run(m, ".env ['greeting]", &mut Standard)?;
            m.text_pop()
        })
        .expect("the turn should succeed");
    assert_eq!(greeting, "hello");
}
