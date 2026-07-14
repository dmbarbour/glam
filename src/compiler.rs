use std::borrow::Cow;
use std::fmt;
use std::sync::Arc;

use crate::core::Builtin;
use crate::core::{
    Atom, DeferredValue, Dict, Expr as CoreExpr, Key, KeyExpr as CoreKeyExpr, Lambda, NetValue,
    Value,
};
use crate::core_net::CoreNetData;
use crate::interaction_net::{NetBuildError, NetBuilder, Node, Port};
use crate::number::Number;

pub type ModuleLoader = Arc<dyn Fn(ModuleLoadArgs) -> Result<Value, String> + Send + Sync>;
pub type BinaryFileLoader = Arc<dyn Fn(BinaryLoadArgs) -> Result<Value, String> + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompileNetError {
    Build(NetBuildError),
    OpenData,
}

impl fmt::Display for CompileNetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Build(error) => error.fmt(formatter),
            Self::OpenData => formatter.write_str(
                "closed interaction-net construction cannot embed lambda or capture placeholders",
            ),
        }
    }
}

impl std::error::Error for CompileNetError {}

impl From<NetBuildError> for CompileNetError {
    fn from(error: NetBuildError) -> Self {
        Self::Build(error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryLoadArgs {
    pub reference: Arc<str>,
    pub importer_source_path: Option<Arc<str>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleLoadArgs {
    pub reference: Arc<str>,
    pub importer_source_path: Option<Arc<str>>,
    pub module_path: Arc<[String]>,
    pub prior_defs: Value,
    pub final_defs: Value,
}

#[derive(Clone)]
pub struct CompileContext {
    // Ideally, we should actually abstract the effects API, i.e. such that
    // our front-end compilers don't even know what an `Expr` or `Value`
    // looks like under-the-hood. But for now, we use core data structures.
    source_path: Option<Arc<str>>,
    pub source_binary: Arc<[u8]>,
    pub module_path: Arc<[String]>,
    pub prior_defs: Value, // prior dictionary, can be observed at compile-time
    pub final_defs: Value, // future dictionary, cannot observe at compile-time
    local_module_loader: Option<ModuleLoader>,
    local_binary_loader: Option<BinaryFileLoader>,
}

impl Default for CompileContext {
    fn default() -> Self {
        Self {
            source_path: None,
            source_binary: Arc::from([]),
            module_path: Arc::from([]),
            prior_defs: Value::Dict(Dict::new_sync()), // empty prior dictionary
            final_defs: Value::expr(CoreExpr::Future(crate::core::IVar::new())), // future dictionary, assigned later
            local_module_loader: None,
            local_binary_loader: None,
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

    pub fn with_prior_defs(mut self, prior: Value) -> Self {
        self.prior_defs = prior;
        self
    }

    pub fn with_final_defs(mut self, final_defs: Value) -> Self {
        self.final_defs = final_defs;
        self
    }

    pub fn with_local_module_loader(mut self, loader: ModuleLoader) -> Self {
        self.local_module_loader = Some(loader);
        self
    }

    pub fn with_local_binary_loader(mut self, loader: BinaryFileLoader) -> Self {
        self.local_binary_loader = Some(loader);
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

    // TODO: methods to emit diagnostics with source context
    // diagnostic messages should be emitted as values at this layer

    // TODO: eliminate direct use of Builtin in this API. The front-end
    // knows about builtins, but will access them as atoms, not as the Builtin enum.

    pub fn key_expr_key(&self, key: Key) -> CoreKeyExpr {
        CoreKeyExpr::Key(key)
    }

    pub fn key_expr_index(&self, value: Value) -> CoreKeyExpr {
        CoreKeyExpr::Index(Arc::new(value_to_core_expr(value)))
    }

    pub fn key_expr_path_index(&self, value: Value) -> CoreKeyExpr {
        CoreKeyExpr::PathIndex(Arc::new(value_to_core_expr(value)))
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

    pub fn value_list(&self, items: Vec<Value>) -> Value {
        Value::expr(CoreExpr::List(Arc::from(
            items
                .into_iter()
                .map(value_to_core_expr)
                .map(Arc::new)
                .collect::<Vec<_>>(),
        )))
    }

    pub fn value_apply(&self, function: Value, argument: Value) -> Value {
        self.value_apply_many(function, [argument])
    }

    pub fn value_apply_many(
        &self,
        function: Value,
        arguments: impl IntoIterator<Item = Value>,
    ) -> Value {
        let arguments = arguments.into_iter().collect::<Vec<_>>();
        if arguments.is_empty() {
            return function;
        }
        let mut expr = value_to_core_expr(function);
        for argument in arguments {
            expr = CoreExpr::Apply(Arc::new(expr), Arc::new(value_to_core_expr(argument)));
        }
        Value::expr(expr)
    }

    /// Constructs one closed interaction-net value. The callback returns the
    /// net's sole exposed port; all other ports must be wired before it
    /// returns. The immutable template exists only long enough to instantiate
    /// the shared runtime.
    pub fn value_net(
        &self,
        build: impl FnOnce(&mut NetBuilder<CoreNetData>) -> Result<Port, NetBuildError>,
    ) -> Result<Value, CompileNetError> {
        let mut builder = NetBuilder::new();
        let exposed = build(&mut builder)?;
        let template = builder.try_finish(exposed)?;
        if template.nodes().iter().any(|node| {
            matches!(
                node,
                Node::Data(CoreNetData::Lambda(_) | CoreNetData::Capture(_))
            )
        }) {
            return Err(CompileNetError::OpenData);
        }
        let runtime = template.instantiate_shared();
        Ok(Value::Net(NetValue::new(runtime)))
    }

    pub fn value_lambda(&self, body: Value) -> Value {
        self.value_lambdas(1, body)
    }

    /// Constructs one curried function without preparing an intermediate net
    /// for every syntactic parameter. The semantic expression remains a
    /// lambda spine during the migration away from `CoreExpr::Lambda`, while
    /// interaction-net lowering treats that entire spine as one bind chain.
    pub fn value_lambdas(&self, arity: usize, body: Value) -> Value {
        assert!(arity > 0, "a lambda must bind at least one parameter");
        let mut expr = value_to_core_expr(body);
        for _ in 0..arity {
            expr = CoreExpr::Lambda(Arc::new(Lambda::new(Arc::new(expr))));
        }
        let CoreExpr::Lambda(lambda) = &expr else {
            unreachable!();
        };
        if closed_net_lambda_body(lambda.body()) {
            lambda.prepare_closed_net();
        }
        Value::expr(expr)
    }

    pub fn value_local(&self, index: usize) -> Value {
        Value::expr(CoreExpr::Local(index))
    }

    pub fn value_access(&self, base: Value, path: Vec<CoreKeyExpr>) -> Value {
        Value::expr(CoreExpr::Access(
            Arc::new(value_to_core_expr(base)),
            Arc::from(path),
        ))
    }

    pub fn value_lambda_body(&self, value: &Value) -> Option<Value> {
        let Value::Expr(thunk) = value else {
            return None;
        };
        let Some(env) = thunk.env() else {
            return None;
        };
        if !env.is_empty() {
            return None;
        }
        let CoreExpr::Lambda(body) = thunk.expr()?.as_ref() else {
            return None;
        };
        Some(Value::expr(body.body().as_ref().clone()))
    }

    pub fn builtin_apply2_value(&self, builtin: Builtin, left: Value, right: Value) -> Value {
        self.value_apply_many(self.value_builtin(builtin), [left, right])
    }

    pub fn builtin_apply3_value(
        &self,
        builtin: Builtin,
        first: Value,
        second: Value,
        third: Value,
    ) -> Value {
        self.value_apply_many(self.value_builtin(builtin), [first, second, third])
    }

    pub fn local_module_load_args(
        &self,
        reference: &str,
        module_path: Arc<[String]>,
        prior_defs: Value,
        final_defs: Value,
    ) -> ModuleLoadArgs {
        ModuleLoadArgs {
            reference: Arc::from(reference),
            importer_source_path: self.source_path.clone(),
            module_path,
            prior_defs,
            final_defs,
        }
    }

    pub fn value_load_local_module(&self, args: ModuleLoadArgs) -> Value {
        let label: Arc<str> = Arc::from(format!("import {}", args.reference));
        let loader = self.local_module_loader.clone();

        Value::expr(CoreExpr::Deferred(Arc::new(DeferredValue::new(
            label,
            move || {
                let Some(loader) = &loader else {
                    return Err(format!(
                        "local import `{}` cannot be loaded without a module loader",
                        args.reference
                    ));
                };
                loader(args.clone())
            },
        ))))
    }

    pub fn local_binary_load_args(&self, reference: &str) -> BinaryLoadArgs {
        BinaryLoadArgs {
            reference: Arc::from(reference),
            importer_source_path: self.source_path.clone(),
        }
    }

    pub fn value_load_local_binary(&self, args: BinaryLoadArgs) -> Value {
        let label: Arc<str> = Arc::from(format!("import binary {}", args.reference));
        let loader = self.local_binary_loader.clone();

        Value::expr(CoreExpr::Deferred(Arc::new(DeferredValue::new(
            label,
            move || {
                let Some(loader) = &loader else {
                    return Err(format!(
                        "binary import `{}` cannot be loaded without a binary loader",
                        args.reference
                    ));
                };
                loader(args.clone())
            },
        ))))
    }
}

fn value_to_core_expr(value: Value) -> CoreExpr {
    match value {
        Value::Expr(thunk) if thunk.env().is_some_and(|env| env.is_empty()) => {
            thunk.expr().unwrap().as_ref().clone()
        }
        value => CoreExpr::Value(value),
    }
}

fn closed_net_lambda_body(expr: &CoreExpr) -> bool {
    let mut arity = 1;
    let mut body = expr;
    while let CoreExpr::Lambda(lambda) = body {
        arity += 1;
        body = lambda.body();
    }
    closed_net_expr(body, arity)
}

fn closed_net_expr(expr: &CoreExpr, arity: usize) -> bool {
    match expr {
        CoreExpr::Value(value) => matches!(
            value,
            Value::Atom(_)
                | Value::Number(_)
                | Value::Binary(_)
                | Value::Builtin(_)
                | Value::Net(_)
        ),
        CoreExpr::Deferred(_) | CoreExpr::Future(_) | CoreExpr::Error(_) => true,
        CoreExpr::List(items) => items.iter().all(|item| closed_net_expr(item, arity)),
        CoreExpr::Apply(function, argument) => {
            closed_net_expr(function, arity) && closed_net_expr(argument, arity)
        }
        CoreExpr::Local(index) => *index < arity,
        CoreExpr::Lambda(_) | CoreExpr::Access(_, _) => false,
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

        assert_eq!(context.prior_defs, Value::Dict(Dict::new_sync()));
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

    #[test]
    fn compile_context_constructs_a_closed_net_value() {
        let context = CompileContext::default();
        let value = context
            .value_net(|builder| {
                let [application, argument, result] = builder.bind();
                builder.try_wire(argument, result)?;
                Ok(application)
            })
            .unwrap();

        let Value::Net(net) = value else {
            panic!("closed net construction should produce a net value");
        };
        assert_eq!(net.runtime().with(|runtime| runtime.exposed().index()), 1);
    }

    #[test]
    fn compile_context_reports_incomplete_net_construction() {
        let context = CompileContext::default();
        let error = context
            .value_net(|builder| {
                let [application, _, _] = builder.bind();
                Ok(application)
            })
            .unwrap_err();

        assert!(matches!(
            error,
            CompileNetError::Build(NetBuildError::PortUnwired(_))
        ));
    }

    #[test]
    fn compile_context_rejects_open_net_placeholders() {
        let context = CompileContext::default();
        let error = context
            .value_net(|builder| Ok(builder.data(CoreNetData::Capture(0))))
            .unwrap_err();

        assert_eq!(error, CompileNetError::OpenData);
    }

    #[test]
    fn compile_context_prepares_only_closed_leaf_lambdas_as_nets() {
        let context = CompileContext::default();
        let closed = context.value_lambda(context.value_local(0));
        let captured = context.value_lambda(context.value_local(1));

        let Value::Expr(closed) = closed else {
            panic!("lambda compiler term should remain inspectable during migration");
        };
        let CoreExpr::Lambda(closed) = closed.expr().unwrap().as_ref() else {
            panic!("lambda compiler term should retain its semantic wrapper");
        };
        let Value::Expr(captured) = captured else {
            panic!("captured lambda should remain a compatibility expression");
        };
        let CoreExpr::Lambda(captured) = captured.expr().unwrap().as_ref() else {
            panic!("captured lambda should retain its semantic wrapper");
        };

        assert!(closed.is_closed_lowered());
        assert!(!captured.is_closed_lowered());
    }

    #[test]
    fn compile_context_prepares_one_net_for_a_lambda_spine() {
        let context = CompileContext::default();
        let grouped = context.value_lambdas(3, context.value_local(2));

        let Value::Expr(grouped) = grouped else {
            panic!("lambda compiler term should remain inspectable during migration");
        };
        let CoreExpr::Lambda(outer) = grouped.expr().unwrap().as_ref() else {
            panic!("grouped lambda should retain its semantic wrapper");
        };
        let CoreExpr::Lambda(middle) = outer.body().as_ref() else {
            panic!("grouped lambda should retain its middle semantic wrapper");
        };
        let CoreExpr::Lambda(inner) = middle.body().as_ref() else {
            panic!("grouped lambda should retain its inner semantic wrapper");
        };

        assert!(outer.is_closed_lowered());
        assert!(!middle.is_closed_lowered());
        assert!(!inner.is_closed_lowered());

        let captured = context.value_lambdas(3, context.value_local(3));
        let Value::Expr(captured) = captured else {
            panic!("captured lambda should remain a compatibility expression");
        };
        let CoreExpr::Lambda(captured) = captured.expr().unwrap().as_ref() else {
            panic!("captured lambda should retain its semantic wrapper");
        };
        assert!(!captured.is_closed_lowered());
    }

    #[test]
    fn compile_context_builds_one_semantic_application_spine() {
        let context = CompileContext::default();
        let value = context.value_apply_many(
            context.value_builtin(Builtin::Add),
            [
                context.value_number(1.into()),
                context.value_number(2.into()),
            ],
        );

        let Value::Expr(value) = value else {
            panic!("application spine should remain a semantic expression");
        };
        let mut expr = value.expr().unwrap().as_ref();
        let mut arity = 0;
        while let CoreExpr::Apply(function, _) = expr {
            arity += 1;
            expr = function;
        }
        assert_eq!(arity, 2);
        assert!(matches!(
            expr,
            CoreExpr::Value(Value::Builtin(Builtin::Add))
        ));
    }
}
