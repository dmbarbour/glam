use std::fmt;
use std::sync::Arc;

use internment::Intern;
use rpds::RedBlackTreeMapSync;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Term {
    Expr(Expr),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    Value(Value),
    List(Arc<[Arc<Expr>]>),
    Apply(Arc<Expr>, Arc<Expr>),
    Name(Arc<[KeyExpr]>),
    SingletonDict {
        key: KeyExpr,
        value: Arc<Expr>,
    },
    DictUnion {
        items: Arc<[Arc<Expr>]>,
        key_context: Option<Key>,
    },
    Error(Arc<str>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyExpr {
    Key(Key),
    Expr(Arc<Expr>),
    ListExpr(Arc<Expr>),
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
    Number(i64),
    Binary(Arc<[u8]>),
    List(Arc<[Key]>),
    Dict(Arc<[(Key, Key)]>),
}

impl Key {
    pub fn atom_from_text(text: impl Into<String>) -> Self {
        Self::atom_from_key(&Self::binary_from_text(text))
    }

    pub fn atom_from_key(key: &Key) -> Self {
        Self::Atom(Atom::from_key(key))
    }

    pub fn binary_from_text(text: impl Into<String>) -> Self {
        Self::Binary(Arc::from(text.into().into_bytes()))
    }

    pub fn from_value(value: &Value) -> Option<Self> {
        match value {
            Value::Number(number) => Some(Self::Number(*number)),
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
            Key::List(items) => f.debug_tuple("List").field(items).finish(),
            Key::Dict(entries) => f.debug_tuple("Dict").field(entries).finish(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Number(i64),
    Binary(Arc<[u8]>),
    List(List),
    Dict(Dict),
    Builtin(Builtin),
    Expr(Arc<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Builtin {
    Append,
}

impl Builtin {
    pub fn arity(self) -> usize {
        match self {
            Self::Append => 2,
        }
    }
}

pub type Dict = RedBlackTreeMapSync<Key, Value>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct List(Arc<ListNode>);

#[derive(Debug, Clone, PartialEq, Eq)]
enum ListNode {
    Empty,
    Bytes(Arc<[u8]>),
    Values(Arc<[Value]>),
    Concat(List, List),
}

impl List {
    pub fn empty() -> Self {
        Self(Arc::new(ListNode::Empty))
    }

    pub fn from_bytes(bytes: Arc<[u8]>) -> Self {
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
            Self(Arc::new(ListNode::Values(Arc::from(values))))
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

    pub fn for_each_segment<E>(
        &self,
        on_bytes: &mut impl FnMut(&Arc<[u8]>) -> Result<(), E>,
        on_values: &mut impl FnMut(&Arc<[Value]>) -> Result<(), E>,
    ) -> Result<(), E> {
        match self.0.as_ref() {
            ListNode::Empty => Ok(()),
            ListNode::Bytes(bytes) => on_bytes(bytes),
            ListNode::Values(values) => on_values(values),
            ListNode::Concat(left, right) => {
                left.for_each_segment(on_bytes, on_values)?;
                right.for_each_segment(on_bytes, on_values)
            }
        }
    }

    fn is_empty(&self) -> bool {
        matches!(self.0.as_ref(), ListNode::Empty)
    }

    fn to_key_items(&self) -> Option<Arc<[Key]>> {
        let items = std::cell::RefCell::new(Vec::new());
        self.for_each_segment(
            &mut |bytes| {
                items
                    .borrow_mut()
                    .extend(bytes.iter().map(|byte| Key::Number(i64::from(*byte))));
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
        Self::Binary(Arc::from(text.as_bytes()))
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
                Value::Number(_)
                | Value::Binary(_)
                | Value::List(_)
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
            Value::Number(1),
            Value::Number(2),
            Value::Number(3),
        ]));

        assert!(matches!(value, Value::List(_)));
    }

    #[test]
    fn semantic_expr_can_hold_literal_values_builtins_and_application() {
        let expr = Expr::Apply(
            Arc::new(Expr::Apply(
                Arc::new(Expr::Value(Value::Builtin(Builtin::Append))),
                Arc::new(Expr::Value(Value::List(List::from_values(vec![
                    Value::Number(1),
                ])))),
            )),
            Arc::new(Expr::Value(Value::List(List::from_values(vec![
                Value::Number(2),
            ])))),
        );

        assert!(matches!(expr, Expr::Apply(_, _)));
    }

    #[test]
    fn semantic_expr_can_hold_lists() {
        let expr = Expr::List(Arc::from([
            Arc::new(Expr::Value(Value::Number(1))),
            Arc::new(Expr::Value(Value::Number(2))),
        ]));

        assert!(matches!(expr, Expr::List(items) if items.len() == 2));
    }

    #[test]
    fn semantic_expr_can_hold_names() {
        let expr = Expr::Name(Arc::from([KeyExpr::Key(Key::atom_from_text("hello"))]));

        assert!(matches!(expr, Expr::Name(path) if path.len() == 1));
    }

    #[test]
    fn semantic_expr_can_hold_singleton_dicts_unions_and_errors() {
        let expr = Expr::DictUnion {
            items: Arc::from([
                Arc::new(Expr::SingletonDict {
                    key: KeyExpr::Key(Key::atom_from_text("greeting")),
                    value: Arc::new(Expr::Value(Value::binary_from_text("Hello"))),
                }),
                Arc::new(Expr::Error(Arc::from("ambiguous key"))),
            ]),
            key_context: Some(Key::atom_from_text("greeting")),
        };

        assert!(matches!(expr, Expr::DictUnion { items, .. } if items.len() == 2));
    }

    #[test]
    fn keys_can_represent_nested_value_data() {
        let value = Value::Dict(Dict::new_sync().insert(
            Key::atom_from_text("payload"),
            Value::List(List::concat(
                List::from_values(vec![Value::Number(1)]),
                List::from_bytes(Arc::from(&b"Hi"[..])),
            )),
        ));

        assert_eq!(
            Key::from_value(&value),
            Some(Key::Dict(Arc::from([(
                Key::atom_from_text("payload"),
                Key::List(Arc::from([
                    Key::Number(1),
                    Key::Number(i64::from(b'H')),
                    Key::Number(i64::from(b'i')),
                ])),
            )])))
        );
    }

    #[test]
    fn keys_reject_expressions() {
        assert_eq!(
            Key::from_value(&Value::Expr(Arc::new(Expr::Value(Value::Number(1))))),
            None
        );
    }

    #[test]
    fn values_support_non_atom_key_paths() {
        let list_key = Key::List(Arc::from([Key::Number(1), Key::Number(2)]));
        let dict = Dict::new_sync().insert(list_key.clone(), Value::Number(7));
        let value = Value::Dict(dict);

        assert_eq!(value.get_key_path(&[list_key]), Some(&Value::Number(7)));
    }

    #[test]
    fn list_concat_shares_segments() {
        let bytes = List::from_bytes(Arc::from(&b"Hello"[..]));
        let values = List::from_values(vec![Value::Number(33)]);
        let list = List::concat(bytes, values);

        assert!(!list.is_empty());
    }
}
