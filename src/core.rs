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
    Append(Arc<Expr>, Arc<Expr>),
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
    Text(String),
}

impl Key {
    pub fn atom_from_text(text: impl Into<String>) -> Self {
        Self::atom_from_key(&Self::text(text))
    }

    pub fn atom_from_key(key: &Key) -> Self {
        Self::Atom(Atom::from_key(key))
    }

    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(text.into())
    }
}

impl fmt::Debug for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Key::Atom(atom) => f.debug_tuple("Atom").field(atom).finish(),
            Key::Text(text) => f.debug_tuple("Text").field(text).finish(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Number(i64),
    Binary(Arc<[u8]>),
    List(List),
    Dict(Dict),
    Expr(Arc<Expr>),
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
    pub fn get_atom_path(&self, path: &[Atom]) -> Option<&Value> {
        match path {
            [] => Some(self),
            [head, rest @ ..] => match self {
                Value::Dict(dict) => dict.get(&Key::Atom(head.clone()))?.get_atom_path(rest),
                Value::Number(_) | Value::Binary(_) | Value::List(_) | Value::Expr(_) => None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atoms_and_text_keys_are_distinct() {
        let asm = Atom::from_key(&Key::text("asm"));
        let dict = Dict::new_sync()
            .insert(Key::Atom(asm.clone()), Value::binary_from_text("atom"))
            .insert(Key::text("asm"), Value::binary_from_text("text"));
        let value = Value::Dict(dict);

        assert_eq!(
            value.get_atom_path(&[asm]),
            Some(&Value::binary_from_text("atom"))
        );
    }

    #[test]
    fn atom_keys_are_canonical_by_key() {
        assert_eq!(
            Atom::from_key(&Key::text("asm")),
            Atom::from_key(&Key::text("asm"))
        );
        assert_eq!(Atom::from_key(&Key::text("asm")).key(), &Key::text("asm"));
    }

    #[test]
    fn atom_keys_from_equal_keys_are_canonical() {
        let text_key = Key::text("tag");
        let atom_key_1 = Key::atom_from_key(&text_key);
        let atom_key_2 = Key::atom_from_key(&Key::text("tag"));

        assert!(matches!(atom_key_1, Key::Atom(_)));
        assert_eq!(atom_key_1, atom_key_2);
        assert_ne!(atom_key_1, text_key);
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
    fn semantic_expr_can_hold_literal_values_and_append() {
        let expr = Expr::Append(
            Arc::new(Expr::Value(Value::List(List::from_values(vec![
                Value::Number(1),
            ])))),
            Arc::new(Expr::Value(Value::List(List::from_values(vec![
                Value::Number(2),
            ])))),
        );

        assert!(matches!(expr, Expr::Append(_, _)));
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
    fn list_concat_shares_segments() {
        let bytes = List::from_bytes(Arc::from(&b"Hello"[..]));
        let values = List::from_values(vec![Value::Number(33)]);
        let list = List::concat(bytes, values);

        assert!(!list.is_empty());
    }
}
