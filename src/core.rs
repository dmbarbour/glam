use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use bytes::Bytes;
use internment::Intern;
use rpds::RedBlackTreeMapSync;

use crate::core_net::{CoreDataKey, CoreRuntimeNet};
use crate::number::Number;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    Value(Value),
    List(Arc<[Arc<Expr>]>),
    Apply(Arc<Expr>, Arc<Expr>),
    /// Constructs an ordinary function value from once-lowered shared code and
    /// the current values of its lifted captures.
    Function {
        code: Arc<FunctionCode>,
        captures: Arc<[Arc<Expr>]>,
    },
    Local(usize),
    Access(Arc<Expr>, Arc<[KeyExpr]>),
}

#[derive(Clone)]
pub struct LazyValue {
    id: u64,
    label: Arc<str>,
    source: LazySource,
    result: Arc<OnceLock<Result<Value, Arc<str>>>>,
}

impl LazyValue {
    fn with_source(label: impl Into<Arc<str>>, source: LazySource) -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);

        Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            label: label.into(),
            source,
            result: Arc::new(OnceLock::new()),
        }
    }

    pub fn pending(label: impl Into<Arc<str>>) -> Self {
        Self::with_source(label, LazySource::Pending)
    }

    pub fn deferred(
        label: impl Into<Arc<str>>,
        thunk: impl Fn() -> Result<Value, String> + Send + Sync + 'static,
    ) -> Self {
        Self::with_source(label, LazySource::Deferred(Arc::new(thunk)))
    }

    pub fn error(message: impl Into<Arc<str>>) -> Self {
        let value = Self::with_source("error", LazySource::Pending);
        value
            .result
            .set(Err(message.into()))
            .expect("new lazy error must be uninitialized");
        value
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn get(&self) -> Option<Result<Value, Arc<str>>> {
        self.result.get().cloned()
    }

    pub fn set(&self, value: Value) -> Result<(), Value> {
        self.result.set(Ok(value)).map_err(|result| {
            result.expect("setting a lazy value always supplies a successful value")
        })
    }

    pub fn cached(&self) -> Option<Result<Value, Arc<str>>> {
        self.get()
    }

    pub fn cache(&self, value: Result<Value, Arc<str>>) -> Result<Value, Arc<str>> {
        let _ = self.result.set(value);
        self.result
            .get()
            .expect("lazy cache should contain a value after set")
            .clone()
    }

    pub(crate) fn is_pending(&self) -> bool {
        matches!(self.source, LazySource::Pending) && self.result.get().is_none()
    }

    pub(crate) fn force_deferred(&self) -> Option<Result<Value, Arc<str>>> {
        match &self.source {
            LazySource::Deferred(thunk) => Some(
                self.result
                    .get_or_init(|| thunk().map_err(Arc::from))
                    .clone(),
            ),
            _ => None,
        }
    }
}

