use std::borrow::Cow;
use std::sync::Arc;

use crate::core::Builtin;
use crate::core::{Atom, Dict, Expr as CoreExpr, Key, KeyExpr as CoreKeyExpr, Value};
use crate::number::Number;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileContext {
    // Ideally, we should actually abstract the effects API, i.e. such that
    // our front-end compilers don't even know what an `Expr` or `Value`
    // looks like under-the-hood. But for now, we use core data structures directly.
    source_path: Option<Arc<str>>,
    pub source_binary: Arc<[u8]>,
    pub module_path: Arc<[String]>,
    pub prior: Value,
}

impl Default for CompileContext {
    fn default() -> Self {
        Self {
            source_path: None,
            source_binary: Arc::from([]),
            module_path: Arc::from([]),
            prior: Value::Dict(Dict::new_sync()),
        }
    }
}

impl CompileContext {
    pub fn from_source_path(path: &str) -> Self {
        Self::default().with_source_path(path)
    }

    pub fn for_assembly_file(path: &str) -> Self {
        Self::from_source_path(path).with_module_path(["assembly"])
    }

    pub fn from_module_path<I, S>(parts: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::default().with_module_path(parts)
    }

    pub fn with_source_path(mut self, path: impl Into<Arc<str>>) -> Self {
        self.source_path = Some(path.into());
        self
    }

    pub fn with_source_binary(mut self, bytes: impl Into<Arc<[u8]>>) -> Self {
        self.source_binary = bytes.into();
        self
    }

    pub fn with_module_path<I, S>(mut self, parts: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.module_path = Arc::from(
            parts
                .into_iter()
                .map(Into::into)
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        );
        self
    }

    // TODO: methods to emit diagnostics with source context
    // diagnostic messages should be expressed as values at this layer

    pub fn with_prior(mut self, prior: Value) -> Self {
        self.prior = prior;
        self
    }

    pub fn source_path(&self) -> Option<&str> {
        self.source_path.as_deref()
    }

    pub fn abstract_global_path(&self, target: &str) -> Arc<[String]> {
        // TODO: support expression-indexed paths, e.g. foo.bar.[42].baz
        let mut parts = self.module_path.iter().cloned().collect::<Vec<_>>();
        parts.extend(target.split('.').map(ToOwned::to_owned));
        Arc::from(parts.into_boxed_slice())
    }

    pub fn source_text<'a>(
        &'a self,
        fallback: &'a str,
    ) -> Result<Cow<'a, str>, std::str::Utf8Error> {
        if self.source_binary.is_empty() {
            return Ok(Cow::Borrowed(fallback));
        }

        std::str::from_utf8(self.source_binary.as_ref()).map(Cow::Borrowed)
    }

    pub fn expr_value(&self, value: Value) -> CoreExpr {
        CoreExpr::Value(value)
    }

    pub fn expr_list(&self, items: Vec<Arc<CoreExpr>>) -> CoreExpr {
        CoreExpr::List(Arc::from(items))
    }

    pub fn expr_apply(&self, function: CoreExpr, argument: CoreExpr) -> CoreExpr {
        CoreExpr::Apply(Arc::new(function), Arc::new(argument))
    }

    pub fn expr_lambda(&self, body: CoreExpr) -> CoreExpr {
        CoreExpr::Lambda(Arc::new(body))
    }

    pub fn expr_local(&self, index: usize) -> CoreExpr {
        CoreExpr::Local(index)
    }

    pub fn expr_access(&self, base: CoreExpr, path: Vec<CoreKeyExpr>) -> CoreExpr {
        CoreExpr::Access(Arc::new(base), Arc::from(path))
    }

    pub fn key_expr_key(&self, key: Key) -> CoreKeyExpr {
        CoreKeyExpr::Key(key)
    }

    pub fn key_expr_index(&self, expr: CoreExpr) -> CoreKeyExpr {
        CoreKeyExpr::Index(Arc::new(expr))
    }

    pub fn key_expr_path_index(&self, expr: CoreExpr) -> CoreKeyExpr {
        CoreKeyExpr::PathIndex(Arc::new(expr))
    }

    pub fn value_number(&self, number: Number) -> Value {
        Value::Number(number)
    }

    pub fn value_binary(&self, text: &str) -> Value {
        Value::binary_from_text(text)
    }

    pub fn value_atom(&self, atom: Atom) -> Value {
        Value::Atom(atom)
    }

    pub fn value_dict(&self, dict: Dict) -> Value {
        Value::Dict(dict)
    }

    pub fn value_builtin(&self, builtin: Builtin) -> Value {
        Value::Builtin(builtin)
    }

    pub fn value_expr(&self, expr: CoreExpr) -> Value {
        Value::expr(expr)
    }

    pub fn empty_dict_value(&self) -> Value {
        self.value_dict(Dict::new_sync())
    }

    pub fn abstract_global_path_value(&self, path: &[String]) -> Value {
        self.value_atom(Atom::from_key(&Key::AbstractGlobalPath(Arc::from(
            path.to_vec(),
        ))))
    }

    pub fn unit_value(&self) -> Value {
        self.value_atom(Atom::from_key(&Key::abstract_global_path([
            "builtin", "unit",
        ])))
    }

    pub fn dict_union_value(&self, left: &Value, right: &Value) -> Value {
        self.value_expr(self.builtin_apply2_expr(
            Builtin::DictUnion,
            self.expr_value(left.clone()),
            self.expr_value(right.clone()),
        ))
    }

    pub fn builtin_apply2_expr(
        &self,
        builtin: Builtin,
        left: CoreExpr,
        right: CoreExpr,
    ) -> CoreExpr {
        self.expr_apply(
            self.expr_apply(self.expr_value(self.value_builtin(builtin)), left),
            right,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assembly_file_context_uses_assembly_module_root() {
        let context = CompileContext::for_assembly_file("samples/assembly/hello_text.g");

        assert_eq!(context.source_path(), Some("samples/assembly/hello_text.g"));
        assert_eq!(context.module_path.as_ref(), &["assembly".to_owned()]);
    }

    #[test]
    fn compile_context_defaults_prior_to_empty_dict() {
        let context = CompileContext::default();

        assert_eq!(context.prior, Value::Dict(Dict::new_sync()));
    }

    #[test]
    fn unit_value_uses_abstract_global_path_atom() {
        let context = CompileContext::default();
        let unit = context.unit_value();
        let forged = Value::Atom(Atom::from_key(&Key::List(Arc::from([
            Key::binary_from_text("builtin"),
            Key::binary_from_text("unit"),
        ]))));

        assert_eq!(
            unit,
            context.abstract_global_path_value(&["builtin".to_owned(), "unit".to_owned()])
        );
        assert_ne!(unit, forged);
    }
}
