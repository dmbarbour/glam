//! Direct lowering from resolved g-syntax expressions to closed interaction nets.

use std::collections::BTreeMap;
use std::sync::Arc;

#[cfg(test)]
use crate::core::CoreValueFactory;
use crate::core::{FunctionCode, FunctionValue, NetValue, RuntimeValueAccess, Value};
use crate::core_net::{CoreDataKey, CoreOperator, CoreSpecialization};
use crate::interaction_net::{NetBuilder, Port};

use super::resolved::{BindingId, ResolvedExpr, ResolvedPathPart};

/// Consumes one closed front-end semantic expression and lowers it directly to
/// a shared interaction-net computation. No syntax-shaped value survives this
/// boundary.
#[cfg(test)]
pub(super) fn lower_resolved_expr(values: &CoreValueFactory, expr: ResolvedExpr<Value>) -> Value {
    values.with_runtime_value_access(|access| lower_resolved_expr_in(&access, expr))
}

pub(super) fn lower_resolved_expr_in(
    access: &RuntimeValueAccess<'_>,
    expr: ResolvedExpr<Value>,
) -> Value {
    match expr {
        ResolvedExpr::Embedded(value) | ResolvedExpr::Provided(value) => value,
        expr => {
            let (code, captures) = ResolvedNetLowerer::lower_code_in(access, Vec::new(), expr);
            assert!(
                captures.is_empty(),
                "a value leaving g-syntax must be a closed interaction net"
            );
            Value::Lazy(crate::core::LazyValue::from_net_computation_in(
                access,
                NetValue::new(code.duplicate_runtime_in(access)),
            ))
        }
    }
}

pub(super) struct ResolvedNetLowerer<'access, 'scope> {
    values: &'access RuntimeValueAccess<'scope>,
    net: NetBuilder<CoreSpecialization>,
    local_uses: BTreeMap<BindingId, Vec<Port>>,
}

impl<'access, 'scope> ResolvedNetLowerer<'access, 'scope> {
    #[cfg(test)]
    pub(super) fn lower_code(
        values: &CoreValueFactory,
        parameters: Vec<BindingId>,
        body: ResolvedExpr<Value>,
    ) -> (FunctionCode, Vec<BindingId>) {
        values.with_runtime_value_access(|access| {
            ResolvedNetLowerer::lower_code_in(&access, parameters, body)
        })
    }

    /// Lowers `body` as code over `parameters`, returning the code and its
    /// captures: the locals `body` uses but does not bind, in binding order.
    /// They are the code's first inputs, before its parameters.
    pub(super) fn lower_code_in(
        values: &'access RuntimeValueAccess<'scope>,
        parameters: Vec<BindingId>,
        body: ResolvedExpr<Value>,
    ) -> (FunctionCode, Vec<BindingId>) {
        let (lowerer, body) = Self::lower_body_in(values, body);
        // Every local use the body left unbound is recorded, a nested
        // closure's captures among them, so these are exactly its captures.
        let captures = lowerer
            .local_uses
            .keys()
            .filter(|binding| !parameters.contains(binding))
            .copied()
            .collect::<Vec<_>>();
        let mut inputs = captures.clone();
        inputs.extend(parameters.iter().copied());
        let template = lowerer.finish_template(inputs, body);
        let runtime = values
            .construct_managed_core_net(template.instantiate_with(values))
            .expect("managed core-net representation must fit one collector run");
        (
            FunctionCode::new(runtime, parameters.len(), captures.len()),
            captures,
        )
    }

    #[cfg(test)]
    pub(super) fn lower_template(
        values: &CoreValueFactory,
        inputs: Vec<BindingId>,
        body: ResolvedExpr<Value>,
    ) -> crate::core_net::CoreInteractionNet {
        values.with_runtime_value_access(|access| {
            ResolvedNetLowerer::lower_template_in(&access, inputs, body)
        })
    }

    #[cfg(test)]
    fn lower_template_in(
        values: &'access RuntimeValueAccess<'scope>,
        inputs: Vec<BindingId>,
        body: ResolvedExpr<Value>,
    ) -> crate::core_net::CoreInteractionNet {
        let (lowerer, body) = Self::lower_body_in(values, body);
        lowerer.finish_template(inputs, body)
    }

    /// Lowers `body` into a new net, recording its local uses. Returns the
    /// port that yields the body's value.
    fn lower_body_in(
        values: &'access RuntimeValueAccess<'scope>,
        body: ResolvedExpr<Value>,
    ) -> (Self, Port) {
        let mut lowerer = Self {
            values,
            net: NetBuilder::new(),
            local_uses: BTreeMap::new(),
        };
        let body_boundary = lowerer.net.copy(1);
        lowerer.compile_into(body, body_boundary.outputs[0]);
        (lowerer, body_boundary.input)
    }

