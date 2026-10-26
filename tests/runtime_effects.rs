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
    if !m.dict_defines("calls")? {
        return Ok(0);
    }
    m.dup(1)?;
    m.dict_get("calls")?;
    m.int_pop()
}

/// Pushes the effect that `source` describes.
fn compile(m: &mut Machine<'_, '_>, source: &str) -> Result<(), Error> {
    m.dict_pack(Vec::<&str>::new())?;
    m.script_object(
        ["effects"],
        "g",
        format!("language g0\nimport 'std as std\neffect = {source}\n"),
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
        assert!(error.to_string().contains("a state and its"), "{error}");
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
            Ok((read, count, seen, m.local_defines("count")))
        })
        .expect("the turn should succeed");
    assert_eq!(read, (1, 1), "the effect reads the state it was given");
    assert_eq!((count, seen), (5, 1), "and returns the state it left");
    assert!(!local, "the machine's locals are untouched");
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

/// Defines `.env` and `.port` itself, passing only the heap through. Each
/// answers 7; its request is its arity.
struct OwnEnv;

impl EffectProvider for OwnEnv {
    type Request = usize;

    fn passes_through(&self) -> EffectFamilies {
        EffectFamilies::HEAP
    }

    fn requests(&self) -> Vec<EffectRequestSpec<usize>> {
        vec![
            EffectRequestSpec::new("env", ["own_env_test", "request", "env"], 1, 1),
            EffectRequestSpec::new("port", ["own_env_test", "request", "port"], 0, 0),
        ]
    }

    fn answer(&mut self, m: &mut Machine<'_, '_>, arity: &usize) -> Result<Answer, Error> {
        m.discard(*arity)?;
        m.int_push(7);
        Ok(Answer::Return)
    }
}

#[test]
fn a_provider_defines_a_family_it_does_not_pass_through() {
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    let got = rt
        .run("m", |m| {
            run(
                m,
                ".heap.set ['k] 2 =>> .env ['anything] >>= (\\e -> .port >>= (\\p -> .r (e + p)))",
                &mut OwnEnv,
            )?;
            m.int_pop()
        })
        .expect("the turn should succeed");
    assert_eq!(
        got, 14,
        "the provider's `.env` and `.port`, not the machine's"
    );
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
            m.heap_get(["shared", "n"])?;
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
    let (text, severity) = rt
        .run((), |root| {
            assert_eq!(root.port_available("diagnostics"), before + 1);
            root.port_read("diagnostics", before + 1)?;
            root.dup(1)?;
            root.dict_get_path(["msg", "text"])?;
            let text = root.text_pop()?;
            root.dict_get_path(["msg", "severity"])?;
            Ok((text, root.atom_pop()?))
        })
        .expect("the root reads its diagnostics input");
    assert_eq!(
        (text.as_str(), severity.as_str()),
        ("careful", "warn"),
        "the port carries the enriched message"
    );
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
            m.env_overlay()?;
            run(m, ".env.lookup ['greeting]", &mut Standard)?;
            m.text_pop()
        })
        .expect("the turn should succeed");
    assert_eq!(greeting, "hello");
}

#[test]
fn an_effect_overlays_and_clears_the_turn_machine_s_environment() {
    // `machines-inherit-an-environment`: an effect a turn runs overlays its
    // machine's environment, stacked on the machine's share, and clears it,
    // as a task does its machine's.
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    let (greeting, farewell, cleared) = rt
        .run("m", |m| {
            m.dict_pack(Vec::<&str>::new())?;
            m.script_object(
                ["shares"],
                "g",
                "language g0\nobject share with\n    greeting = \"hello\"\n",
            )?;
            m.dict_get("share")?;
            m.env_overlay()?;
            run(
                m,
                ".env.overlay (std.object_from_dict {farewell:\"bye\"}) =>> .env.lookup ['greeting] >>= (\\g -> .env.lookup ['farewell] >>= (\\f -> .r [g, f]))",
                &mut Standard,
            )?;
            m.list_unpack(2)?;
            let farewell = m.text_pop()?;
            let greeting = m.text_pop()?;
            // A lookup is lazy: what is undefined is an error once forced.
            let cleared = run(m, ".env.clear =>> .env.lookup ['greeting]", &mut Standard)
                .and_then(|()| m.text_pop())
                .is_err();
            Ok((greeting, farewell, cleared))
        })
        .expect("the turn should succeed");
    assert_eq!(
        (greeting.as_str(), farewell.as_str(), cleared),
        ("hello", "bye", true)
    );
}

