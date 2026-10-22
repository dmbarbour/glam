//! Regional state of the compile builtin (`compile-yields-an-open-mixin`):
//! its record converts to a key, so the compile reads it as data, never
//! evaluating it, and a host call then compiles the source on the thread
//! that forces it.

use std::sync::Arc;

use glam_gc::Visitor;

use crate::compiler::{CompileCall, SourceCompiler, SourceRecord};
use crate::core::{
    EvaluationFailure, HostCallRecord, Key, LazyId, LazyValue, Value,
    trace_compatibility_value_managed_edges,
};
use crate::evaluation::EvaluationValueAccess;

use super::access_machine::{RegionalConversionPoll, RegionalKeyConversion};
use super::builtin_machine::RegionalBuiltinPoll;

pub(in crate::eval) struct RegionalCompileMachine {
    compiler: Value,
    record_value: Value,
    replay: Value,
    base: Value,
    self_value: Value,
    record: RegionalKeyConversion,
}

impl RegionalCompileMachine {
    pub(in crate::eval) fn new_in(
        access: &EvaluationValueAccess<'_>,
        source_owner: LazyId,
        arguments: &[Value],
    ) -> Self {
        let [compiler, record, replay, base, self_value] = arguments else {
            unreachable!("compile retains five operands")
        };
        let duplicate = |value| access.values().duplicate_value(value);
        Self {
            compiler: duplicate(compiler),
            record_value: duplicate(record),
            replay: duplicate(replay),
            base: duplicate(base),
            self_value: duplicate(self_value),
            record: RegionalKeyConversion::new(access, duplicate(record), Some(source_owner)),
        }
    }

    pub(in crate::eval) fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> RegionalBuiltinPoll {
        match self.record.poll_in(access, step_budget) {
            RegionalConversionPoll::Ready(record) => match self.compile_in(access, &record) {
                Ok(compiled) => RegionalBuiltinPoll::Ready(compiled),
                Err(failure) => RegionalBuiltinPoll::Failed(failure),
            },
            RegionalConversionPoll::Boundary(request) => RegionalBuiltinPoll::Boundary(request),
            RegionalConversionPoll::Continue => RegionalBuiltinPoll::Continue,
            RegionalConversionPoll::BudgetExhausted => RegionalBuiltinPoll::BudgetExhausted,
            RegionalConversionPoll::Failed(failure) => RegionalBuiltinPoll::Failed(failure),
        }
    }

    /// The host call that compiles `record` for this application.
    fn compile_in(
        &self,
        access: &EvaluationValueAccess<'_>,
        record: &Key,
    ) -> Result<Value, Arc<EvaluationFailure>> {
        let failure = |message: String| Arc::new(EvaluationFailure::message(message));
        let record = SourceRecord::from_key(record).map_err(failure)?;
        let values = access.values();
        let compiler = match &self.compiler {
            Value::Opaque(compiler) => compiler.downcast::<SourceCompiler>(values.values()),
            _ => None,
        }
        .ok_or_else(|| failure("a compile takes the runtime's compiler".to_owned()))?;
        Ok(Value::Lazy(LazyValue::external_host_call_in(
            values,
            HostCallRecord::external_with_semantic_values(
                "source compilation",
                "src/eval/compile_machine.rs",
                "the runtime's compiler and the record's data plus the explicit arguments",
            ),
            [
                values.duplicate_value(&self.compiler),
                values.duplicate_value(&self.record_value),
                values.duplicate_value(&self.replay),
                values.duplicate_value(&self.base),
                values.duplicate_value(&self.self_value),
            ],
            move |captures| {
                let mut roots = captures.into_roots().into_vec().into_iter();
                let mut next = || roots.next().expect("a compilation receives its arguments");
                let call = CompileCall {
                    compiler: next(),
                    record: next(),
                    replay: next(),
                    base: next(),
                    self_value: next(),
                };
                (compiler.0)(record.clone(), call)
            },
        )))
    }

    pub(in crate::eval) fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        trace_compatibility_value_managed_edges(&self.compiler, visitor);
        trace_compatibility_value_managed_edges(&self.record_value, visitor);
        trace_compatibility_value_managed_edges(&self.replay, visitor);
        trace_compatibility_value_managed_edges(&self.base, visitor);
        trace_compatibility_value_managed_edges(&self.self_value, visitor);
        self.record.trace_managed_edges(visitor);
    }
}
