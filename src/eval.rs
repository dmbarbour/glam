use std::fmt;

use crate::core::{Term, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assembly {
    root: Value,
}

impl Assembly {
    pub fn get(&self, path: &str) -> Option<&Value> {
        self.root
            .get_name_path(&path.split('.').collect::<Vec<_>>())
    }

    pub fn result_bytes(&self) -> Result<Vec<u8>, EvalError> {
        match self.get("asm.result") {
            Some(Value::Text(text)) => Ok(text.as_bytes().to_vec()),
            Some(Value::Dict(_)) => Err(EvalError::new("`asm.result` is not binary text data")),
            None => Err(EvalError::new("assembly did not define `asm.result`")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalError {
    message: String,
}

impl EvalError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for EvalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for EvalError {}

pub fn eval_term(term: &Term) -> Result<Assembly, EvalError> {
    match term {
        Term::Data(value @ Value::Dict(_)) => Ok(Assembly {
            root: value.clone(),
        }),
        Term::Data(Value::Text(_)) => {
            Err(EvalError::new("assembly root must be a dictionary value"))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::core::{Key, Term, Value};

    use super::*;

    #[test]
    fn evaluates_text_result_to_bytes() {
        let mut asm = BTreeMap::new();
        asm.insert(Key::name("result"), Value::Text("Hello, World!".to_owned()));
        let mut root = BTreeMap::new();
        root.insert(Key::name("asm"), Value::Dict(asm));

        let assembly = eval_term(&Term::Data(Value::Dict(root))).expect("assembly should evaluate");

        assert_eq!(
            assembly.result_bytes().expect("result should extract"),
            b"Hello, World!"
        );
    }

    #[test]
    fn reports_missing_result() {
        let assembly =
            eval_term(&Term::Data(Value::Dict(BTreeMap::new()))).expect("assembly should evaluate");

        assert_eq!(
            assembly.result_bytes().unwrap_err().to_string(),
            "assembly did not define `asm.result`"
        );
    }
}