    /// Binds a lowered body's local uses to `inputs` and closes its net.
    fn finish_template(
        mut self,
        inputs: Vec<BindingId>,
        body: Port,
    ) -> crate::core_net::CoreInteractionNet {
        if inputs.is_empty() {
            assert!(
                self.local_uses.is_empty(),
                "closed net body contains an unbound local"
            );
            return self.net.finish(body);
        }

        let binds = self.net.function_spine(inputs.len());
        self.net.wire(binds.result, body);
        for (binding, source) in inputs.into_iter().zip(binds.arguments) {
            let targets = self.local_uses.remove(&binding).unwrap_or_default();
            self.distribute(source, &targets);
        }
        assert!(
            self.local_uses.is_empty(),
            "lowered function body contains an unbound local"
        );
        self.net.finish(binds.input)
    }

    fn compile_into(&mut self, expr: ResolvedExpr<Value>, target: Port) {
        match expr {
            ResolvedExpr::Embedded(value) | ResolvedExpr::Provided(value) => {
                self.data_into(value, target);
            }
            ResolvedExpr::Local(binding) => {
                self.local_uses.entry(binding).or_default().push(target)
            }
            ResolvedExpr::List(items) => self.list_into(items, target),
            ResolvedExpr::Access { base, path } => {
                let mut arguments = vec![*base];
                let path = path
                    .into_iter()
                    .map(|part| match part {
                        ResolvedPathPart::Key(key) => CoreDataKey::Key(key),
                        ResolvedPathPart::Index(expr) => {
                            arguments.push(*expr);
                            CoreDataKey::Index
                        }
                        ResolvedPathPart::PathIndex(expr) => {
                            arguments.push(*expr);
                            CoreDataKey::PathIndex
                        }
                    })
                    .collect::<Vec<_>>();
                self.operator_application_into(
                    crate::eval::access_operator(self.values, Arc::from(path), Arc::from([])),
                    arguments,
                    target,
                );
            }
            ResolvedExpr::Lambda { parameters, body } => {
                self.function_into(parameters, *body, target);
            }
            ResolvedExpr::Apply {
                function,
                arguments,
            } => self.semantic_application_into(*function, arguments, target),
            ResolvedExpr::ApplyLambda {
                parameters,
                body,
                arguments,
            } => {
                // Preserve the grouped redex as one lowering operation. The
                // function template is emitted once and the complete argument
                // spine is attached without intermediate host expressions.
                self.semantic_application_into(
                    ResolvedExpr::Lambda { parameters, body },
                    arguments,
                    target,
                );
            }
        }
    }

    /// Lowers a list literal by composition: a run of closed items of at
    /// least [`LIST_OPERATOR_MAX_ARITY`] becomes one list value, the other
    /// items lists of at most that many, and the parts join with `++` in the
    /// shape of a finger tree (see [`ListTree::finger`]). Each step then
    /// finds its pair within the tree's depth plus one part, and copies at
    /// most one part's items.
    ///
    /// [`LIST_OPERATOR_MAX_ARITY`]: crate::eval::LIST_OPERATOR_MAX_ARITY
    fn list_into(&mut self, items: Vec<ResolvedExpr<Value>>, target: Port) {
        let parts = ListParts::split(self.values, items)
            .into_iter()
            .map(ListTree::Part)
            .collect();
        match ListTree::finger(parts) {
            Some(tree) => self.list_tree_into(tree, target),
            None => self.data_into(Value::List(crate::core::List::empty()), target),
        }
    }

    fn list_tree_into(&mut self, tree: ListTree, target: Port) {
        match tree {
            ListTree::Part(ListPart::Closed(values)) => {
                self.data_into(Value::List(crate::core::List::from_values(values)), target)
            }
            ListTree::Part(ListPart::Items(items)) => self.lazy_operator_application_into(
                crate::eval::list_operator(self.values, items.len(), Arc::from([])),
                items,
                target,
            ),
            ListTree::Append(left, right) => {
                let mut output = self.net.unary_operator(crate::eval::builtin_operator(
                    self.values,
                    crate::core::BuiltinCall {
                        builtin: crate::core::Builtin::Append,
                        arguments: Arc::from([]),
                    },
                ));
                for operand in [*left, *right] {
                    let [application, argument, result] = self.net.bind();
                    self.net.wire(output, application);
                    self.list_tree_into(operand, argument);
                    output = result;
                }
                self.net.wire(output, target);
            }
        }
    }