impl PartialEq for LazyValue {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for LazyValue {}

impl fmt::Debug for LazyValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LazyValue")
            .field("id", &self.id)
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyExpr {
    Key(Key),
    Index(Arc<Expr>),
    PathIndex(Arc<Expr>),
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Atom {
    // Atom is optimized tagged data `[Key]:()`
    // use Intern for fast comparison and hash
    key: Intern<Key>,
}

impl Atom {
    pub fn from_key(key: &Key) -> Self {
        Self {
            key: Intern::new(key.clone()),
        }
    }

    pub fn key(&self) -> &Key {
        self.key.as_ref()
    }
}

impl fmt::Debug for Atom {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Atom").field(self.key()).finish()
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Key {
    Atom(Atom),
    Number(Number),
    Binary(Bytes),
    AbstractGlobalPath(Arc<[String]>),
    List(Arc<[Key]>),
    Dict(Arc<[(Key, Key)]>),
}

impl Key {
    pub fn atom_from_text(text: impl AsRef<str>) -> Self {
        Self::atom_from_key(&Self::binary_from_text(text))
    }

    pub fn atom_from_key(key: &Key) -> Self {
        Self::Atom(Atom::from_key(key))
    }

    pub fn binary_from_text(text: impl AsRef<str>) -> Self {
        Self::Binary(Bytes::copy_from_slice(text.as_ref().as_bytes()))
    }

    pub fn abstract_global_path<I, S>(parts: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::AbstractGlobalPath(Arc::from(
            parts.into_iter().map(Into::into).collect::<Vec<_>>(),
        ))
    }

    pub fn from_value(value: &Value) -> Option<Self> {
        match value {
            Value::Atom(atom) => Some(Self::Atom(*atom)),
            Value::Number(number) => Some(Self::Number(number.clone())),
            Value::Binary(bytes) => Some(Self::Binary(bytes.clone())),
            Value::List(list) => Some(Self::List(list_to_key_items(list)?)),
            Value::Dict(dict) => Some(Self::Dict(Arc::from(
                dict.iter()
                    .map(|(key, value)| {
                        let value = Self::from_value(value)?;
                        if matches!(&value, Key::Dict(entries) if entries.is_empty()) {
                            return Some(None);
                        }
                        Some(Some((key.clone(), value)))
                    })
                    .collect::<Option<Vec<_>>>()?
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>(),
            ))),
            Value::Builtin(_) | Value::PartialBuiltin(_) | Value::Function(_) | Value::Net(_) => {
                None
            }
            Value::Lazy(_) => None,
        }
    }
}

impl fmt::Debug for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Key::Atom(atom) => f.debug_tuple("Atom").field(atom).finish(),
            Key::Number(number) => f.debug_tuple("Number").field(number).finish(),
            Key::Binary(bytes) => f.debug_tuple("Binary").field(bytes).finish(),
            Key::AbstractGlobalPath(parts) => {
                f.debug_tuple("AbstractGlobalPath").field(parts).finish()
            }
            Key::List(items) => f.debug_tuple("List").field(items).finish(),
            Key::Dict(entries) => f.debug_tuple("Dict").field(entries).finish(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Atom(Atom),
    Number(Number),
    Binary(Bytes),
    List(List),
    Dict(Dict),
    Builtin(Builtin),
    PartialBuiltin(BuiltinCall),
    /// An ordinary observable function value backed by a shared curried net
    /// stage. Unlike `Net`, this never exposes structural binders as values.
    Function(FunctionValue),
    /// A closed interaction net with one designated exposed port.
    Net(NetValue),
    /// A closed suspended computation, pending value, or memoized failure.
    Lazy(LazyValue),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetValue {
    runtime: CoreRuntimeNet,
}

impl NetValue {
    pub fn new(runtime: CoreRuntimeNet) -> Self {
        Self { runtime }
    }

    pub fn runtime(&self) -> &CoreRuntimeNet {
        &self.runtime
    }

    pub fn into_runtime(self) -> CoreRuntimeNet {
        self.runtime
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionCode {
    runtime: CoreRuntimeNet,
    arity: usize,
    capture_count: usize,
}

impl FunctionCode {
    pub(crate) fn new(runtime: CoreRuntimeNet, arity: usize, capture_count: usize) -> Self {
        Self {
            runtime,
            arity,
            capture_count,
        }
    }

    pub(crate) fn runtime(&self) -> &CoreRuntimeNet {
        &self.runtime
    }

    pub fn arity(&self) -> usize {
        self.arity
    }

    pub fn capture_count(&self) -> usize {
        self.capture_count
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionValue {
    stage: NetValue,
    remaining_arity: usize,
}

impl FunctionValue {
    pub(crate) fn new(stage: NetValue, remaining_arity: usize) -> Self {
        assert!(
            remaining_arity > 0,
            "a function stage must accept an argument"
        );
        Self {
            stage,
            remaining_arity,
        }
    }

    pub(crate) fn stage(&self) -> &NetValue {
        &self.stage
    }

    pub fn remaining_arity(&self) -> usize {
        self.remaining_arity
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltinCall {
    pub builtin: Builtin,
    pub arguments: Arc<[Value]>,
}

impl BuiltinCall {
    pub fn new(builtin: Builtin) -> Self {
        Self {
            builtin,
            arguments: Arc::from([]),
        }
    }
}

#[derive(Clone)]
enum LazySource {
    Pending,
    Deferred(Arc<dyn Fn() -> Result<Value, String> + Send + Sync>),
    Expr {
        expr: Arc<Expr>,
        env: Arc<[Value]>,
    },
    Access {
        path: Arc<[CoreDataKey]>,
        arguments: Arc<[Value]>,
    },
    Builtin(BuiltinCall),
    NetComputation(NetValue),
    FunctionCall {
        function: FunctionValue,
        arguments: Arc<[Value]>,
    },
}

impl LazyValue {
    pub fn new(expr: Arc<Expr>, env: Arc<[Value]>) -> Self {
        Self::with_source("expression", LazySource::Expr { expr, env })
    }

    pub(crate) fn from_access(path: Arc<[CoreDataKey]>, arguments: Arc<[Value]>) -> Self {
        Self::with_source("access", LazySource::Access { path, arguments })
    }

    pub(crate) fn from_builtin(call: BuiltinCall) -> Self {
        Self::with_source("builtin call", LazySource::Builtin(call))
    }

    pub(crate) fn from_function_call(function: FunctionValue, arguments: Arc<[Value]>) -> Self {
        Self::with_source(
            "function call",
            LazySource::FunctionCall {
                function,
                arguments,
            },
        )
    }

    pub(crate) fn from_net_computation(net: NetValue) -> Self {
        Self::with_source("net computation", LazySource::NetComputation(net))
    }

    pub fn expr(&self) -> Option<&Arc<Expr>> {
        match &self.source {
            LazySource::Expr { expr, .. } => Some(expr),
            _ => None,
        }
    }

    pub fn env(&self) -> Option<&Arc<[Value]>> {
        match &self.source {
            LazySource::Expr { env, .. } => Some(env),
            _ => None,
        }
    }

    pub(crate) fn access(&self) -> Option<(&[CoreDataKey], &[Value])> {
        match &self.source {
            LazySource::Access { path, arguments } => Some((path, arguments)),
            _ => None,
        }
    }

    pub(crate) fn builtin(&self) -> Option<&BuiltinCall> {
        match &self.source {
            LazySource::Builtin(call) => Some(call),
            _ => None,
        }
    }

    pub(crate) fn function_call(&self) -> Option<(&FunctionValue, &[Value])> {
        match &self.source {
            LazySource::FunctionCall {
                function,
                arguments,
            } => Some((function, arguments)),
            _ => None,
        }
    }

    pub(crate) fn net_computation(&self) -> Option<&NetValue> {
        match &self.source {
            LazySource::NetComputation(net) => Some(net),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Builtin {
    Append,
    Add,
    Subtract,
    Multiply,
    Divide,
    Greater,
    GreaterEqual,
    Equal,
    NotEqual,
    LessEqual,
    Less,
    Fixpoint,
    Anno,
    MergeDuplicate,
    Floor,
    Mod,
    Slice,
    Map,
    ListLen,
    ListSplit,
    ListSplitEnd,
    ListHead,
    ListTail,
    ListEffect,
    ListEffectReturn,
    ListEffectSeq,
    ListEffectAlt,
    ListEffectCut,
    ListEffectFix,
    DictSingleton,
    DictUnion,
    DictUpdate,
    ObjectSpec,
    ObjectLocalName,
    ObjectInstanceFromParts,
    ObjectInstance,
}

impl Builtin {
    pub fn arity(self) -> usize {
        match self {
            Self::Append => 2,
            Self::Add => 2,
            Self::Subtract => 2,
            Self::Multiply => 2,
            Self::Divide => 2,
            Self::Greater => 2,
            Self::GreaterEqual => 2,
            Self::Equal => 2,
            Self::NotEqual => 2,
            Self::LessEqual => 2,
            Self::Less => 2,
            Self::Fixpoint => 1,
            Self::Anno => 2,
            Self::MergeDuplicate => 3,
            Self::Floor => 1,
            Self::Mod => 2,
            Self::Slice => 3,
            Self::Map => 2,
            Self::ListLen => 1,
            Self::ListSplit => 2,
            Self::ListSplitEnd => 2,
            Self::ListHead => 1,
            Self::ListTail => 1,
            Self::ListEffect => 1,
            Self::ListEffectReturn => 1,
            Self::ListEffectSeq => 2,
            Self::ListEffectAlt => 2,
            Self::ListEffectCut => 1,
            Self::ListEffectFix => 1,
            Self::DictSingleton => 2,
            Self::DictUnion => 2,
            Self::DictUpdate => 3,
            Self::ObjectSpec => 1,
            Self::ObjectLocalName => 2,
            Self::ObjectInstanceFromParts => 3,
            Self::ObjectInstance => 1,
        }
    }
}

pub type Dict = RedBlackTreeMapSync<Key, Value>;

pub type List = crate::list::List<Value, LazyValue>;

fn list_to_key_items(list: &List) -> Option<Arc<[Key]>> {
    let items = std::cell::RefCell::new(Vec::new());
    list.for_each_segment(
        &mut |bytes| {
            items
                .borrow_mut()
                .extend(bytes.iter().map(|byte| Key::Number(Number::from_u8(*byte))));
            Ok::<_, ()>(())
        },
        &mut |values| {
            for value in values {
                items.borrow_mut().push(Key::from_value(value).ok_or(())?);
            }
            Ok(())
        },
    )
    .ok()?;
    Some(Arc::from(items.into_inner()))
}

impl Value {
    pub fn binary_from_text(text: &str) -> Self {
        Self::Binary(Bytes::copy_from_slice(text.as_bytes()))
    }

    pub fn expr(expr: Expr) -> Self {
        Self::Lazy(LazyValue::new(Arc::new(expr), Arc::from([])))
    }

    pub fn expr_arc(expr: Arc<Expr>) -> Self {
        Self::Lazy(LazyValue::new(expr, Arc::from([])))
    }

    pub fn deferred(
        label: impl Into<Arc<str>>,
        thunk: impl Fn() -> Result<Value, String> + Send + Sync + 'static,
    ) -> Self {
        Self::Lazy(LazyValue::deferred(label, thunk))
    }

    pub fn error(message: impl Into<Arc<str>>) -> Self {
        Self::Lazy(LazyValue::error(message))
    }

    pub fn singleton_list(value: Value) -> List {
        List::from_values(vec![value])
    }
}

impl Value {
    pub fn get_key_path(&self, path: &[Key]) -> Option<&Value> {
        match path {
            [] => Some(self),
            [head, rest @ ..] => match self {
                Value::Dict(dict) => dict.get(head)?.get_key_path(rest),
                Value::Atom(_)
                | Value::Number(_)
                | Value::Binary(_)
                | Value::List(_)
                | Value::Function(_)
                | Value::Net(_)
                | Value::Builtin(_)
                | Value::PartialBuiltin(_)
                | Value::Lazy(_) => None,
            },
        }
    }

    pub fn get_atom_path(&self, path: &[Atom]) -> Option<&Value> {
        let path = path.iter().cloned().map(Key::Atom).collect::<Vec<_>>();
        self.get_key_path(&path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    #[test]
    fn atoms_and_binary_keys_are_distinct() {
        let asm = Atom::from_key(&Key::binary_from_text("asm"));
        let dict = Dict::new_sync()
            .insert(Key::Atom(asm.clone()), Value::binary_from_text("atom"))
            .insert(
                Key::binary_from_text("asm"),
                Value::binary_from_text("binary"),
            );
        let value = Value::Dict(dict);

        assert_eq!(
            value.get_atom_path(&[asm]),
            Some(&Value::binary_from_text("atom"))
        );
    }

    #[test]
    fn atom_keys_are_canonical_by_key() {
        assert_eq!(
            Atom::from_key(&Key::binary_from_text("asm")),
            Atom::from_key(&Key::binary_from_text("asm"))
        );
        assert_eq!(
            Atom::from_key(&Key::binary_from_text("asm")).key(),
            &Key::binary_from_text("asm")
        );
    }

    #[test]
    fn atom_keys_from_equal_keys_are_canonical() {
        let binary_key = Key::binary_from_text("tag");
        let atom_key_1 = Key::atom_from_key(&binary_key);
        let atom_key_2 = Key::atom_from_key(&Key::binary_from_text("tag"));

        assert!(matches!(atom_key_1, Key::Atom(_)));
        assert_eq!(atom_key_1, atom_key_2);
        assert_ne!(atom_key_1, binary_key);
    }

    #[test]
    fn values_can_store_lists_and_numbers() {
        let value = Value::List(List::from_values(vec![
            Value::Number(1.into()),
            Value::Number(2.into()),
            Value::Number(3.into()),
        ]));

        assert!(matches!(value, Value::List(_)));
    }

    #[test]
    fn semantic_expr_can_hold_literal_values_builtins_and_application() {
        let expr = Expr::Apply(
            Arc::new(Expr::Apply(
                Arc::new(Expr::Value(Value::Builtin(Builtin::Append))),
                Arc::new(Expr::Value(Value::List(List::from_values(vec![
                    Value::Number(1.into()),
                ])))),
            )),
            Arc::new(Expr::Value(Value::List(List::from_values(vec![
                Value::Number(2.into()),
            ])))),
        );

        assert!(matches!(expr, Expr::Apply(_, _)));
    }

    #[test]
    fn semantic_expr_can_hold_lists() {
        let expr = Expr::List(Arc::from([
            Arc::new(Expr::Value(Value::Number(1.into()))),
            Arc::new(Expr::Value(Value::Number(2.into()))),
        ]));

        assert!(matches!(expr, Expr::List(items) if items.len() == 2));
    }

    #[test]
    fn semantic_expr_can_hold_accesses() {
        let expr = Expr::Access(
            Arc::new(Expr::Local(0)),
            Arc::from([KeyExpr::Key(Key::atom_from_text("hello"))]),
        );

        assert!(matches!(expr, Expr::Access(_, path) if path.len() == 1));
    }

    #[test]
    fn semantic_values_can_hold_atoms() {
        let value = Value::Atom(Atom::from_key(&Key::binary_from_text("greeting")));

        assert!(matches!(value, Value::Atom(_)));
    }

    #[test]
    fn semantic_values_can_hold_lazy_errors() {
        let value = Value::error("ambiguous key");

        assert!(
            matches!(value, Value::Lazy(lazy) if lazy.cached().is_some_and(|value| value.is_err()))
        );
    }

    #[test]
    fn keys_can_represent_nested_value_data() {
        let value = Value::Dict(Dict::new_sync().insert(
            Key::atom_from_text("payload"),
            Value::List(List::concat(
                List::from_values(vec![Value::Number(1.into())]),
                List::from_bytes(Bytes::from_static(b"Hi")),
            )),
        ));

        assert_eq!(
            Key::from_value(&value),
            Some(Key::Dict(Arc::from([(
                Key::atom_from_text("payload"),
                Key::List(Arc::from([
                    Key::Number(1.into()),
                    Key::Number(Number::from_u8(b'H')),
                    Key::Number(Number::from_u8(b'i')),
                ])),
            )])))
        );
    }

    #[test]
    fn empty_dict_values_are_elided_from_dict_keys() {
        let empty = Value::Dict(Dict::new_sync());
        let with_empty_field = Value::Dict(
            Dict::new_sync().insert(Key::atom_from_text("key"), Value::Dict(Dict::new_sync())),
        );

        assert_eq!(Key::from_value(&empty), Some(Key::Dict(Arc::from([]))));
        assert_eq!(
            Key::from_value(&with_empty_field),
            Some(Key::Dict(Arc::from([])))
        );
    }

    #[test]
    fn keys_reject_expressions() {
        assert_eq!(
            Key::from_value(&Value::expr(Expr::Value(Value::Number(1.into())))),
            None
        );
    }

    #[test]
    fn abstract_global_path_keys_are_distinct_from_list_keys() {
        let abstract_path = Key::abstract_global_path(["builtin", "unit"]);
        let list_path = Key::List(Arc::from([
            Key::binary_from_text("builtin"),
            Key::binary_from_text("unit"),
        ]));

        assert_ne!(abstract_path, list_path);
    }

    #[test]
    fn values_support_non_atom_key_paths() {
        let list_key = Key::List(Arc::from([Key::Number(1.into()), Key::Number(2.into())]));
        let dict = Dict::new_sync().insert(list_key.clone(), Value::Number(7.into()));
        let value = Value::Dict(dict);

        assert_eq!(
            value.get_key_path(&[list_key]),
            Some(&Value::Number(7.into()))
        );
    }

    #[test]
    fn list_concat_shares_segments() {
        let bytes = List::from_bytes(Bytes::from_static(b"Hello"));
        let values = List::from_values(vec![Value::Number(33.into())]);
        let list = List::concat(bytes, values);

        assert!(!list.is_empty());
    }

    #[test]
    fn balanced_lists_use_finger_tree_and_preserve_segments() {
        let list = List::concat(
            List::concat(
                List::from_bytes(Bytes::from_static(b"He")),
                List::from_bytes(Bytes::from_static(b"ll")),
            ),
            List::from_values(vec![Value::Number(111.into()), Value::Number(33.into())]),
        );

        let balanced = list.balanced();

        assert_eq!(balanced.len(), 6);
        let bytes = std::cell::RefCell::new(Vec::new());
        let values = std::cell::RefCell::new(Vec::new());
        balanced
            .for_each_segment(
                &mut |segment| {
                    bytes.borrow_mut().extend_from_slice(segment);
                    Ok::<_, ()>(())
                },
                &mut |segment| {
                    values.borrow_mut().extend(segment.iter().cloned());
                    Ok(())
                },
            )
            .expect("balanced list should walk");
        assert_eq!(bytes.into_inner(), b"Hell");
        assert_eq!(
            values.into_inner(),
            vec![Value::Number(111.into()), Value::Number(33.into())]
        );
    }

    #[test]
    fn list_slice_uses_rope_segments() {
        let list = List::concat(
            List::from_bytes(Bytes::from_static(b"Hello")),
            List::from_values(vec![Value::Number(44.into()), Value::Number(32.into())]),
        )
        .balanced();

        let sliced = list.slice(1, 6);

        let bytes = std::cell::RefCell::new(Vec::new());
        let values = std::cell::RefCell::new(Vec::new());
        sliced
            .for_each_segment(
                &mut |segment| {
                    bytes.borrow_mut().extend_from_slice(segment);
                    Ok::<_, ()>(())
                },
                &mut |segment| {
                    values.borrow_mut().extend(segment.iter().cloned());
                    Ok(())
                },
            )
            .expect("sliced list should walk");
        assert_eq!(bytes.into_inner(), b"ello");
        assert_eq!(values.into_inner(), vec![Value::Number(44.into())]);
    }

    #[test]
    fn list_slice_shares_partial_byte_and_value_leaves() {
        let bytes = Bytes::from_static(b"Hello");
        let value_leaf =
            List::from_values(vec![Value::Number(44.into()), Value::Number(32.into())]);
        let original_value_ptr = std::cell::Cell::new(std::ptr::null());
        value_leaf
            .for_each_segment(&mut |_| Ok::<_, ()>(()), &mut |values| {
                original_value_ptr.set(values.as_ptr());
                Ok(())
            })
            .unwrap();
        let list = List::concat(List::from_bytes(bytes.clone()), value_leaf).balanced();

        let sliced = list.slice(1, 6);

        let byte_ptrs = std::cell::RefCell::new(Vec::new());
        let value_ptrs = std::cell::RefCell::new(Vec::new());
        sliced
            .for_each_segment(
                &mut |segment| {
                    byte_ptrs.borrow_mut().push(segment.as_ptr());
                    Ok::<_, ()>(())
                },
                &mut |segment| {
                    value_ptrs.borrow_mut().push(segment.as_ptr());
                    Ok(())
                },
            )
            .expect("sliced list should walk");

        assert_eq!(byte_ptrs.into_inner(), vec![bytes[1..].as_ptr()]);
        assert_eq!(value_ptrs.into_inner(), vec![original_value_ptr.get()]);
    }

    #[test]
    fn list_split_from_end_preserves_lazy_concat_when_split_is_in_right_branch() {
        let left = List::from_values(vec![Value::Number(1.into()), Value::Number(2.into())]);
        let list = List::concat(left.clone(), List::from_bytes(Bytes::from_static(b"abc")));

        let (prefix, suffix) = list
            .split_from_end(1)
            .expect("suffix count should be in bounds");

        assert_eq!(prefix.len(), left.len() + 2);

        let bytes = std::cell::RefCell::new(Vec::new());
        suffix
            .for_each_segment(
                &mut |segment| {
                    bytes.borrow_mut().extend_from_slice(segment);
                    Ok::<_, ()>(())
                },
                &mut |_| Ok(()),
            )
            .expect("suffix should walk");
        assert_eq!(bytes.into_inner(), b"c");
    }
}
