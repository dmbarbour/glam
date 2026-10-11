//! Nets with no evaluation left, compiled to the values they build.
//!
//! A net with no active pair, copy, cursor obligation or callable checkpoint
//! (the test whole copies make) can still interact once a caller applies it
//! to arguments. When each such interaction only builds a value (an
//! operator meets data, a fan copies it, an eraser drops it, or an
//! application bind passes an operator its next operand), applying the net
//! amounts to applying its operators in data order, and nothing in it
//! waits on evaluation. Such a net compiles to those steps, which an
//! evaluator can run instead of copying the net (`perf-compiled-nets`).

use super::*;
use crate::trusted_hash::TrustedHashSet;

/// What applying a compiled net computes. Each step applies its operator to
/// its operands in order, which leaves the operator with nothing more to
/// take, so it yields data; the result is one operand.
pub(crate) struct CompiledNet<S: NetSpecialization> {
    inputs: usize,
    steps: Box<[CompiledStep<S>]>,
    result: CompiledOperand<S>,
}

pub(crate) struct CompiledStep<S: NetSpecialization> {
    pub(crate) operator: S::Operator,
    pub(crate) operands: Box<[CompiledOperand<S>]>,
}

/// A value a step or the result takes: one of the net's inputs, a constant
/// it holds, or an earlier step's result.
pub(crate) enum CompiledOperand<S: NetSpecialization> {
    Input(usize),
    Constant(S::Data),
    Step(usize),
}

/// Why a net did not compile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Uncompiled {
    /// It has evaluation left, so it may compile once that is done.
    Evaluating,
    /// It has none left, and it never will compile.
    Unsupported,
}

impl<S: NetSpecialization> CompiledNet<S> {
    /// How many arguments the net takes.
    pub(crate) fn inputs(&self) -> usize {
        self.inputs
    }

    pub(crate) fn steps(&self) -> &[CompiledStep<S>] {
        &self.steps
    }

    pub(crate) fn result(&self) -> &CompiledOperand<S> {
        &self.result
    }

    /// Visits every payload the compiled net holds.
    pub(crate) fn visit_payloads(&self, visit: &mut impl FnMut(RuntimeNetPayload<'_, S>)) {
        for step in &self.steps {
            visit(RuntimeNetPayload::Operator(&step.operator));
        }
        let operands = self.steps.iter().flat_map(|step| step.operands.iter());
        for operand in operands.chain([&self.result]) {
            if let CompiledOperand::Constant(data) = operand {
                visit(RuntimeNetPayload::Data(data));
            }
        }
    }
}

impl<S: NetSpecialization> RuntimeNet<S> {
    /// How many nodes the net holds, its interface included.
    pub(crate) fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// A ready active pair whose rule only builds: a rewrite, or an operator
    /// meeting data. Reducing it evaluates nothing, so normalizing a net
    /// before it is applied reduces these and no others: not a call of data,
    /// which needs the callable's value, nor a cursor or checkpoint pair.
    pub(crate) fn next_constructive_pair(&self) -> Option<ActivePairKey> {
        self.active.iter().find_map(|(&pair, state)| {
            if !state.is_ready() {
                return None;
            }
            let (left, right) = self.pair_nodes(pair)?;
            let constructive = match (self.node(left)?, self.node(right)?) {
                (
                    RuntimeNode::RemoteCursor { .. }
                    | RuntimeNode::CallableCheckpoint(_)
                    | RuntimeNode::Interface,
                    _,
                )
                | (
                    _,
                    RuntimeNode::RemoteCursor { .. }
                    | RuntimeNode::CallableCheckpoint(_)
                    | RuntimeNode::Interface,
                ) => false,
                (RuntimeNode::Erase, _)
                | (_, RuntimeNode::Erase)
                | (RuntimeNode::Fan { .. }, _)
                | (_, RuntimeNode::Fan { .. })
                | (RuntimeNode::Bind, RuntimeNode::Bind)
                | (RuntimeNode::Operator(_), RuntimeNode::Data(_))
                | (RuntimeNode::Data(_), RuntimeNode::Operator(_)) => true,
                _ => false,
            };
            constructive.then_some(pair)
        })
    }