    fn function_into(
        &mut self,
        parameters: Vec<BindingId>,
        body: ResolvedExpr<Value>,
        target: Port,
    ) {
        assert!(!parameters.is_empty(), "a function must bind an argument");
        let (code, captures) = Self::lower_code_in(self.values, parameters, body);
        let code = Arc::new(code);
        if captures.is_empty() {
            self.data_into(
                Value::Function(FunctionValue::new(
                    NetValue::new(code.duplicate_runtime_in(self.values)),
                    code.arity(),
                )),
                target,
            );
        } else {
            let operator = crate::eval::function_capture_operator(self.values, code, Arc::from([]));
            self.binding_operator_application_into(operator, captures, target);
        }
    }

    fn semantic_application_into(
        &mut self,
        function: ResolvedExpr<Value>,
        arguments: Vec<ResolvedExpr<Value>>,
        target: Port,
    ) {
        let mut output = self.net.unary_operator(crate::eval::apply_arity_operator(
            self.values,
            arguments.len(),
            Arc::from([]),
        ));
        let [application, function_port, result] = self.net.bind();
        self.net.wire(output, application);
        self.compile_into(function, function_port);
        output = result;
        for argument in arguments {
            let [application, argument_port, result] = self.net.bind();
            self.net.wire(output, application);
            self.compile_lazy_into(argument, argument_port);
            output = result;
        }
        self.net.wire(output, target);
    }

    fn operator_application_into(
        &mut self,
        operator: CoreOperator,
        arguments: Vec<ResolvedExpr<Value>>,
        target: Port,
    ) {
        let mut output = self.net.unary_operator(operator);
        for argument in arguments {
            let [application, argument_port, result] = self.net.bind();
            self.net.wire(output, application);
            self.compile_into(argument, argument_port);
            output = result;
        }
        self.net.wire(output, target);
    }

    fn lazy_operator_application_into(
        &mut self,
        operator: CoreOperator,
        arguments: Vec<ResolvedExpr<Value>>,
        target: Port,
    ) {
        let mut output = self.net.unary_operator(operator);
        for argument in arguments {
            let [application, argument_port, result] = self.net.bind();
            self.net.wire(output, application);
            self.compile_lazy_into(argument, argument_port);
            output = result;
        }
        self.net.wire(output, target);
    }

    fn binding_operator_application_into(
        &mut self,
        operator: CoreOperator,
        bindings: Vec<BindingId>,
        target: Port,
    ) {
        let mut output = self.net.unary_operator(operator);
        for binding in bindings {
            let [application, argument_port, result] = self.net.bind();
            self.net.wire(output, application);
            self.local_uses
                .entry(binding)
                .or_default()
                .push(argument_port);
            output = result;
        }
        self.net.wire(output, target);
    }

    fn compile_lazy_into(&mut self, expr: ResolvedExpr<Value>, target: Port) {
        match expr {
            ResolvedExpr::Embedded(value) | ResolvedExpr::Provided(value) => {
                self.data_into(value, target);
            }
            expr => {
                let (code, captures) = Self::lower_code_in(self.values, Vec::new(), expr);
                let code = Arc::new(code);
                if captures.is_empty() {
                    self.data_into(
                        Value::Lazy(crate::core::LazyValue::from_net_computation_in(
                            self.values,
                            NetValue::new(code.duplicate_runtime_in(self.values)),
                        )),
                        target,
                    );
                } else {
                    let operator =
                        crate::eval::computation_capture_operator(self.values, code, Arc::from([]));
                    self.binding_operator_application_into(operator, captures, target);
                }
            }
        }
    }

    fn data_into(&mut self, data: Value, target: Port) {
        let data = self.net.data(data);
        self.net.wire(data, target);
    }

    fn distribute(&mut self, source: Port, targets: &[Port]) {
        let copy = self.net.copy(targets.len());
        self.net.wire(source, copy.input);
        for (output, target) in copy.outputs.into_iter().zip(targets) {
            self.net.wire(output, *target);
        }
    }
}

/// How a composed list literal joins its parts.
enum ListTree {
    Part(ListPart),
    Append(Box<ListTree>, Box<ListTree>),
}

impl ListTree {
    fn append(left: Self, right: Self) -> Self {
        Self::Append(Box::new(left), Box::new(right))
    }

    /// Joins `elements` in order in the shape of a finger tree: the first
    /// and last stand at the ends, `first ++ (middle ++ last)`, and the
    /// middle is built the same way from pairs of the elements between
    /// them. Element size doubles at each level, so both ends stay shallow
    /// and every part lies within a depth logarithmic in their number.
    fn finger(mut elements: Vec<Self>) -> Option<Self> {
        if elements.len() <= 3 {
            let last = elements.pop()?;
            return Some(
                elements
                    .into_iter()
                    .rev()
                    .fold(last, |joined, element| Self::append(element, joined)),
            );
        }
        let last = elements.pop().expect("more than three elements");
        let mut elements = elements.into_iter();
        let first = elements.next().expect("more than three elements");
        let mut pairs = Vec::new();
        while let Some(left) = elements.next() {
            pairs.push(match elements.next() {
                Some(right) => Self::append(left, right),
                None => left,
            });
        }
        let middle = Self::finger(pairs).expect("at least two middle elements");
        Some(Self::append(first, Self::append(middle, last)))
    }
}

