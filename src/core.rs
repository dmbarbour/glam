use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use bytes::Bytes;
use internment::Intern;
use rpds::RedBlackTreeMapSync;

use crate::core_net::{ClosedLambdaNet, CoreDataKey, CoreRuntimeNet, lower_closed_lambda};
use crate::number::Number;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    Value(Value),
    List(Arc<[Arc<Expr>]>),
    Apply(Arc<Expr>, Arc<Expr>),
    Lambda(Arc<Lambda>),
    Local(usize),
    Access(Arc<Expr>, Arc<[KeyExpr]>),
    Deferred(Arc<DeferredValue>),
    Future(IVar),
    Error(Arc<str>),
}

impl Expr {
    pub fn lambda(body: Arc<Expr>) -> Self {
        Self::Lambda(Arc::new(Lambda::new(body)))
    }
}

#[derive(Debug)]
pub struct Lambda {
    body: Arc<Expr>,
    closed_net: OnceLock<ClosedLambdaNet>,
}

impl Lambda {
    pub fn new(body: Arc<Expr>) -> Self {
        Self {
            body,
            closed_net: OnceLock::new(),
        }
    }

    pub fn body(&self) -> &Arc<Expr> {
        &self.body
    }

    pub(crate) fn prepare_closed_net(&self) {
        self.closed_net
            .get_or_init(|| lower_closed_lambda(self.body.clone()));
    }

    pub(crate) fn closed_net(&self) -> Option<ClosedLambdaNet> {
        self.closed_net.get().cloned()
    }

    #[cfg(test)]
    pub(crate) fn is_closed_lowered(&self) -> bool {
        self.closed_net.get().is_some()
    }
}

impl PartialEq for Lambda {
    fn eq(&self, other: &Self) -> bool {
        self.body == other.body
    }
}

impl Eq for Lambda {}

#[derive(Clone)]
pub struct DeferredValue {
    id: u64,
    label: Arc<str>,
    thunk: Arc<dyn Fn() -> Result<Value, String> + Send + Sync>,
    result: Arc<OnceLock<Result<Value, Arc<str>>>>,
}

impl DeferredValue {
    pub fn new(
        label: impl Into<Arc<str>>,
        thunk: impl Fn() -> Result<Value, String> + Send + Sync + 'static,
    ) -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);

        Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            label: label.into(),
            thunk: Arc::new(thunk),
            result: Arc::new(OnceLock::new()),
        }
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn force(&self) -> Result<Value, Arc<str>> {
        self.result
            .get_or_init(|| (self.thunk)().map_err(Arc::from))
            .clone()
    }
}

impl PartialEq for DeferredValue {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for DeferredValue {}

impl fmt::Debug for DeferredValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeferredValue")
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
            Value::Builtin(_) | Value::PartialBuiltin(_) | Value::Net(_) => None,
            Value::Closure(_) => None,
            Value::Expr(_) => None,
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

#[derive(Clone)]
pub struct IVar(Arc<OnceLock<Value>>);

impl IVar {
    pub fn new() -> Self {
        Self(Arc::new(OnceLock::new()))
    }

    pub fn get(&self) -> Option<&Value> {
        self.0.get()
    }

    pub fn set(&self, value: Value) -> Result<(), Value> {
        self.0.set(value)
    }
}

impl PartialEq for IVar {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for IVar {}

impl fmt::Debug for IVar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FixpointHandle(..)")
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
    /// A closed interaction net with one designated exposed port.
    Net(NetValue),
    /// Temporary expression-evaluator compatibility for lambda bodies that
    /// still contain dictionary access.
    Closure(Closure),
    Expr(Thunk),
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

#[derive(Debug, Clone)]
pub struct Closure {
    pub env: Arc<[Value]>,
    pub(crate) source_body: Arc<Expr>,
}

impl PartialEq for Closure {
    fn eq(&self, other: &Self) -> bool {
        self.env == other.env && self.source_body == other.source_body
    }
}

impl Eq for Closure {}

#[derive(Clone)]
pub struct Thunk {
    source: ThunkSource,
    result: Arc<OnceLock<Result<Value, Arc<str>>>>,
}

#[derive(Clone, PartialEq, Eq)]
enum ThunkSource {
    Expr {
        expr: Arc<Expr>,
        env: Arc<[Value]>,
    },
    Access {
        path: Arc<[CoreDataKey]>,
        arguments: Arc<[Value]>,
    },
    Builtin(BuiltinCall),
}

impl Thunk {
    pub fn new(expr: Arc<Expr>, env: Arc<[Value]>) -> Self {
        Self {
            source: ThunkSource::Expr { expr, env },
            result: Arc::new(OnceLock::new()),
        }
    }

