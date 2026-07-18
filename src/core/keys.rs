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

macro_rules! protocol_value {
    ($name:ident, $val:ident) => {
        pub(crate) static $name: LazyLock<Value> =
            LazyLock::new(|| Value::Atom(Atom::from_key(&$val)));
    };
}

protocol_key!(APPLY, "apply");
protocol_key!(EFF, "eff");

protocol_key!(SPEC, "spec");
protocol_key!(NAME, "name");
protocol_key!(DEPS, "deps");
protocol_key!(DEFS, "defs");

protocol_key!(MSG, "msg");
protocol_key!(TEXT, "text");
protocol_key!(SEVERITY, "severity");
protocol_key!(LOCATION, "location");
protocol_key!(LINE, "line");
protocol_key!(ORIGIN, "origin");
protocol_key!(SOURCE, "source");
protocol_key!(INVOCATION, "invocation");
protocol_key!(NAMESPACE, "namespace");
protocol_key!(IMPORT_CHAIN, "import_chain");
protocol_key!(IMPORTER, "importer");
protocol_key!(REQUEST, "request");
protocol_key!(EXTENDS, "extends");

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
protocol_key!(HEAP, "heap");

pub(crate) static UNIT: LazyLock<Key> =
    LazyLock::new(|| Key::abstract_global_path(["builtin", "unit"]));
protocol_value!(UNIT_VALUE, UNIT);

pub(crate) static OBJECT_REFLECTION_GUARD: LazyLock<Key> =
    LazyLock::new(|| Key::abstract_global_path(["builtin", "reflection", "object_guard"]));
protocol_value!(OBJECT_REFLECTION_GUARD_VALUE, OBJECT_REFLECTION_GUARD);

protocol_key!(INFO, "info");
protocol_key!(WARN, "warn");
protocol_key!(ERROR, "error");
protocol_value!(INFO_VALUE, INFO);
protocol_value!(WARN_VALUE, WARN);
protocol_value!(ERROR_VALUE, ERROR);

protocol_key!(FILE, "file");
protocol_key!(SCRIPT, "script");
