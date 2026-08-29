//! Stable embedding-oriented facade for assembling modules and observing values.
//!
//! The child modules own staged construction, runtime coordination, diagnostics, and value
//! operations. Front-end syntax, core values, evaluator topology, and interaction-net scheduling
//! remain implementation details behind this re-export boundary.

mod assembly;
mod diagnostics;
mod error;
mod evaluator;
mod runtime;
mod value;

pub(crate) use assembly::CompilationExecution;
pub use assembly::{
    Assembler, AssemblerBuilder, BuiltModule, ModuleBuilder, ModuleInput, ReasoningVolume,
    ReflectionEnvironmentBuilder,
};
#[cfg(test)]
use assembly::{AssemblerReflectionHost, authoritative_reflection_environment};
pub use diagnostics::{
    Diagnostic, DiagnosticBus, DiagnosticCounts, DiagnosticEvent, DiagnosticIngress,
    DiagnosticSubscriber, DiagnosticSubscription,
};
pub use error::{Error, ReasoningFailure};
pub use evaluator::{ReflectionInspector, ValueEvaluator};
#[cfg(test)]
use runtime::publish_runtime_observation;
pub use runtime::*;
pub use value::{
    EffectTokenDomain, EvaluatedValue, NetBind, NetBuilder, NetCopy, NetPort, PromiseResolver,
    Value, ValueKind, Values,
};

#[cfg(test)]
use bytes::Bytes;
#[cfg(test)]
pub(crate) trait TestValueFacade {
    fn apply(
        &self,
        function: &Value,
        arguments: impl IntoIterator<Item = Value>,
    ) -> Result<Value, Error>;

    fn get(&self, root: &Value, path: &str) -> Result<Value, Error>;

    fn evaluate(&self, value: &Value) -> Result<Value, Error>;

    fn to_binary(&self, value: &Value) -> Result<Bytes, Error>;
}

#[cfg(test)]
impl TestValueFacade for Assembler {
    fn apply(
        &self,
        function: &Value,
        arguments: impl IntoIterator<Item = Value>,
    ) -> Result<Value, Error> {
        self.values().apply(function, arguments)
    }

    fn get(&self, root: &Value, path: &str) -> Result<Value, Error> {
        let value = self.values().access_names(root, path.split('.'))?;
        self.evaluator()
            .eval(&value)
            .map(EvaluatedValue::into_value)
    }

    fn evaluate(&self, value: &Value) -> Result<Value, Error> {
        self.evaluator().eval(value).map(EvaluatedValue::into_value)
    }

    fn to_binary(&self, value: &Value) -> Result<Bytes, Error> {
        let values = self.values();
        let binary = values.anno_binary(value.clone())?;
        let evaluated = self.evaluator().eval(&binary)?;
        evaluated
            .as_bytes()?
            .ok_or_else(|| Error::new("test value did not evaluate to binary data"))
    }
}

#[cfg(test)]
mod tests;
