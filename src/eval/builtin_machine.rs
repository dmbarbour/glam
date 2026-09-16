//! Durable owners for saturated builtin computations.
//!
//! A builtin source roots its operands once, then delegates each semantic
//! demand to the ordinary WHNF owner. Family-specific immediate operations run
//! only after those demands complete and never retain raw values across polls.

use std::sync::Arc;

use crate::core::{Builtin, EvaluatedValue, EvaluationFailure, Value};
use crate::evaluation::{
    EvalContext, EvaluationPollContext, EvaluatorStepContext, WhnfOwnerPoll, WorkDependency,
    poll_whnf_computation,
};
use crate::number::Number;
use crate::runtime::{RuntimeFailureRoot, RuntimeValueRoot};

use super::value::number_from_evaluated;
use super::whnf::WhnfComputation;

pub(crate) enum BuiltinTaskPoll {
    Ready(RuntimeValueRoot),
    Pending(WorkDependency),
    Yielded,
    Failed(RuntimeFailureRoot),
}

pub(crate) enum BuiltinTaskMachine {
    Numeric(NumericBuiltinMachine),
}

impl BuiltinTaskMachine {
    pub(crate) fn supports(builtin: Builtin) -> bool {
        matches!(
            builtin,
            Builtin::Add
                | Builtin::Subtract
                | Builtin::Multiply
                | Builtin::Divide
                | Builtin::Floor
                | Builtin::Mod
        )
    }

    pub(crate) fn new(builtin: Builtin, arguments: Vec<RuntimeValueRoot>) -> Self {
        assert!(Self::supports(builtin));
        assert_eq!(
            arguments.len(),
            builtin.arity(),
            "a builtin source must contain one saturated call"
        );
        Self::Numeric(NumericBuiltinMachine::new(builtin, arguments))
    }

    pub(crate) fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> BuiltinTaskPoll {
        match self {
            Self::Numeric(machine) => {
                machine.poll(poll_context, context, durable_context, step_budget)
            }
        }
    }
}

pub(crate) struct NumericBuiltinMachine {
    builtin: Builtin,
    arguments: Vec<RuntimeValueRoot>,
    next: usize,
    demand: Option<WhnfComputation>,
    numbers: Vec<Number>,
}

impl NumericBuiltinMachine {
    fn new(builtin: Builtin, arguments: Vec<RuntimeValueRoot>) -> Self {
        let argument_count = arguments.len();
        Self {
            builtin,
            arguments,
            next: 0,
            demand: None,
            numbers: Vec::with_capacity(argument_count),
        }
    }

    fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> BuiltinTaskPoll {
        while self.next < self.arguments.len() {
            let demand = self.demand.get_or_insert_with(|| {
                WhnfComputation::from_root(self.arguments[self.next].clone())
            });
            let value =
                match poll_whnf_computation(demand, poll_context, durable_context, step_budget) {
                    WhnfOwnerPoll::Ready(value) => value,
                    WhnfOwnerPoll::Pending(dependency) => {
                        return BuiltinTaskPoll::Pending(dependency);
                    }
                    WhnfOwnerPoll::Yielded => return BuiltinTaskPoll::Yielded,
                    WhnfOwnerPoll::Failed(failure) => return BuiltinTaskPoll::Failed(failure),
                    WhnfOwnerPoll::External(boundary) => {
                        unreachable!("numeric demand produced an external {boundary:?} boundary")
                    }
                };
            let value = EvaluatedValue::try_from(context.project_root(&value))
                .expect("numeric operand demand must reach WHNF");
            let number = match number_from_evaluated(value, numeric_name(self.builtin)) {
                Ok(number) => number,
                Err(error) => {
                    return BuiltinTaskPoll::Failed(
                        context.root_failure(error.into_permanent_failure()),
                    );
                }
            };
            self.numbers.push(number);
            self.next += 1;
            self.demand = None;
        }

        match numeric_result(self.builtin, &self.numbers) {
            Ok(result) => BuiltinTaskPoll::Ready(context.root_value(Value::Number(result))),
            Err(message) => BuiltinTaskPoll::Failed(
                context.root_failure(Arc::new(EvaluationFailure::message(message))),
            ),
        }
    }
}

fn numeric_name(builtin: Builtin) -> &'static str {
    match builtin {
        Builtin::Add => "add",
        Builtin::Subtract => "subtract",
        Builtin::Multiply => "multiply",
        Builtin::Divide => "divide",
        Builtin::Floor => "floor",
        Builtin::Mod => "mod",
        _ => unreachable!("numeric name requested for another builtin"),
    }
}

fn numeric_result(builtin: Builtin, arguments: &[Number]) -> Result<Number, &'static str> {
    match builtin {
        Builtin::Add => Ok(arguments[0].add(&arguments[1])),
        Builtin::Subtract => Ok(arguments[0].sub(&arguments[1])),
        Builtin::Multiply => Ok(arguments[0].mul(&arguments[1])),
        Builtin::Divide => arguments[0]
            .checked_div(&arguments[1])
            .ok_or("divide builtin cannot divide by zero"),
        Builtin::Floor => Ok(arguments[0].floor()),
        Builtin::Mod => arguments[0]
            .checked_mod(&arguments[1])
            .ok_or("mod builtin cannot divide by zero"),
        _ => unreachable!("numeric result requested for another builtin"),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::core::{CoreValueFactory, PromisedValue};
    use crate::evaluation::EvalContext;
    use crate::runtime::{RuntimeIds, allocate_evaluation_runtime_id};

    fn context() -> crate::evaluation::OwnedEvalContext {
        EvalContext::isolated(CoreValueFactory::new(
            allocate_evaluation_runtime_id(),
            RuntimeIds::new(),
        ))
    }

    #[test]
    fn numeric_machine_resumes_at_the_second_operand_without_replaying_the_first() {
        let context = context();
        let first_demands = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&first_demands);
        let first = Value::semantic_thunk(context.values(), "first numeric operand", move |_| {
            observed.fetch_add(1, Ordering::Relaxed);
            Ok(Value::Number(1.into()))
        });
        let second = PromisedValue::new(context.values(), "second numeric operand");
        let sum = Value::builtin_call(
            context.values(),
            Builtin::Add,
            vec![first, Value::Promised(second.clone())],
        );

        let blocked = crate::eval::eval_value(&context, &sum)
            .expect_err("the second operand must remain an exact suspension boundary");
        assert!(blocked.unassigned_promise_root().is_some() || blocked.blocked_on().is_some());
        assert_eq!(first_demands.load(Ordering::Relaxed), 1);

        crate::core::set_test_promise(context.values(), &second, Value::Number(2.into()))
            .expect("the second operand should accept its assignment");
        assert_eq!(
            crate::eval::eval_value(&context, &sum).expect("numeric work must resume"),
            Value::Number(3.into())
        );
        assert_eq!(first_demands.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn numeric_machine_retains_numeric_failure_contracts() {
        let context = context();
        let divide = Value::builtin_call(
            context.values(),
            Builtin::Divide,
            vec![Value::Number(1.into()), Value::Number(0.into())],
        );
        assert_eq!(
            crate::eval::eval_value(&context, &divide)
                .expect_err("division by zero must remain a permanent failure")
                .to_string(),
            "divide builtin cannot divide by zero"
        );

        let invalid = Value::builtin_call(
            context.values(),
            Builtin::Floor,
            vec![Value::binary_from_text("not a number")],
        );
        assert_eq!(
            crate::eval::eval_value(&context, &invalid)
                .expect_err("numeric kind validation must remain a permanent failure")
                .to_string(),
            "floor builtin requires number values"
        );
    }
}
