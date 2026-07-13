use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use bytes::Bytes;
use fingertrees::measure::Measured;
use fingertrees::monoid::Sum;
use internment::Intern;
use rpds::RedBlackTreeMapSync;

use crate::number::Number;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    Value(Value),
    List(Arc<[Arc<Expr>]>),
    Apply(Arc<Expr>, Arc<Expr>),
    Lambda(Arc<Expr>),
    Local(usize),
    Access(Arc<Expr>, Arc<[KeyExpr]>),
    Deferred(Arc<DeferredValue>),
    Future(IVar),
    Error(Arc<str>),
}

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
            Value::List(list) => Some(Self::List(list.to_key_items()?)),
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
            Value::Builtin(_) => None,
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
    Closure(Closure),
    Expr(Thunk),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Closure {
    pub body: Arc<Expr>,
    pub env: Arc<[Value]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thunk {
    pub expr: Arc<Expr>,
    pub env: Arc<[Value]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Builtin {
    Append,
    Add,
    Subtract,
    Multiply,
    Divide,
    Fixpoint,
    Anno,
    MergeDuplicate,
    Floor,
    Mod,
    Slice,
    Map,
    ListLen,
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
            Self::Fixpoint => 1,
            Self::Anno => 2,
            Self::MergeDuplicate => 3,
            Self::Floor => 1,
            Self::Mod => 2,
            Self::Slice => 3,
            Self::Map => 2,
            Self::ListLen => 1,
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

#[derive(Debug, Clone)]
pub struct List(Arc<ListNode>);

#[derive(Debug, Clone)]
enum ListNode {
    Empty,
    Bytes(Bytes),
    Values(SharedSlice<Value>),
    Concat(List, List),
    Finger(FingerList),
}

type FingerList = fingertrees::sync::FingerTree<ListChunk>;

#[derive(Debug, Clone, PartialEq, Eq)]
enum ListChunk {
    Bytes(Bytes),
    Values(SharedSlice<Value>),
}

#[derive(Clone)]
struct SharedSlice<T> {
    data: Arc<[T]>,
    start: usize,
    len: usize,
}

impl<T> SharedSlice<T> {
    fn from_vec(values: Vec<T>) -> Self {
        let len = values.len();
        Self {
            data: Arc::from(values),
            start: 0,
            len,
        }
    }

    fn len(&self) -> usize {
        self.len
    }

    fn as_slice(&self) -> &[T] {
        &self.data[self.start..self.start + self.len]
    }

    fn slice(&self, start: usize, end: usize) -> Self {
        assert!(start <= end);
        assert!(end <= self.len);
        Self {
            data: self.data.clone(),
            start: self.start + start,
            len: end - start,
        }
    }
}

impl<T: fmt::Debug> fmt::Debug for SharedSlice<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_slice().fmt(f)
    }
}

impl<T: PartialEq> PartialEq for SharedSlice<T> {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl<T: Eq> Eq for SharedSlice<T> {}

impl ListChunk {
    fn len(&self) -> usize {
        match self {
            Self::Bytes(bytes) => bytes.len(),
            Self::Values(values) => values.len(),
        }
    }

    fn slice(&self, start: usize, end: usize) -> Option<Self> {
        assert!(start <= end);
        assert!(end <= self.len());
        if start == end {
            None
        } else {
            Some(match self {
                Self::Bytes(bytes) => Self::Bytes(bytes.slice(start..end)),
                Self::Values(values) => Self::Values(values.slice(start, end)),
            })
        }
    }

    fn for_each_segment<E>(
        &self,
        on_bytes: &mut impl FnMut(&[u8]) -> Result<(), E>,
        on_values: &mut impl FnMut(&[Value]) -> Result<(), E>,
    ) -> Result<(), E> {
        match self {
            Self::Bytes(bytes) => on_bytes(bytes),
            Self::Values(values) => on_values(values.as_slice()),
        }
    }
}

impl Measured for ListChunk {
    type Measure = Sum<usize>;

    fn measure(&self) -> Self::Measure {
        Sum(self.len())
    }
}

impl PartialEq for List {
    fn eq(&self, other: &Self) -> bool {
        if self.len() != other.len() {
            return false;
        }
        self.to_value_items_for_eq() == other.to_value_items_for_eq()
    }
}

impl Eq for List {}

impl List {
    pub fn empty() -> Self {
        Self(Arc::new(ListNode::Empty))
    }

    pub fn from_bytes(bytes: impl Into<Bytes>) -> Self {
        let bytes = bytes.into();
        if bytes.is_empty() {
            Self::empty()
        } else {
            Self(Arc::new(ListNode::Bytes(bytes)))
        }
    }

    pub fn from_values(values: Vec<Value>) -> Self {
        if values.is_empty() {
            Self::empty()
        } else {
            Self(Arc::new(ListNode::Values(SharedSlice::from_vec(values))))
        }
    }

    pub fn concat(left: Self, right: Self) -> Self {
        if left.is_empty() {
            right
        } else if right.is_empty() {
            left
        } else {
            Self(Arc::new(ListNode::Concat(left, right)))
        }
    }

    pub fn len(&self) -> usize {
        match self.0.as_ref() {
            ListNode::Empty => 0,
            ListNode::Bytes(bytes) => bytes.len(),
            ListNode::Values(values) => values.len(),
            ListNode::Concat(left, right) => left.len() + right.len(),
            ListNode::Finger(finger) => finger.measure().0,
        }
    }

    pub fn balanced(&self) -> Self {
        Self::from_finger(self.to_finger())
    }

    pub fn slice(&self, start: usize, end: usize) -> Self {
        assert!(start <= end);
        assert!(end <= self.len());
        self.slice_checked(start, end)
    }

    pub fn for_each_segment<E>(
        &self,
        on_bytes: &mut impl FnMut(&[u8]) -> Result<(), E>,
        on_values: &mut impl FnMut(&[Value]) -> Result<(), E>,
    ) -> Result<(), E> {
        match self.0.as_ref() {
            ListNode::Empty => Ok(()),
            ListNode::Bytes(bytes) => on_bytes(bytes),
            ListNode::Values(values) => on_values(values.as_slice()),
            ListNode::Concat(left, right) => {
                left.for_each_segment(on_bytes, on_values)?;
                right.for_each_segment(on_bytes, on_values)
            }
            ListNode::Finger(finger) => finger
                .iter()
                .try_for_each(|chunk| chunk.for_each_segment(on_bytes, on_values)),
        }
    }

    fn is_empty(&self) -> bool {
        matches!(self.0.as_ref(), ListNode::Empty)
    }

    fn from_finger(finger: FingerList) -> Self {
        if finger.is_empty() {
            Self::empty()
        } else {
            Self(Arc::new(ListNode::Finger(finger)))
        }
    }

    fn to_finger(&self) -> FingerList {
        let mut finger = FingerList::new();
        self.push_chunks_into(&mut finger);
        finger
    }

    fn push_chunks_into(&self, finger: &mut FingerList) {
        match self.0.as_ref() {
            ListNode::Empty => {}
            ListNode::Bytes(bytes) => {
                *finger = finger.push_right(ListChunk::Bytes(bytes.clone()));
            }
            ListNode::Values(values) => {
                *finger = finger.push_right(ListChunk::Values(values.clone()));
            }
            ListNode::Concat(left, right) => {
                left.push_chunks_into(finger);
                right.push_chunks_into(finger);
            }
            ListNode::Finger(right) => *finger = finger.concat(right),
        }
    }

    fn to_value_items_for_eq(&self) -> Vec<Value> {
        let items = std::cell::RefCell::new(Vec::new());
        self.for_each_segment(
            &mut |bytes| {
                items.borrow_mut().extend(
                    bytes
                        .iter()
                        .map(|byte| Value::Number(Number::from_u8(*byte))),
                );
                Ok::<_, ()>(())
            },
            &mut |values| {
                items.borrow_mut().extend(values.iter().cloned());
                Ok(())
            },
        )
        .expect("collecting list items for equality should not fail");
        items.into_inner()
    }

    fn slice_checked(&self, start: usize, end: usize) -> Self {
        if start == end {
            return Self::empty();
        }

        match self.0.as_ref() {
            ListNode::Empty => Self::empty(),
            ListNode::Bytes(bytes) => Self::from_bytes(bytes.slice(start..end)),
            ListNode::Values(values) => Self(Arc::new(ListNode::Values(values.slice(start, end)))),
            ListNode::Concat(left, right) => {
                Self::slice_concat(left, left.len(), right, start, end)
            }
            ListNode::Finger(finger) => Self::slice_finger(finger, start, end),
        }
    }

    fn slice_finger(finger: &FingerList, start: usize, end: usize) -> Self {
        let (_, tail) = Self::split_finger_at(finger, start);
        let (middle, _) = Self::split_finger_at(&tail, end - start);
        Self::from_finger(middle)
    }

    fn split_finger_at(finger: &FingerList, index: usize) -> (FingerList, FingerList) {
        let len = finger.measure().0;
        assert!(index <= len);
        if index == 0 {
            return (FingerList::new(), finger.clone());
        }
        if index == len {
            return (finger.clone(), FingerList::new());
        }

        let (mut left, right) = finger.split(|measure| measure.0 > index);
        let left_len = left.measure().0;
        if left_len == index {
            return (left, right);
        }

        let Some((chunk, tail)) = right.view_left() else {
            unreachable!("finger split inside a non-empty tree should leave a boundary chunk");
        };
        let chunk_offset = index - left_len;
        if let Some(chunk_left) = chunk.slice(0, chunk_offset) {
            left = left.push_right(chunk_left);
        }

        let mut right = tail;
        if let Some(chunk_right) = chunk.slice(chunk_offset, chunk.len()) {
            right = right.push_left(chunk_right);
        }
        (left, right)
    }

    fn slice_concat(left: &List, left_len: usize, right: &List, start: usize, end: usize) -> Self {
        if end <= left_len {
            left.slice_checked(start, end)
        } else if start >= left_len {
            right.slice_checked(start - left_len, end - left_len)
        } else {
            Self::concat(
                left.slice_checked(start, left_len),
                right.slice_checked(0, end - left_len),
            )
        }
    }

    fn to_key_items(&self) -> Option<Arc<[Key]>> {
        let items = std::cell::RefCell::new(Vec::new());
        self.for_each_segment(
            &mut |bytes| {
                items
                    .borrow_mut()
                    .extend(bytes.iter().map(|byte| Key::Number(Number::from_u8(*byte))));
                Ok::<_, ()>(())
            },
            &mut |values| {
                for value in values.iter() {
                    items.borrow_mut().push(Key::from_value(value).ok_or(())?);
                }
                Ok(())
            },
        )
        .ok()?;
        Some(Arc::from(items.into_inner()))
    }
}

impl Value {
    pub fn binary_from_text(text: &str) -> Self {
        Self::Binary(Bytes::copy_from_slice(text.as_bytes()))
    }

    pub fn expr(expr: Expr) -> Self {
        Self::Expr(Thunk {
            expr: Arc::new(expr),
            env: Arc::from([]),
        })
    }

    pub fn expr_arc(expr: Arc<Expr>) -> Self {
        Self::Expr(Thunk {
            expr,
            env: Arc::from([]),
        })
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
                | Value::Closure(_)
                | Value::Builtin(_)
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
        assert!(matches!(balanced.0.as_ref(), ListNode::Finger(_)));
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
        let original_value_ptr = match value_leaf.0.as_ref() {
            ListNode::Values(values) => values.as_slice().as_ptr(),
            _ => panic!("value leaf should store values"),
        };
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
        assert_eq!(value_ptrs.into_inner(), vec![original_value_ptr]);
    }
}
