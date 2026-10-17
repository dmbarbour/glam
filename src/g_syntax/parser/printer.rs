//! Prints expression trees as source, with the fewest parentheses the
//! grammar reads back as the same tree: the round-trip test's printer, and a
//! start on the formatter. It prints on one line, with every block braced.
//!
//! What may stand where follows Syntax.md "Tail Position": a tail form (a
//! lambda, a prefix conditional, a `let` or a `using`) runs to the end of its
//! region, so it stands bare only where nothing follows it there. A form
//! that ends at its own `}` (a braced `do`, a match, an update) may be
//! followed by an operator but not by an argument or a path. An update's
//! base is the chain before its `with`, so it stands bare only where it
//! begins its chain.

use crate::g_syntax::{
    ConditionalMode, ConditionalResultMode, DefinitionKind, DoExpr, DoStepKind, IfExpr,
    MatchCommitment, MatchExpr, MatchOutcome, ObjectBodyDefinition, ObjectBodyDefinitionKind,
    SyntaxExpr, SyntaxGuardClause, SyntaxKeyExpr, SyntaxOperator, SyntaxPattern, SyntaxPatternKind,
};

use super::expression::{operand_reads_ungrouped, operator_symbol};

/// `expression` as source.
pub(super) fn print_expression(expression: &SyntaxExpr) -> String {
    let mut printer = Printer::default();
    printer.expression(expression, true);
    printer.out
}

#[derive(Default)]
struct Printer {
    out: String,
}

/// An expression's top infix operator, if it is an operator chain.
fn top_operator(expression: &SyntaxExpr) -> Option<SyntaxOperator> {
    use crate::core::Builtin;
    Some(match expression {
        SyntaxExpr::OperatorApply { operator, .. } => *operator,
        SyntaxExpr::ComparisonChain { rest, .. } => rest.first()?.0,
        SyntaxExpr::Add(..) => SyntaxOperator::Builtin(Builtin::Add),
        SyntaxExpr::Subtract(..) => SyntaxOperator::Builtin(Builtin::Subtract),
        SyntaxExpr::Multiply(..) => SyntaxOperator::Builtin(Builtin::Multiply),
        SyntaxExpr::Divide(..) => SyntaxOperator::Builtin(Builtin::Divide),
        SyntaxExpr::Append(..) => SyntaxOperator::Builtin(Builtin::Append),
        _ => return None,
    })
}

/// Whether a form runs to the end of its region.
fn is_tail_form(expression: &SyntaxExpr) -> bool {
    matches!(
        expression,
        SyntaxExpr::Lambda(..)
            | SyntaxExpr::If(_)
            | SyntaxExpr::Let { .. }
            | SyntaxExpr::Using { .. }
    )
}

/// Whether a form ends at its own `}`.
fn ends_with_block(expression: &SyntaxExpr) -> bool {
    matches!(
        expression,
        SyntaxExpr::Do(_) | SyntaxExpr::Match(_) | SyntaxExpr::With { .. }
    )
}

impl Printer {
    fn text(&mut self, text: &str) {
        self.out.push_str(text);
    }

    fn group(&mut self, expression: &SyntaxExpr) {
        self.text("(");
        self.expression(expression, true);
        self.text(")");
    }

    /// An expression that begins its chain; `end` says whether nothing
    /// follows it in its region.
    fn expression(&mut self, expression: &SyntaxExpr, end: bool) {
        if is_tail_form(expression) && !end {
            return self.group(expression);
        }
        if let Some(operator) = top_operator(expression) {
            return self.chain(expression, operator, end, true);
        }
        self.application(expression, end, true);
    }

    /// An operator chain whose top operator is `operator`. `leading` says
    /// whether it begins its chain, as an update's base must.
    fn chain(
        &mut self,
        expression: &SyntaxExpr,
        operator: SyntaxOperator,
        end: bool,
        leading: bool,
    ) {
        if let SyntaxExpr::ComparisonChain { first, rest } = expression {
            self.operand(first, operator, true, false, leading);
            let last = rest.len() - 1;
            for (at, (operator, operand)) in rest.iter().enumerate() {
                self.text(" ");
                self.text(operator_symbol(*operator));
                self.text(" ");
                self.operand(operand, *operator, false, end && at == last, false);
            }
            return;
        }
        let (left, right) = match expression {
            SyntaxExpr::OperatorApply { left, right, .. }
            | SyntaxExpr::Add(left, right)
            | SyntaxExpr::Subtract(left, right)
            | SyntaxExpr::Multiply(left, right)
            | SyntaxExpr::Divide(left, right)
            | SyntaxExpr::Append(left, right) => (left, right),
            _ => unreachable!("only operator chains have a top operator"),
        };
        self.operand(left, operator, true, false, leading);
        self.text(" ");
        self.text(operator_symbol(operator));
        self.text(" ");
        self.operand(right, operator, false, end, false);
    }