/// Answers `.mark` by marking the machine's local state, and `.choose n`
/// with the alternatives 1 to `n`.
#[derive(Clone)]
enum Host {
    Mark,
    Choose,
}

struct Marker;

impl EffectProvider for Marker {
    type Request = Host;

    fn requests(&self) -> Vec<EffectRequestSpec<Host>> {
        vec![
            EffectRequestSpec::new("mark", ["marker_test", "request", "mark"], 0, Host::Mark),
            EffectRequestSpec::new(
                "choose",
                ["marker_test", "request", "choose"],
                1,
                Host::Choose,
            ),
        ]
    }

    fn answer(&mut self, m: &mut Machine<'_, '_>, request: &Host) -> Result<Answer, Error> {
        match request {
            Host::Mark => {
                m.int_push(1);
                m.local_set("marked")?;
                m.unit_push();
                Ok(Answer::Return)
            }
            Host::Choose => {
                let count = m.int_pop()?;
                for choice in 1..=count {
                    m.int_push(choice);
                }
                Ok(Answer::Alternatives(count as usize))
            }
        }
    }
}

/// The counter's requests and the marker's together.
struct Both;

#[derive(Clone)]
enum Either {
    Counter(Ask),
    Marker(Host),
}

impl EffectProvider for Both {
    type Request = Either;

    fn requests(&self) -> Vec<EffectRequestSpec<Either>> {
        Counter
            .requests()
            .into_iter()
            .map(|spec| spec.map_request(Either::Counter))
            .chain(
                Marker
                    .requests()
                    .into_iter()
                    .map(|spec| spec.map_request(Either::Marker)),
            )
            .collect()
    }

    fn answer(&mut self, m: &mut Machine<'_, '_>, request: &Either) -> Result<Answer, Error> {
        match request {
            Either::Counter(ask) => Counter.answer(m, ask),
            Either::Marker(host) => Marker.answer(m, host),
        }
    }
}

/// The evaluated kind of the shared heap's value at `key`.
/// Whether the shared heap defines `key` (`dict-empty-is-a-value`).
fn heap_defined(m: &mut Machine<'_, '_>, key: &str) -> Result<bool, Error> {
    match m.heap_try_get([key]) {
        Ok(()) => m.discard(1).map(|()| true),
        Err(error) if error.kind() == ErrorKind::Backtrack => Ok(false),
        Err(error) => Err(error),
    }
}

fn heap_kind(m: &mut Machine<'_, '_>, key: &str) -> Result<ValueKind, Error> {
    m.heap_get([key])?;
    m.eval()?;
    let kind = m.kind()?;
    m.discard(1)?;
    Ok(kind)
}

