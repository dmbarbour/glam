use std::collections::BTreeMap;
use std::fmt;

use internment::Intern;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Term {
    Data(Value),
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
    Text(String),
    Dict(BTreeMap<Key, Value>),
}

impl Value {
    pub fn get_atom_path(&self, path: &[Atom]) -> Option<&Value> {
        match path {
            [] => Some(self),
            [head, rest @ ..] => match self {
                Value::Dict(dict) => dict.get(&Key::Atom(head.clone()))?.get_atom_path(rest),
                Value::Text(_) => None,
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
        let mut dict = BTreeMap::new();
        dict.insert(Key::Atom(asm.clone()), Value::Text("atom".to_owned()));
        dict.insert(Key::text("asm"), Value::Text("text".to_owned()));
        let value = Value::Dict(dict);

        assert_eq!(
            value.get_atom_path(&[asm]),
            Some(&Value::Text("atom".to_owned()))
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
}
