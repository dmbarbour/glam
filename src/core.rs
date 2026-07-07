use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Term {
    Data(Value),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Key {
    Name(String),
    Text(String),
}

impl Key {
    pub fn name(name: impl Into<String>) -> Self {
        Self::Name(name.into())
    }

    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(text.into())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Text(String),
    Dict(BTreeMap<Key, Value>),
}

impl Value {
    pub fn get_name_path(&self, path: &[&str]) -> Option<&Value> {
        match path {
            [] => Some(self),
            [head, rest @ ..] => match self {
                Value::Dict(dict) => dict.get(&Key::name(*head))?.get_name_path(rest),
                Value::Text(_) => None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_and_text_keys_are_distinct() {
        let mut dict = BTreeMap::new();
        dict.insert(Key::name("asm"), Value::Text("name".to_owned()));
        dict.insert(Key::text("asm"), Value::Text("text".to_owned()));
        let value = Value::Dict(dict);

        assert_eq!(
            value.get_name_path(&["asm"]),
            Some(&Value::Text("name".to_owned()))
        );
    }
}
