use std::fmt;

use super::{Assembler, Error, EvaluatedValue, Value, ValueKind};
use crate::core::Value as CoreValue;

/// Privileged, runtime-specific observation in one assembler session.
///
/// These operations are outside assembly reproducibility and are not a stable
/// semantic surface. Each method documents whether its own protocol demands a
/// value; ordinary WHNF evaluation and list materialization instead belong to
/// [`ValueEvaluator`] and [`super::Values`].
#[derive(Clone, Copy)]
pub struct ReflectionInspector<'a> {
    pub(super) assembler: &'a Assembler,
}

/// Deterministic semantic demand through one assembler's client session.
///
/// Successful evaluation reaches only outer weak-head normal form. Nested
/// values remain lazy and are represented by [`EvaluatedValue`] handles.
#[derive(Clone, Copy)]
pub struct ValueEvaluator<'a> {
    pub(super) assembler: &'a Assembler,
}

impl ValueEvaluator<'_> {
    /// Demands `value` to outer weak-head normal form.
    pub fn eval(&self, value: &Value) -> Result<EvaluatedValue, Error> {
        let values = self.assembler.values();
        let value = values.clone_core(value)?;
        self.assembler
            .eval_context()
            .evaluate_whnf(&value)
            .map(|value| EvaluatedValue::from_whnf(&values, values.wrap(value)))
            .map_err(|error| self.assembler.evaluation_error(error))
    }
}

impl ReflectionInspector<'_> {
    /// Compares the retained runtime representations without demanding either
    /// value.
    ///
    /// This is a reflection operation rather than Glam's logical equality:
    /// lazy and promised values compare by their current identities, while
    /// immediate containers compare their currently retained structure.
    pub fn same_representation(&self, left: &Value, right: &Value) -> Result<bool, Error> {
        self.assembler
            .values()
            .with_access(|values| Ok(values.core_value(left)? == values.core_value(right)?))
    }

    /// Reports the current outer runtime representation without demanding it.
    pub fn kind(&self, value: &Value) -> Result<ValueKind, Error> {
        self.assembler
            .values()
            .with_access(|values| Ok(ValueKind::from_core(values.core_value(value)?)))
    }

    /// Returns a sealed carrier's associated metadata without evaluating it.
    ///
    /// The supplied value is evaluated only far enough to recognize its outer
    /// kind. Ordinary values return `None`; a failure while reaching that kind
    /// remains an evaluation error rather than a metadata mismatch.
    pub fn associated_metadata(&self, value: &Value) -> Result<Option<Value>, Error> {
        let values = self.assembler.values();
        let value = values.clone_core(value)?;
        let value = self
            .assembler
            .eval_context()
            .evaluate_whnf(&value)
            .map_err(|error| self.assembler.evaluation_error(error))?;
        Ok(value.associated_metadata().map(|value| values.wrap(value)))
    }

    /// Returns dictionary entries in canonical key order without evaluating
    /// their values. Keys are reified as ordinary keyable [`Value`]s.
    pub fn dictionary_items(&self, value: &Value) -> Result<Vec<(Value, Value)>, Error> {
        let values = self.assembler.values();
        let value = values.clone_core(value)?;
        let value = self
            .assembler
            .eval_context()
            .evaluate_whnf(&value)
            .map_err(|error| self.assembler.evaluation_error(error))?;
        let CoreValue::Dict(dict) = value else {
            return Err(Error::new(format!(
                "reflection dictionary inspection requires a dictionary, received {}",
                value.diagnostic_kind_name()
            )));
        };
        Ok(dict
            .iter()
            .map(|(key, value)| {
                (
                    values.wrap(key.to_value_with(values.core())),
                    values.wrap(value.clone()),
                )
            })
            .collect())
    }

    /// Returns the key value that gives an atom its identity.
    pub fn atom_key(&self, value: &Value) -> Result<Value, Error> {
        let values = self.assembler.values();
        let value = values.clone_core(value)?;
        let value = self
            .assembler
            .eval_context()
            .evaluate_whnf(&value)
            .map_err(|error| self.assembler.evaluation_error(error))?;
        let CoreValue::Atom(atom) = value else {
            return Err(Error::new(format!(
                "reflection atom inspection requires an atom, received {}",
                value.diagnostic_kind_name()
            )));
        };
        Ok(values.wrap(atom.key().to_value_with(values.core())))
    }
}

impl fmt::Debug for ReflectionInspector<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReflectionInspector")
            .finish_non_exhaustive()
    }
}