    /// An operand on `parent`'s `left` or right side.
    fn operand(
        &mut self,
        expression: &SyntaxExpr,
        parent: SyntaxOperator,
        left: bool,
        end: bool,
        leading: bool,
    ) {
        if let Some(child) = top_operator(expression) {
            return if operand_reads_ungrouped(parent, child, left) {
                self.chain(expression, child, end, leading)
            } else {
                self.group(expression)
            };
        }
        let bare = match expression {
            _ if is_tail_form(expression) => !left && end,
            SyntaxExpr::With { .. } => left && leading,
            _ => true,
        };
        if bare {
            self.application(expression, end && !left, leading);
        } else {
            self.group(expression);
        }
    }

    /// An application, or one of the forms that stand where it does. Only
    /// its last argument may be a tail form or end at a `}`, where nothing
    /// follows it.
    fn application(&mut self, expression: &SyntaxExpr, end: bool, leading: bool) {
        let mut arguments = Vec::new();
        let mut head = expression;
        while let SyntaxExpr::Apply(function, argument) = head {
            arguments.push(argument.as_ref());
            head = function;
        }
        if arguments.is_empty() {
            return self.form(expression, leading);
        }
        if is_tail_form(head) || ends_with_block(head) {
            self.group(head);
        } else {
            self.form(head, leading);
        }
        let last = arguments.len() - 1;
        for (at, argument) in arguments.into_iter().rev().enumerate() {
            self.text(" ");
            let final_tail = at == last && end;
            match argument {
                // A path-led argument is parenthesized.
                SyntaxExpr::Effect(_) => self.group(argument),
                _ if is_tail_form(argument) || ends_with_block(argument) => {
                    // After an operand an `if` reads as postfix, and a
                    // prefix conditional is an error.
                    if final_tail
                        && !matches!(argument, SyntaxExpr::With { .. } | SyntaxExpr::If(_))
                    {
                        self.form(argument, false);
                    } else {
                        self.group(argument);
                    }
                }
                _ => self.atom(argument),
            }
        }
    }

    /// An atom: one that is no form stands as it is, any other in
    /// parentheses.
    fn atom(&mut self, expression: &SyntaxExpr) {
        match expression {
            SyntaxExpr::Apply(..)
            | SyntaxExpr::Lambda(..)
            | SyntaxExpr::If(_)
            | SyntaxExpr::Let { .. }
            | SyntaxExpr::Using { .. }
            | SyntaxExpr::Do(_)
            | SyntaxExpr::Match(_)
            | SyntaxExpr::With { .. } => self.group(expression),
            _ if top_operator(expression).is_some() => self.group(expression),
            _ => self.form(expression, false),
        }
    }

