//! Lazily initialized keys for evaluator protocol fields.
//!
//! Borrow these for lookup and comparison. Clone only when an owned key is
//! required by a persistent-map update.

use std::sync::LazyLock;

use super::{Atom, Key, Value};

macro_rules! protocol_key {
    ($name:ident, $text:literal) => {
        pub(crate) static $name: LazyLock<Key> = LazyLock::new(|| Key::atom_from_text($text));
    };
}

protocol_key!(APPLY, "apply");
protocol_key!(EFF, "eff");

protocol_key!(SPEC, "spec");
protocol_key!(NAME, "name");
protocol_key!(DEPS, "deps");
protocol_key!(DEFS, "defs");

protocol_key!(VALUE, "value");
protocol_key!(LEFT, "left");
protocol_key!(RIGHT, "right");
protocol_key!(TUPLE, "tuple");

protocol_key!(R, "r");
protocol_key!(SEQ, "seq");
protocol_key!(ALT, "alt");
protocol_key!(FAIL, "fail");
protocol_key!(CUT, "cut");
protocol_key!(FIX, "fix");

pub(crate) static UNIT: LazyLock<Key> =
    LazyLock::new(|| Key::abstract_global_path(["builtin", "unit"]));
pub(crate) static UNIT_VALUE: LazyLock<Value> =
    LazyLock::new(|| Value::Atom(Atom::from_key(&UNIT)));