    pub(crate) fn from_access(path: Arc<[CoreDataKey]>, arguments: Arc<[Value]>) -> Self {
        Self {
            source: ThunkSource::Access { path, arguments },
            result: Arc::new(OnceLock::new()),
        }
    }

    pub(crate) fn from_builtin(call: BuiltinCall) -> Self {
        Self {
            source: ThunkSource::Builtin(call),
            result: Arc::new(OnceLock::new()),
        }
    }

    pub fn expr(&self) -> Option<&Arc<Expr>> {
        match &self.source {
            ThunkSource::Expr { expr, .. } => Some(expr),
            ThunkSource::Access { .. } | ThunkSource::Builtin(_) => None,
        }
    }

    pub fn env(&self) -> Option<&Arc<[Value]>> {
        match &self.source {
            ThunkSource::Expr { env, .. } => Some(env),
            ThunkSource::Access { .. } | ThunkSource::Builtin(_) => None,
        }
    }

    pub fn cached(&self) -> Option<Result<Value, Arc<str>>> {
        self.result.get().cloned()
    }

    pub fn cache(&self, value: Result<Value, Arc<str>>) -> Result<Value, Arc<str>> {
        let _ = self.result.set(value);
        self.result
            .get()
            .expect("thunk cache should contain a value after set")
            .clone()
    }

    pub(crate) fn access(&self) -> Option<(&[CoreDataKey], &[Value])> {
        match &self.source {
            ThunkSource::Access { path, arguments } => Some((path, arguments)),
            ThunkSource::Expr { .. } | ThunkSource::Builtin(_) => None,
        }
    }

    pub(crate) fn builtin(&self) -> Option<&BuiltinCall> {
        match &self.source {
            ThunkSource::Builtin(call) => Some(call),
            ThunkSource::Expr { .. } | ThunkSource::Access { .. } => None,
        }
    }
}

impl PartialEq for Thunk {
    fn eq(&self, other: &Self) -> bool {
        self.source == other.source
    }
}

impl Eq for Thunk {}

impl fmt::Debug for Thunk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.source {
            ThunkSource::Expr { expr, env } => f
                .debug_struct("Thunk")
                .field("expr", expr)
                .field("env", env)
                .finish_non_exhaustive(),
            ThunkSource::Access { path, arguments } => f
                .debug_struct("AccessThunk")
                .field("path", path)
                .field("arguments", arguments)
                .finish_non_exhaustive(),
            ThunkSource::Builtin(call) => f.debug_tuple("BuiltinThunk").field(call).finish(),
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

pub type List = crate::list::List<Value, Thunk>;

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
        Self::Expr(Thunk::new(Arc::new(expr), Arc::from([])))
    }

    pub fn expr_arc(expr: Arc<Expr>) -> Self {
        Self::Expr(Thunk::new(expr, Arc::from([])))
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
                | Value::Net(_)
                | Value::Closure(_)
                | Value::Builtin(_)
                | Value::PartialBuiltin(_)
                | Value::Expr(_) => None,
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
    fn semantic_expr_can_hold_errors() {
        let expr = Expr::Error(Arc::from("ambiguous key"));

        assert!(matches!(expr, Expr::Error(_)));
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