    /// Any form, printed as it stands where its caller allowed it.
    fn form(&mut self, expression: &SyntaxExpr, leading: bool) {
        match expression {
            SyntaxExpr::Unit => self.text("()"),
            SyntaxExpr::Number(number) => self.text(&number.to_string()),
            SyntaxExpr::Text(text) => {
                self.text("\"");
                self.text(text);
                self.text("\"");
            }
            SyntaxExpr::Atom(name) => {
                self.text("'");
                self.text(name);
            }
            SyntaxExpr::Effect(path) => {
                for name in path {
                    self.text(".");
                    self.text(name);
                }
            }
            SyntaxExpr::Name(name) => self.text(name),
            SyntaxExpr::PriorName(name) => {
                self.text("_");
                self.text(name);
            }
            SyntaxExpr::Escape(levels, inner) => {
                self.text(&"^".repeat(*levels));
                self.atom(inner);
            }
            SyntaxExpr::Access(base, keys) => {
                self.atom(base);
                self.keys(keys, ".");
            }
            SyntaxExpr::PathDict(keys, payload) => {
                self.path(keys);
                self.text(":");
                match payload.as_ref() {
                    SyntaxExpr::Effect(_) => self.group(payload),
                    _ => self.atom(payload),
                }
            }
            SyntaxExpr::TaggedConstructor(keys) => {
                self.text(":");
                self.path(keys);
            }
            SyntaxExpr::DictUnion(items) => {
                self.text("{");
                for (at, item) in items.iter().enumerate() {
                    if at > 0 {
                        self.text(", ");
                    }
                    match item {
                        // A member's value runs to its comma.
                        SyntaxExpr::PathDict(keys, value) => {
                            self.path(keys);
                            self.text(": ");
                            self.expression(value, true);
                        }
                        _ => self.expression(item, true),
                    }
                }
                self.text("}");
            }
            SyntaxExpr::List(items) => {
                self.text("[");
                self.items(items);
                self.text("]");
            }
            SyntaxExpr::Tuple(items) => {
                self.text("(");
                self.items(items);
                self.text(")");
            }
            SyntaxExpr::OperatorSection {
                operator,
                left,
                right,
            } => {
                self.text("(");
                if let Some(left) = left {
                    self.atom(left);
                    self.text(" ");
                }
                self.text(operator_symbol(*operator));
                if let Some(right) = right {
                    self.text(" ");
                    self.atom(right);
                }
                self.text(")");
            }
            SyntaxExpr::Lambda(parameters, body) => {
                self.text("\\");
                self.text(&parameters.join(" "));
                self.text(" -> ");
                self.expression(body, true);
            }
            SyntaxExpr::If(conditional) => self.conditional(conditional),
            SyntaxExpr::Let { bindings, body } => {
                self.text("let ");
                self.bindings(bindings);
                self.text(" in ");
                self.expression(body, true);
            }
            SyntaxExpr::Using { namespace, body } => {
                // A `do` after the namespace begins the body.
                self.text("using ");
                match namespace.as_ref() {
                    SyntaxExpr::Do(_) => self.form(namespace, false),
                    _ => self.atom(namespace),
                }
                self.text(" in ");
                self.expression(body, true);
            }
            SyntaxExpr::Do(block) => self.do_block(block),
            SyntaxExpr::Match(found) => self.match_expression(found),
            SyntaxExpr::With { base, alias, body } => {
                // The base is all the chain before the `with`.
                if leading && !is_tail_form(base) {
                    self.expression(base, false);
                } else {
                    self.group(base);
                }
                if let Some(alias) = alias {
                    self.text(" as ");
                    self.text(alias);
                }
                self.text(" with {");
                for (at, member) in body.iter().enumerate() {
                    self.text(if at == 0 { " " } else { "; " });
                    self.member(member);
                }
                self.text(" }");
            }
            SyntaxExpr::Apply(..) => self.application(expression, true, leading),
            _ if top_operator(expression).is_some() => self.group(expression),
            other => unimplemented!("the printer does not print {other:?} yet"),
        }
    }

    fn items(&mut self, items: &[SyntaxExpr]) {
        for (at, item) in items.iter().enumerate() {
            if at > 0 {
                self.text(", ");
            }
            self.expression(item, true);
        }
    }

    fn path(&mut self, keys: &[SyntaxKeyExpr]) {
        for (at, key) in keys.iter().enumerate() {
            if at > 0 {
                self.text(".");
            }
            self.key(key);
        }
    }

    fn keys(&mut self, keys: &[SyntaxKeyExpr], separator: &str) {
        for key in keys {
            self.text(separator);
            self.key(key);
        }
    }

    fn key(&mut self, key: &SyntaxKeyExpr) {
        match key {
            SyntaxKeyExpr::Atom(name) => self.text(name),
            SyntaxKeyExpr::Index(index) => {
                self.text("[");
                self.expression(index, true);
                self.text("]");
            }
            SyntaxKeyExpr::PathIndex(index) => self.group(index),
        }
    }

    fn bindings(&mut self, bindings: &[(String, SyntaxExpr)]) {
        let braced = bindings.len() != 1;
        if braced {
            self.text("{");
        }
        for (at, (name, value)) in bindings.iter().enumerate() {
            self.text(if at == 0 {
                if braced { " " } else { "" }
            } else {
                "; "
            });
            self.text(name);
            self.text(" = ");
            self.expression(value, true);
        }
        if braced {
            self.text(" }");
        }
    }

