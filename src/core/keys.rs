//! Lazily initialized keys for evaluator protocol fields.
//!
//! Borrow these for lookup and comparison. Clone only when an owned key is
//! required by a persistent-map update.

use std::sync::LazyLock;

use super::Key;

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

protocol_key!(MSG, "msg");
protocol_key!(TEXT, "text");
protocol_key!(SEVERITY, "severity");
protocol_key!(LOCATION, "location");
protocol_key!(LINE, "line");
protocol_key!(ORIGIN, "origin");
protocol_key!(SOURCE, "source");
protocol_key!(DIGEST, "digest");
protocol_key!(INVOCATION, "invocation");
protocol_key!(NAMESPACE, "namespace");
protocol_key!(IMPORT_CHAIN, "import_chain");
protocol_key!(IMPORTER, "importer");
protocol_key!(REQUEST, "request");
protocol_key!(REPLY, "reply");
protocol_key!(PATH, "path");
protocol_key!(PACKAGE, "package");
protocol_key!(FOLDER, "folder");
protocol_key!(SCOPE, "scope");
protocol_key!(SOURCES, "sources");
protocol_key!(LANG, "lang");
protocol_key!(EXTENDS, "extends");

protocol_key!(VALUE, "value");
protocol_key!(CONTEXT, "context");
protocol_key!(DIAGNOSTIC, "diagnostic");
protocol_key!(FROM, "from");
protocol_key!(EVAL, "eval");
protocol_key!(OP, "op");
protocol_key!(ARGS, "args");
protocol_key!(G, "g");
protocol_key!(IMPORT, "import");
protocol_key!(DEFINITION, "definition");
protocol_key!(KEY, "key");
protocol_key!(OK, "ok");
protocol_key!(ERR, "err");
protocol_key!(LAUNCHED, "launched");
protocol_key!(BLOCKED, "blocked");
protocol_key!(CANCELED, "canceled");
protocol_key!(ABANDONED, "abandoned");
protocol_key!(EXITED, "exited");
protocol_key!(KILLED, "killed");
protocol_key!(PANICKED, "panicked");
protocol_key!(LEFT, "left");
protocol_key!(RIGHT, "right");
protocol_key!(HEAD, "head");
protocol_key!(TAIL, "tail");
protocol_key!(INIT, "init");
protocol_key!(LAST, "last");
protocol_key!(REST, "rest");
protocol_key!(TUPLE, "tuple");

protocol_key!(R, "r");
protocol_key!(SEQ, "seq");
protocol_key!(ALT, "alt");
protocol_key!(FAIL, "fail");
protocol_key!(CUT, "cut");
protocol_key!(FIX, "fix");
protocol_key!(GET, "get");
protocol_key!(SET, "set");
protocol_key!(RESET, "reset");
protocol_key!(SHIFT, "shift");
// A pure handler's state and outcomes (`pure-handler-core`).
protocol_key!(USER, "user");
protocol_key!(HANDLER, "handler");
protocol_key!(STATE, "state");
protocol_key!(FAILED, "failed");
// The CLI's token parser's state and evidence (`pure-handler-token`).
protocol_key!(CURSOR, "cursor");
protocol_key!(EXPECTATIONS, "expectations");
protocol_key!(CANDIDATES, "candidates");
protocol_key!(OFFSET, "offset");
protocol_key!(LABEL, "label");
protocol_key!(REPLACEMENT, "replacement");
protocol_key!(SPAN, "span");

pub(crate) static UNIT: LazyLock<Key> =
    LazyLock::new(|| Key::abstract_global_path(["builtin", "unit"]));

protocol_key!(INFO, "info");
protocol_key!(WARN, "warn");
protocol_key!(ERROR, "error");

protocol_key!(FILE, "file");