/// One part of a composed list literal.
enum ListPart {
    /// A run of closed items, built as one list value during lowering.
    Closed(Vec<Value>),
    /// At most [`crate::eval::LIST_OPERATOR_MAX_ARITY`] items, collected by
    /// one list operator.
    Items(Vec<ResolvedExpr<Value>>),
}

/// Splits a list literal's items into parts, in order: runs of at least
/// [`crate::eval::LIST_OPERATOR_MAX_ARITY`] closed items, and the remaining
/// items in groups of at most that many.
struct ListParts<'access, 'scope> {
    _values: &'access RuntimeValueAccess<'scope>,
    parts: Vec<ListPart>,
    /// Items not yet in a part.
    group: Vec<ResolvedExpr<Value>>,
    /// The closed items ending the literal so far.
    run: Vec<Value>,
}

impl<'access, 'scope> ListParts<'access, 'scope> {
    const LIMIT: usize = crate::eval::LIST_OPERATOR_MAX_ARITY;

    fn split(
        values: &'access RuntimeValueAccess<'scope>,
        items: Vec<ResolvedExpr<Value>>,
    ) -> Vec<ListPart> {
        let mut split = Self {
            _values: values,
            parts: Vec::new(),
            group: Vec::new(),
            run: Vec::new(),
        };
        for item in items {
            match item {
                ResolvedExpr::Embedded(value) | ResolvedExpr::Provided(value) => {
                    split.run.push(value)
                }
                item => {
                    split.end_closed_run();
                    split.group.push(item);
                }
            }
        }
        split.end_closed_run();
        split.flush_group();
        split.parts
    }

    /// Ends a run of closed items: a long one becomes its own part, after
    /// the items before it; a short one joins them.
    fn end_closed_run(&mut self) {
        if self.run.len() >= Self::LIMIT {
            self.flush_group();
            self.parts
                .push(ListPart::Closed(std::mem::take(&mut self.run)));
        } else {
            self.group
                .extend(self.run.drain(..).map(ResolvedExpr::Embedded));
        }
    }

    /// Moves the pending items into parts of at most [`Self::LIMIT`] items.
    fn flush_group(&mut self) {
        let mut items = std::mem::take(&mut self.group).into_iter().peekable();
        while items.peek().is_some() {
            self.parts
                .push(ListPart::Items(items.by_ref().take(Self::LIMIT).collect()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numbered(count: usize) -> Vec<ListTree> {
        (0..count)
            .map(|index| {
                ListTree::Part(ListPart::Closed(vec![Value::Number((index as i64).into())]))
            })
            .collect()
    }

    fn leaves(tree: &ListTree, depth: usize, found: &mut Vec<(i64, usize)>) {
        match tree {
            ListTree::Part(ListPart::Closed(values)) => {
                let Value::Number(number) = &values[0] else {
                    panic!("test parts hold numbers");
                };
                found.push((number.to_i64_if_integer().expect("an integer"), depth));
            }
            ListTree::Part(ListPart::Items(_)) => panic!("test parts are closed"),
            ListTree::Append(left, right) => {
                leaves(left, depth + 1, found);
                leaves(right, depth + 1, found);
            }
        }
    }

    /// A finger-shaped join keeps its parts in order, every part within a
    /// depth logarithmic in their number, and both ends near the root.
    #[test]
    fn finger_joins_keep_order_and_logarithmic_depth() {
        assert!(ListTree::finger(Vec::new()).is_none());
        for count in [1, 2, 3, 4, 5, 6, 7, 10, 33, 100, 1_000, 4_096] {
            let tree = ListTree::finger(numbered(count)).expect("a nonempty join");
            let mut found = Vec::new();
            leaves(&tree, 0, &mut found);
            let order = found.iter().map(|(index, _)| *index).collect::<Vec<_>>();
            assert_eq!(order, (0..count as i64).collect::<Vec<_>>());
            let deepest = found.iter().map(|(_, depth)| *depth).max().unwrap();
            let levels = usize::BITS - count.leading_zeros();
            assert!(
                deepest <= 3 * levels as usize,
                "{count} parts reach depth {deepest}"
            );
            if count >= 4 {
                assert_eq!(found[0].1, 1, "the first part stands beside the root");
                assert_eq!(found[count - 1].1, 2, "the last part is two from the root");
            }
        }
    }
}