    fn conditional(&mut self, conditional: &IfExpr) {
        self.text(match conditional.mode {
            ConditionalMode::Pure => "if ",
            ConditionalMode::Host => "try ",
        });
        self.guards(&conditional.guards);
        self.text(match conditional.then_mode {
            ConditionalResultMode::Ordinary => " then ",
            ConditionalResultMode::Tentative => " then? ",
        });
        self.expression(&conditional.then_result, true);
        self.text(" else ");
        self.expression(&conditional.else_result, true);
    }

    /// Guard clauses, an `and` chain: a clause whose own top operator is an
    /// `and` or `or` is parenthesized.
    fn guards(&mut self, guards: &[SyntaxGuardClause]) {
        let last = guards.len() - 1;
        for (at, guard) in guards.iter().enumerate() {
            if at > 0 {
                self.text(" and ");
            }
            match guard {
                SyntaxGuardClause::Pass => self.text("_"),
                SyntaxGuardClause::Effect(expression) => {
                    if matches!(
                        top_operator(expression),
                        Some(SyntaxOperator::BoolAnd | SyntaxOperator::BoolOr)
                    ) {
                        self.group(expression);
                    } else {
                        self.expression(expression, at == last);
                    }
                }
                SyntaxGuardClause::EffectBind { pattern, operation } => {
                    self.pattern(pattern);
                    self.text(" <- ");
                    self.expression(operation, at == last);
                }
                SyntaxGuardClause::ValueBind { pattern, value } => {
                    self.pattern(pattern);
                    self.text(" = ");
                    self.expression(value, at == last);
                }
            }
        }
    }

    fn pattern(&mut self, pattern: &SyntaxPattern) {
        match &pattern.kind {
            SyntaxPatternKind::Capture(name) => self.text(name),
            SyntaxPatternKind::Wildcard => self.text("_"),
            other => unimplemented!("the printer does not print the pattern {other:?} yet"),
        }
    }

    fn do_block(&mut self, block: &DoExpr) {
        self.text("do {");
        for step in &block.steps {
            self.text(" ");
            match &step.kind {
                DoStepKind::Bind { pattern, operation } => {
                    self.pattern(pattern);
                    self.text(" <- ");
                    self.expression(operation, true);
                }
                DoStepKind::ValueBind { pattern, value } => {
                    self.pattern(pattern);
                    self.text(" = ");
                    self.expression(value, true);
                }
                DoStepKind::Then(expression) => self.expression(expression, true),
                DoStepKind::Declare(names) => {
                    self.text("declare ");
                    self.text(&names.join(", "));
                }
            }
            self.text(";");
        }
        self.text(" ");
        self.expression(&block.result, true);
        self.text(" }");
    }

    fn match_expression(&mut self, found: &MatchExpr) {
        self.text(match found.mode {
            ConditionalMode::Pure => "match",
            ConditionalMode::Host => "try_match",
        });
        if found.commitment == MatchCommitment::Open {
            self.text("*");
        }
        self.text(" ");
        self.expression(&found.subject, true);
        self.text(" as {");
        for (at, arm) in found.arms.iter().enumerate() {
            self.text(if at == 0 { " " } else { "; " });
            self.pattern(&arm.pattern);
            if !arm.guards.is_empty() {
                self.text(" when ");
                self.guards(&arm.guards);
            }
            match &arm.outcome {
                MatchOutcome::Result {
                    mode, expression, ..
                } => {
                    self.text(match mode {
                        ConditionalResultMode::Ordinary => " => ",
                        ConditionalResultMode::Tentative => " =>? ",
                    });
                    self.expression(expression, true);
                }
                MatchOutcome::Nested(_) => {
                    unimplemented!("the printer does not print nested arms yet")
                }
            }
        }
        self.text(" }");
    }

    fn member(&mut self, member: &ObjectBodyDefinition) {
        let ObjectBodyDefinitionKind::Definition(definition) = &member.kind else {
            unimplemented!("the printer prints only definitions as members yet");
        };
        self.path(&definition.target);
        for parameter in &definition.parameters {
            self.text(" ");
            self.text(parameter);
        }
        self.text(match definition.kind {
            DefinitionKind::Introduce => " = ",
            DefinitionKind::Override => " := ",
            DefinitionKind::Update => " ::= ",
        });
        let value = definition
            .expr
            .as_ref()
            .expect("a printed definition has a value");
        // A definition with parameters holds them around its value.
        match value {
            SyntaxExpr::Lambda(parameters, body) if *parameters == definition.parameters => {
                self.expression(body, true);
            }
            _ => self.expression(value, true),
        }
    }
}

#[cfg(test)]
mod tests;
