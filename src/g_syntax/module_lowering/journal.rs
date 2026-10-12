//! The journal a namespace keeps in `meta.journal` of the names introduced,
//! overridden or updated in it (`perf-definition-assertions`).
//!
//! A stretch of declarations between imports appends its entries to each
//! journal it touched once, lazily, as `(default [] prior) ++ entries`. An
//! entry is `{op:intro:['foo], at:g:{origin, line}}`, with `over` for `:=`
//! and `update` for `::=`.

use std::collections::BTreeMap;

use super::super::*;
use super::objects::apply_builtin_resolved;
use crate::core::List;

#[derive(Clone, Copy)]
pub(in crate::g_syntax) enum JournalOp {
    Introduce,
    Override,
    Update,
    /// `declare`: the name must be defined by the journal's end.
    Declare,
    /// `assume`: the name was defined outside the journal.
    Assume,
}

impl JournalOp {
    fn tag(self) -> &'static str {
        match self {
            Self::Introduce => "intro",
            Self::Override => "over",
            Self::Update => "update",
            Self::Declare => "declare",
            Self::Assume => "assume",
        }
    }

    /// What declaring, assuming or defining a nested name says of each
    /// namespace on the way.
    fn of_namespace(self) -> Self {
        match self {
            Self::Declare | Self::Assume => self,
            Self::Introduce | Self::Override | Self::Update => Self::Update,
        }
    }
}

struct JournalEntry {
    op: JournalOp,
    path: Vec<Key>,
    line: usize,
}

/// The entries recorded since the last import, by namespace path.
#[derive(Default)]
pub(in crate::g_syntax) struct PendingJournal {
    namespaces: BTreeMap<Vec<String>, Vec<JournalEntry>>,
}

impl PendingJournal {
    /// Records `op` on `target` in the namespace that holds it, and of each
    /// namespace on the way in the one that holds that (an update, for a
    /// definition). A
    /// path through `refl` or `meta` stays whole, since neither keeps a
    /// journal of its own.
    pub(in crate::g_syntax) fn record(&mut self, target: &[&str], op: JournalOp, line: usize) {
        let mut namespace = Vec::new();
        let mut rest = target;
        while let [name, tail @ ..] = rest
            && !tail.is_empty()
            && !matches!(*name, "refl" | "meta")
        {
            self.push(&namespace, op.of_namespace(), vec![name_as_key(name)], line);
            namespace.push((*name).to_owned());
            rest = tail;
        }
        if !rest.is_empty() {
            let path = rest.iter().map(|name| name_as_key(name)).collect();
            self.push(&namespace, op, path, line);
        }
    }

    /// Records `op` on a top-level `key`.
    pub(in crate::g_syntax) fn record_key(&mut self, key: Key, op: JournalOp, line: usize) {
        self.push(&[], op, vec![key], line);
    }

    fn push(&mut self, namespace: &[String], op: JournalOp, path: Vec<Key>, line: usize) {
        let entry = JournalEntry { op, path, line };
        match self.namespaces.get_mut(namespace) {
            Some(entries) => entries.push(entry),
            None => {
                self.namespaces.insert(namespace.to_vec(), vec![entry]);
            }
        }
    }

    /// Writes each touched journal into `definitions`.
    pub(in crate::g_syntax) fn flush_in(
        &mut self,
        access: &RuntimeValueAccess<'_>,
        context: &CompileContext,
        mut definitions: Value,
    ) -> Value {
        if self.namespaces.is_empty() {
            return definitions;
        }
        let parts = EntryParts::new_in(access, context);
        for (namespace, entries) in std::mem::take(&mut self.namespaces) {
            let target = namespace
                .iter()
                .map(String::as_str)
                .chain(["meta", "journal"])
                .collect::<Vec<_>>()
                .join(".");
            let prior = path_resolved_in_definitions(
                access,
                &target,
                ResolvedExpr::Provided(access.duplicate_value(&definitions)),
            );
            let entries = entries
                .into_iter()
                .map(|entry| parts.entry_in(access, entry))
                .collect();
            let journal = apply_builtin_resolved(
                access,
                Builtin::Append,
                [
                    apply_builtin_resolved(
                        access,
                        Builtin::Default,
                        [ResolvedExpr::Provided(Value::List(List::empty())), prior],
                    ),
                    ResolvedExpr::Provided(Value::List(List::from_values(entries))),
                ],
            );
            definitions = lower_resolved_expr_in(
                access,
                update_module_resolved(
                    access,
                    ResolvedExpr::Provided(definitions),
                    &target,
                    journal,
                ),
            );
        }
        definitions
    }
}

/// What every entry written at once shares.
struct EntryParts {
    op: Key,
    at: Key,
    tags: [Key; 5],
    origin: Option<Value>,
}

impl EntryParts {
    fn new_in(access: &RuntimeValueAccess<'_>, context: &CompileContext) -> Self {
        Self {
            op: name_as_key("op"),
            at: name_as_key("at"),
            tags: [
                JournalOp::Introduce,
                JournalOp::Override,
                JournalOp::Update,
                JournalOp::Declare,
                JournalOp::Assume,
            ]
            .map(|op| name_as_key(op.tag())),
            origin: context.opaque_origin(access),
        }
    }

    fn entry_in(&self, access: &RuntimeValueAccess<'_>, entry: JournalEntry) -> Value {
        let path = entry
            .path
            .iter()
            .map(|key| key.to_value_in(access))
            .collect::<Vec<_>>();
        let tag = &self.tags[entry.op as usize];
        let op = Dict::new_sync().insert(tag.clone(), Value::List(List::from_values(path)));
        let mut location = Dict::new_sync().insert(
            (*keys::LINE).clone(),
            Value::Number(crate::number::Number::from_usize(entry.line)),
        );
        if let Some(origin) = &self.origin {
            location = location.insert((*keys::ORIGIN).clone(), access.duplicate_value(origin));
        }
        let at = Dict::new_sync().insert((*keys::G).clone(), Value::Dict(location));
        Value::Dict(
            Dict::new_sync()
                .insert(self.op.clone(), Value::Dict(op))
                .insert(self.at.clone(), Value::Dict(at)),
        )
    }
}

/// A journal of `entries`, each `(op, path, line)`, which a compiler writes
/// for what it provides a module, such as `env` on `import ... as`.
pub(in crate::g_syntax) fn seed_journal_in(
    access: &RuntimeValueAccess<'_>,
    context: &CompileContext,
    entries: &[(JournalOp, &str, usize)],
) -> Value {
    let parts = EntryParts::new_in(access, context);
    let entries = entries
        .iter()
        .map(|(op, path, line)| {
            parts.entry_in(
                access,
                JournalEntry {
                    op: *op,
                    path: path.split('.').map(name_as_key).collect(),
                    line: *line,
                },
            )
        })
        .collect();
    Value::List(List::from_values(entries))
}
