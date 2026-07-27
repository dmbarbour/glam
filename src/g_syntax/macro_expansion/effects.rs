use crate::api::{Diagnostic, Value};
use crate::core::{Dict, Key, List, Value as CoreValue};
use crate::eval;
use crate::reflection::{
    EffectRequestSpec, RequestContext, RequestResult, TaskError, TaskSpecialization,
    get_value_path, parse_severity, prepare_message,
};

use super::host::{MacroHost, MacroJournal, MacroSnapshot};

const CASE_EXIT_TAG: [&str; 5] = ["macro_runtime", "g0", "request", "case", "exit"];

#[derive(Clone, Copy)]
pub(super) struct MacroEffects;

#[derive(Clone, Copy)]
pub(super) enum MacroRequest {
    Environment,
    Log,
    Case,
    CaseExit,
}

impl TaskSpecialization for MacroEffects {
    type Host = MacroHost;
    type Request = MacroRequest;
    type Snapshot = MacroSnapshot;
    type Journal = MacroJournal;

    fn exposes_shared_heap(&self) -> bool {
        false
    }

    fn requests(&self) -> Vec<EffectRequestSpec<Self::Request>> {
        vec![
            EffectRequestSpec::new(
                "env",
                ["reflection_runtime", "v0", "request", "env"],
                1,
                MacroRequest::Environment,
            ),
            EffectRequestSpec::new(
                "log",
                ["reflection_runtime", "v0", "request", "log"],
                2,
                MacroRequest::Log,
            ),
            EffectRequestSpec::new(
                "case",
                ["macro_runtime", "g0", "request", "case"],
                2,
                MacroRequest::Case,
            ),
            EffectRequestSpec::hidden(CASE_EXIT_TAG, 0, MacroRequest::CaseExit),
        ]
    }

    fn handle_request(
        &self,
        request: Self::Request,
        arguments: Vec<Value>,
        context: &mut RequestContext<'_, Self>,
    ) -> Result<RequestResult, TaskError> {
        match request {
            MacroRequest::Environment => environment(arguments, context),
            MacroRequest::Log => log(arguments, context),
            MacroRequest::Case => enter_case(arguments, context),
            MacroRequest::CaseExit => exit_case(arguments, context),
        }
    }
}

fn environment(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, MacroEffects>,
) -> Result<RequestResult, TaskError> {
    let [path]: [Value; 1] = arguments
        .try_into()
        .map_err(|_| TaskError::new("macro `.env` received the wrong number of arguments"))?;
    let path = eval::eval_key_path_list(context.eval_context(), path.as_core())
        .map_err(|error| TaskError::new(error.to_string()))?;
    let environment = context
        .transaction()
        .ok_or_else(|| TaskError::new("macro `.env` escaped its isolated transaction"))?
        .parts()
        .0
        .environment
        .as_core()
        .clone();
    let value = get_value_path(context.eval_context(), &environment, &path)?;
    Ok(RequestResult::Return(Value::from_core(value)))
}

fn log(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, MacroEffects>,
) -> Result<RequestResult, TaskError> {
    let [severity, message]: [Value; 2] = arguments
        .try_into()
        .map_err(|_| TaskError::new("macro `.log` received the wrong number of arguments"))?;
    let severity = parse_severity(context.eval_context(), severity)?;
    let message = prepare_message(context.eval_context(), message)?;
    let mut transaction = context
        .transaction()
        .ok_or_else(|| TaskError::new("macro `.log` escaped its isolated transaction"))?;
    transaction
        .parts()
        .1
        .push_diagnostic(Diagnostic::from_emission(severity, message));
    Ok(RequestResult::ReturnUnit)
}

fn enter_case(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, MacroEffects>,
) -> Result<RequestResult, TaskError> {
    let [explanation, parser]: [Value; 2] = arguments
        .try_into()
        .map_err(|_| TaskError::new("macro `.case` received the wrong number of arguments"))?;
    let mut transaction = context
        .transaction()
        .ok_or_else(|| TaskError::new("macro `.case` escaped its isolated transaction"))?;
    let (_, journal) = transaction.parts();
    journal.active_cases.push(explanation.clone());
    journal.visited_cases.push(explanation);
    Ok(RequestResult::Scoped {
        operation: parser,
        close: case_exit_effect(),
    })
}

fn exit_case(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, MacroEffects>,
) -> Result<RequestResult, TaskError> {
    let []: [Value; 0] = arguments
        .try_into()
        .map_err(|_| TaskError::new("internal macro case close received arguments"))?;
    let mut transaction = context
        .transaction()
        .ok_or_else(|| TaskError::new("macro case close escaped its isolated transaction"))?;
    transaction
        .parts()
        .1
        .active_cases
        .pop()
        .ok_or_else(|| TaskError::new("macro case stack became unbalanced"))?;
    Ok(RequestResult::ReturnUnit)
}

fn case_exit_effect() -> Value {
    let request = CoreValue::Dict(Dict::new_sync().insert(
        Key::abstract_global_path(CASE_EXIT_TAG),
        CoreValue::List(List::empty()),
    ));
    Value::from_core(eval::constant_effect(request))
}