    /// Compiles this net as a function of `inputs` arguments, if it has no
    /// evaluation left and applying it only builds values. `operands` gives
    /// how many more operands an operator takes before it yields data.
    pub(crate) fn compile(
        &self,
        inputs: usize,
        duplicator: &impl RuntimeNetPayloadDuplicator<S>,
        operands: impl Fn(&S::Operator) -> usize,
    ) -> Result<CompiledNet<S>, Uncompiled> {
        if !self.active.is_empty() || !self.copies.is_empty() || !self.cursor_obligations.is_empty()
        {
            return Err(Uncompiled::Evaluating);
        }
        if inputs == 0 || self.nodes.len() > whole_copy::WHOLE_COPY_MAX_NODES {
            return Err(Uncompiled::Unsupported);
        }
        let interface = self.exposed.ok_or(Uncompiled::Unsupported)?;

        // The function spine: one bind per argument, each facing the
        // interface or the bind before with its principal. A bind's second
        // auxiliary is its argument and its first faces what follows: the
        // next bind, or for the last, the body.
        let mut spine = Vec::with_capacity(inputs);
        let mut next = self.neighbor(interface);
        for _ in 0..inputs {
            let port = next.filter(|port| {
                port.index() == 0 && matches!(self.node(port.node()), Some(RuntimeNode::Bind))
            });
            let bind = port.ok_or(Uncompiled::Unsupported)?.node();
            spine.push(bind);
            next = self.neighbor(Port::auxiliary(bind, 1));
        }
        let body = next.ok_or(Uncompiled::Unsupported)?;

        let mut compiler = Compiler {
            net: self,
            duplicator,
            operands,
            spine,
            steps: Vec::new(),
            ends: TrustedHashMap::default(),
            reached: TrustedHashSet::default(),
        };
        let result = compiler
            .operand(body, self.nodes.len())
            .ok_or(Uncompiled::Unsupported)?;

        // Every node takes part in what the body computes, apart from the
        // interface and erasers of unused values: a step whose result is
        // dropped would be work the net does that the steps leave out.
        let Compiler {
            steps,
            spine,
            reached,
            ..
        } = compiler;
        let accounted = self.nodes.iter().all(|(id, entry)| {
            matches!(entry.node, RuntimeNode::Interface | RuntimeNode::Erase)
                || spine.contains(id)
                || reached.contains(id)
        });
        if !accounted {
            return Err(Uncompiled::Unsupported);
        }
        Ok(CompiledNet {
            inputs,
            steps: steps.into_boxed_slice(),
            result,
        })
    }
}

struct Compiler<'net, S: NetSpecialization, D, O> {
    net: &'net RuntimeNet<S>,
    duplicator: &'net D,
    operands: O,
    /// The function spine's binds, by argument.
    spine: Vec<NodeId>,
    steps: Vec<CompiledStep<S>>,
    /// Each compiled step, by the port its result leaves through.
    ends: TrustedHashMap<Port, usize>,
    /// The nodes the body's computation reaches, apart from the spine.
    reached: TrustedHashSet<NodeId>,
}

impl<S, D, O> Compiler<'_, S, D, O>
where
    S: NetSpecialization,
    D: RuntimeNetPayloadDuplicator<S>,
    O: Fn(&S::Operator) -> usize,
{
    /// The operand the providing port `port` gives, within `depth` more
    /// nodes, which bounds a cycle.
    fn operand(&mut self, port: Port, depth: usize) -> Option<CompiledOperand<S>> {
        let depth = depth.checked_sub(1)?;
        let node = port.node();
        match (self.net.node(node)?, port.index()) {
            (RuntimeNode::Bind, 2) => match self.spine.iter().position(|&bind| bind == node) {
                Some(input) => Some(CompiledOperand::Input(input)),
                None => self.step(port, depth).map(CompiledOperand::Step),
            },
            (RuntimeNode::Operator(_), 1) => self.step(port, depth).map(CompiledOperand::Step),
            // A fan copies the value its principal takes.
            (RuntimeNode::Fan { .. }, 1 | 2) => {
                self.reached.insert(node);
                let source = self.net.neighbor(Port::principal(node))?;
                self.operand(source, depth)
            }
            (RuntimeNode::Data(data), 0) => {
                self.reached.insert(node);
                Some(CompiledOperand::Constant(
                    self.duplicator.duplicate_data(data),
                ))
            }
            _ => None,
        }
    }

    /// The step whose result leaves through `end`: an operator's result, or
    /// the result of the last application bind passing it operands.
    fn step(&mut self, end: Port, depth: usize) -> Option<usize> {
        if let Some(&step) = self.ends.get(&end) {
            return Some(step);
        }
        // Walk back from the end to the operator, taking each bind's
        // operand on the way.
        let mut operands = Vec::new();
        let mut port = end;
        let mut depth = depth;
        let operator = loop {
            depth = depth.checked_sub(1)?;
            let node = port.node();
            self.reached.insert(node);
            match (self.net.node(node)?, port.index()) {
                (RuntimeNode::Operator(operator), 1) => {
                    let source = self.net.neighbor(Port::principal(node))?;
                    operands.push(self.operand(source, depth)?);
                    break operator;
                }
                (RuntimeNode::Bind, 2) if !self.spine.contains(&node) => {
                    let source = self.net.neighbor(Port::auxiliary(node, 1))?;
                    operands.push(self.operand(source, depth)?);
                    port = self.net.neighbor(Port::principal(node))?;
                }
                _ => return None,
            }
        };
        operands.reverse();
        if operands.len() != (self.operands)(operator) {
            return None;
        }
        let step = self.steps.len();
        self.steps.push(CompiledStep {
            operator: self.duplicator.duplicate_operator(operator),
            operands: operands.into_boxed_slice(),
        });
        self.ends.insert(end, step);
        Some(step)
    }
}