#[test]
fn a_failed_first_operand_leaves_nothing_behind() {
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    rt.run("m", |m| {
        run(
            m,
            ".alt ((.heap.set ['written] 1) =>> .mark =>> (.next 1 >>= (\\_ -> .fail))) (.calls)",
            &mut Both,
        )?;
        assert_eq!(
            m.int_pop()?,
            0,
            "the effect's state rolls back with its branch"
        );
        assert!(!heap_defined(m, "written")?, "the heap rolls back");
        assert!(
            !m.local_defines("marked"),
            "what the provider did rolls back"
        );
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn a_choice_commits_its_first_operand_once_it_succeeds() {
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    let written = rt
        .run("m", |m| {
            run(
                m,
                ".alt (.heap.set ['written] \"yes\") (.r ())",
                &mut Standard,
            )?;
            m.discard(1)?;
            m.heap_get(["written"])?;
            m.text_pop()
        })
        .expect("the turn should succeed");
    assert_eq!(written, "yes");
}

#[test]
fn within_a_cut_a_later_failure_reaches_the_second_operand() {
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    let (read, written) = rt
        .run("m", |m| {
            run(
                m,
                ".cut ((.alt ((.heap.set ['k] 1) =>> .r 1) ((.heap.set ['k] 2) =>> .r 2)) >>= (\\x -> (x == 2) =>> .heap.get ['k]))",
                &mut Standard,
            )?;
            let read = m.int_pop()?;
            m.heap_get(["k"])?;
            Ok((read, m.int_pop()?))
        })
        .expect("the turn should succeed");
    assert_eq!((read, written), (2, 2));
}

#[test]
fn a_provider_answers_with_alternatives() {
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    rt.run("m", |m| {
        run(m, ".choose 3", &mut Marker)?;
        assert_eq!(m.int_pop()?, 1, "outside a cut, the first is final");
        run(
            m,
            ".cut (.choose 3 >>= (\\x -> (x == 3) =>> .r x))",
            &mut Marker,
        )?;
        assert_eq!(
            m.int_pop()?,
            3,
            "within a cut, a later failure takes the next"
        );
        let none = run(m, ".choose 0", &mut Marker).expect_err("no alternative fails");
        assert_eq!(none.kind(), ErrorKind::Backtrack, "{none}");
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn within_the_turn_s_cut_a_failed_effect_cut_leaves_nothing_behind() {
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    rt.run("m", |m| {
        m.cut(|m| {
            run(
                m,
                ".alt ((.heap.set ['first] 1) =>> (.fail)) (.r ())",
                &mut Standard,
            )?;
            m.discard(1)?;
            assert!(!heap_defined(m, "first")?);
            let failed = run(
                m,
                ".cut ((.heap.set ['second] 1) =>> (.fail))",
                &mut Standard,
            )
            .expect_err("the effect's cut fails");
            assert_eq!(failed.kind(), ErrorKind::Backtrack, "{failed}");
            assert!(!heap_defined(m, "second")?);
            Ok(())
        })
    })
    .expect("the turn should succeed");
}

#[test]
fn outside_the_turn_s_cut_a_failed_effect_cut_leaves_nothing_behind() {
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    rt.run("m", |m| {
        let failed = run(m, ".cut ((.heap.set ['k] 1) =>> (.fail))", &mut Standard)
            .expect_err("the effect's cut fails");
        assert_eq!(failed.kind(), ErrorKind::Backtrack, "{failed}");
        assert!(!heap_defined(m, "k")?);
        Ok(())
    })
    .expect("the turn should succeed");
}

/// Answers `.ready` with the shared heap's `ready`, or with pushback while
/// it holds none, signalling its first wait.
struct Ready {
    waiting: Option<std::sync::mpsc::Sender<()>>,
    calls: usize,
}

impl EffectProvider for Ready {
    type Request = ();

    fn requests(&self) -> Vec<EffectRequestSpec<()>> {
        vec![EffectRequestSpec::new(
            "ready",
            ["ready_test", "request", "ready"],
            0,
            (),
        )]
    }

    fn answer(&mut self, m: &mut Machine<'_, '_>, _request: &()) -> Result<Answer, Error> {
        self.calls += 1;
        if !heap_defined(m, "ready")? {
            if let Some(waiting) = self.waiting.take() {
                waiting
                    .send(())
                    .expect("the writer waits for the first attempt");
            }
            return Err(m.retry());
        }
        m.heap_get(["ready"])?;
        Ok(Answer::Return)
    }
}

#[test]
fn an_effect_cut_that_meets_pushback_runs_again_once_what_it_read_changes() {
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    let (waiting, waited) = std::sync::mpsc::channel();
    let (result, calls, attempted) = rt
        .run("p", |p| {
            let fork = p.fork("reader", move |r| {
                let mut ready = Ready {
                    waiting: Some(waiting),
                    calls: 0,
                };
                run(
                    r,
                    ".cut ((.heap.set ['attempted] 1) =>> .ready)",
                    &mut ready,
                )?;
                r.int_push(ready.calls as i64);
                Ok(())
            })?;
            waited.recv().expect("the reader's first attempt waits");
            p.int_push(1);
            p.heap_set(["ready"])?;
            p.join(fork)?;
            p.take("reader", 2)?;
            let calls = p.int_pop()?;
            let result = p.int_pop()?;
            p.heap_get(["attempted"])?;
            Ok((result, calls, p.int_pop()?))
        })
        .expect("the turn should succeed");
    assert_eq!(result, 1);
    assert_eq!(calls, 2, "the cut ran again once `ready` changed");
    assert_eq!(attempted, 1, "the second attempt committed");
}

/// Answers `.spawn` by forking a child that writes the shared heap, which
/// it leaves for the answer's end to join.
struct Spawner;

impl EffectProvider for Spawner {
    type Request = ();

    fn requests(&self) -> Vec<EffectRequestSpec<()>> {
        vec![EffectRequestSpec::new(
            "spawn",
            ["spawn_test", "request", "spawn"],
            0,
            (),
        )]
    }

    fn answer(&mut self, m: &mut Machine<'_, '_>, _request: &()) -> Result<Answer, Error> {
        let _unjoined = m.fork("writer", |w| {
            w.int_push(1);
            w.heap_set(["forked"])
        })?;
        m.unit_push();
        Ok(Answer::Return)
    }
}

#[test]
fn an_answer_s_forks_join_before_its_cut_ends() {
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    rt.run("m", |m| {
        run(m, ".alt (.spawn =>> (.fail)) (.r ())", &mut Spawner)?;
        m.discard(1)?;
        assert!(
            !heap_defined(m, "forked")?,
            "the failed fork's write rolls back"
        );
        run(m, ".alt (.spawn) (.r ())", &mut Spawner)?;
        m.discard(1)?;
        assert_eq!(
            heap_kind(m, "forked")?,
            ValueKind::Number,
            "the committed fork's write stands"
        );
        Ok(())
    })
    .expect("the turn should succeed");
}

/// Answers `.pause` with unit once the test lets it go: it says it has
/// reached the pause, then waits, so the test can act in between.
struct Pause {
    reached: std::sync::mpsc::Sender<()>,
    resume: std::sync::mpsc::Receiver<()>,
}

impl EffectProvider for Pause {
    type Request = ();

    fn requests(&self) -> Vec<EffectRequestSpec<()>> {
        vec![EffectRequestSpec::new(
            "pause",
            ["pause_test", "request", "pause"],
            0,
            (),
        )]
    }

    fn answer(&mut self, m: &mut Machine<'_, '_>, _request: &()) -> Result<Answer, Error> {
        self.reached.send(()).expect("the test listens");
        self.resume.recv().expect("the test lets the pause go");
        m.unit_push();
        Ok(Answer::Return)
    }
}

#[test]
fn try_answers_with_its_outcome_in_an_effect_a_turn_runs() {
    // `api-refl-try`: as `m.atomic` is, `.try` is run once.
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    let text = rt
        .run("m", |m| {
            run(
                m,
                ".try (.r \"A\") >>= (\\a -> .try (.heap.set ['k] \"set\" =>> .fail) >>= (\\f -> .alt (.heap.try_get ['k] =>> .r \"fail\") (.r (if a.ok == \"A\" and f == 'fail then \"pass\" else \"fail\"))))",
                &mut Counter,
            )?;
            m.text_pop()
        })
        .expect("the turn should succeed");
    assert_eq!(text, "pass");
}

#[test]
fn try_answers_conflict_when_another_commit_intervenes() {
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    rt.run("setup", |m| {
        m.int_push(1);
        m.heap_set(["x"])
    })
    .expect("the setup should commit");
    let (reached_sender, reached) = std::sync::mpsc::channel();
    let (resume, resume_receiver) = std::sync::mpsc::channel();
    let text = rt
        .run((), |root| {
            let worker = root.fork("worker", move |m| {
                let mut pause = Pause {
                    reached: reached_sender,
                    resume: resume_receiver,
                };
                // The `.try` reads `x`, then pauses before it commits.
                run(
                    m,
                    ".try (.heap.get ['x] >>= (\\x -> .pause =>> .heap.set ['y] x)) >>= (\\r -> .r (if std.dict.defines ['conflict] r then \"conflict\" else \"no conflict\"))",
                    &mut pause,
                )
            })?;
            reached.recv().expect("the worker reaches its pause");
            root.int_push(2);
            root.heap_set(["x"])?;
            resume.send(()).expect("the worker waits");
            root.join(worker)?;
            root.take("worker", 1)?;
            root.text_pop()
        })
        .expect("the turn should succeed");
    assert_eq!(
        text, "conflict",
        "the `.try` answers, without running again"
    );
}

#[test]
fn try_answers_wait_on_a_promise_in_an_effect_a_turn_runs() {
    // The turn answers the effect's requests through promises it resolves
    // itself; only a promise outside them is a wait. Once the promise is
    // kept, a second `.try` succeeds. The continuation keeps the resolver,
    // without which the promise would be broken, not pending.
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    let text = rt
        .run("m", |m| {
            run(
                m,
                ".promise.new >>= (\\p -> .heap.set ['k] \"set\" =>> .try (if p.promise == 1 then .r \"kept\" else .r \"other\") >>= (\\r -> .promise.fulfill p.resolver 1 =>> .try (if p.promise == 1 then .r \"kept\" else .r \"other\") >>= (\\s -> .heap.get ['k] >>= (\\k -> .r (if std.dict.defines ['wait] r then (if s.ok == \"kept\" then k else \"not kept\") else \"no wait\")))))",
                &mut Counter,
            )?;
            m.text_pop()
        })
        .expect("the turn should succeed");
    assert_eq!(text, "set");
}

/// Answers `.signal` with unit, telling the test the first time.
struct Signal {
    sent: Option<std::sync::mpsc::Sender<()>>,
}

impl EffectProvider for Signal {
    type Request = ();

    fn requests(&self) -> Vec<EffectRequestSpec<()>> {
        vec![EffectRequestSpec::new(
            "signal",
            ["signal_test", "request", "signal"],
            0,
            (),
        )]
    }

    fn answer(&mut self, m: &mut Machine<'_, '_>, _request: &()) -> Result<Answer, Error> {
        if let Some(sent) = self.sent.take() {
            sent.send(()).expect("the test listens");
        }
        m.unit_push();
        Ok(Answer::Return)
    }
}

#[test]
fn a_cut_that_fails_on_a_try_s_wait_parks_until_the_promise_is_kept() {
    // `api-refl-try`: what a `.try` waited on stays with the cut around it,
    // so a cut that fails on the `wait` runs again once the promise is kept.
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    let (signal_sender, signal) = std::sync::mpsc::channel();
    let text = rt
        .run((), |root| {
            root.promise_new();
            root.local_set("resolver")?;
            root.heap_set(["p"])?;
            let worker = root.fork("worker", move |m| {
                let mut signal = Signal {
                    sent: Some(signal_sender),
                };
                m.cut(|m| {
                    run(
                        m,
                        ".heap.get ['p] >>= (\\p -> .try (if p == 1 then .r \"kept\" else .r \"other\") >>= (\\r -> if std.dict.defines ['wait] r then (.signal =>> .fail) else .r r.ok))",
                        &mut signal,
                    )
                })
            })?;
            // The worker's `.try` answered `wait`; its cut fails and parks.
            signal.recv().expect("the worker waits once");
            root.local_get("resolver")?;
            root.int_push(1);
            root.promise_fulfill()?;
            root.join(worker)?;
            root.take("worker", 1)?;
            root.text_pop()
        })
        .expect("the turn should succeed");
    assert_eq!(text, "kept");
}

#[test]
fn an_effect_reads_writes_and_puts_back_on_the_turn_machine_s_ports() {
    // `machines-communicate-through-wired-ports`: ports pass through to the
    // machine of the turn running the effect, as its own methods do.
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    let seen = rt
        .run("p", |p| {
            p.wire_add(
                glam::Endpoint::child("a", "out"),
                glam::Endpoint::child("b", "in"),
            )?;
            p.run("a", |a| {
                run(a, ".port.write 'out [1, 2]", &mut Standard)?;
                a.discard(1)
            })?;
            p.run("b", |b| {
                run(
                    b,
                    ".port.read 'in 1 >>= (\\x -> .port.unread 'in [0] =>> .port.available 'in >>= (\\n -> .port.pending >>= (\\pending -> .port.read 'in 2 >>= (\\rest -> .r (std.list.concat [x, [n, pending.in], rest])))))",
                    &mut Standard,
                )?;
                b.list_unpack(5)?;
                (0..5).map(|_| b.int_pop()).collect::<Result<Vec<_>, _>>()
            })
        })
        .expect("the turn should succeed");
    assert_eq!(
        seen,
        [2, 0, 2, 2, 1],
        "read, put back ahead of what was left, counted, read again"
    );
}

#[test]
fn an_effect_wires_and_asks_on_the_turn_machine_s_ports() {
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    let (listed, answer, delivered) = rt
        .run("p", |p| {
            run(
                p,
                ".wire.add (out:'o) (child:{key:'c, port:'i}) =>> .wire.defaults 'off =>> .wire.list >>= (\\w -> .wire.del (out:'o) (child:{key:'c, port:'i}) =>> .wire.list >>= (\\after -> .wire.defaults 'on =>> .r [std.list.len w.wires, std.list.len after.wires, w.defaults]))",
                &mut Standard,
            )?;
            p.list_unpack(3)?;
            let defaults = p.atom_pop()?;
            let after = p.int_pop()?;
            let listed = (p.int_pop()?, after, defaults);
            run(
                p,
                ".wire.add (out:'q) (in:'q) =>> .port.ask 'q 41 >>= (\\reply -> .port.read 'q 1 >>= (\\asked -> .promise.fulfill (std.list.head asked).reply ((std.list.head asked).request + 1) =>> std.seq reply (.r reply)))",
                &mut Standard,
            )?;
            let answer = p.int_pop()?;
            run(
                p,
                ".wire.add (out:'o) (child:{key:'c, port:'i}) =>> .port.write 'o [7]",
                &mut Standard,
            )?;
            p.discard(1)?;
            let delivered = p.run("c", |c| {
                c.port_read("i", 1)?;
                c.int_pop()
            })?;
            Ok((listed, answer, delivered))
        })
        .expect("the turn should succeed");
    assert_eq!(listed, (1, 0, "off".to_owned()), "listed, then unwired");
    assert_eq!(answer, 42, "a request travels with its reply's promise");
    assert_eq!(delivered, 7);
}
