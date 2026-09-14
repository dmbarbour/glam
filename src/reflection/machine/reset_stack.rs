//! Resumable decoding of the reset frames serialized in reflection state.
//!
//! This owner is deliberately separate from pure outer-WHNF evaluation. It
//! composes the shared WHNF, logical-list-front, and key-conversion machines,
//! retaining only runtime roots across poll boundaries.

use crate::core::{Key, Value};
use crate::eval::{ConversionPoll, KeyConversionMachine, ListFrontMachine, ListFrontPoll};
use crate::evaluation::{
    EvalContext, EvaluationPollContext, WhnfOwnerPoll, WorkDependency, poll_whnf_computation,
};
use crate::runtime::RuntimeValueRoot;

use super::{ResetFrame, TaskHalt, WhnfComputation};

pub(super) struct DecodedResetStack {
    pub(super) serialized: RuntimeValueRoot,
    pub(super) frames: Vec<ResetFrame>,
}

pub(super) struct ResetStackMachine {
    serialized: RuntimeValueRoot,
    frames: Vec<ResetFrame>,
    state: ResetStackState,
}

enum ResetStackState {
    StackWhnf(WhnfComputation),
    Frames(ListFrontMachine),
    Frame {
        remaining: ListFrontMachine,
        frame: Box<ResetFrameMachine>,
    },
    Complete,
    Poisoned,
}

struct ResetFrameMachine {
    state: ResetFrameState,
}

enum ResetFrameState {
    FrameWhnf(WhnfComputation),
    Fields {
        front: ListFrontMachine,
        fields: Vec<RuntimeValueRoot>,
    },
    Key {
        conversion: KeyConversionMachine,
        continuation: RuntimeValueRoot,
        scope_depth: RuntimeValueRoot,
        order: RuntimeValueRoot,
    },
    Scope {
        key: Key,
        continuation: RuntimeValueRoot,
        demand: WhnfComputation,
        order: RuntimeValueRoot,
    },
    Order {
        key: Key,
        continuation: RuntimeValueRoot,
        scope_depth: usize,
        demand: WhnfComputation,
    },
    Complete,
    Poisoned,
}

pub(super) enum ResetStackPoll {
    Ready(DecodedResetStack),
    Continue,
    Pending(WorkDependency),
    Yielded,
    Failed(TaskHalt),
}

enum ResetFramePoll {
    Ready(ResetFrame),
    Continue,
    Pending(WorkDependency),
    Yielded,
    Failed(TaskHalt),
}

impl ResetStackMachine {
    pub(super) fn new(serialized: RuntimeValueRoot) -> Self {
        Self {
            frames: Vec::new(),
            state: ResetStackState::StackWhnf(WhnfComputation::from_root(serialized.clone())),
            serialized,
        }
    }

    pub(super) fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvalContext,
        step_budget: usize,
    ) -> ResetStackPoll {
        let state = std::mem::replace(&mut self.state, ResetStackState::Poisoned);
        match state {
            ResetStackState::StackWhnf(mut demand) => {
                match poll_whnf_computation(&mut demand, poll_context, context, step_budget.max(1))
                {
                    WhnfOwnerPoll::Ready(stack) => {
                        let is_list = poll_context.evaluate(context, |evaluator| {
                            matches!(evaluator.project_root(&stack), Value::List(_))
                        });
                        if !is_list {
                            self.state = ResetStackState::StackWhnf(demand);
                            return ResetStackPoll::Failed(TaskHalt::new(
                                "reflection continuation state must be a list",
                            ));
                        }
                        self.state = ResetStackState::Frames(ListFrontMachine::unowned(stack));
                        ResetStackPoll::Continue
                    }
                    WhnfOwnerPoll::Pending(dependency) => {
                        self.state = ResetStackState::StackWhnf(demand);
                        ResetStackPoll::Pending(dependency)
                    }
                    WhnfOwnerPoll::Yielded => {
                        self.state = ResetStackState::StackWhnf(demand);
                        ResetStackPoll::Yielded
                    }
                    WhnfOwnerPoll::Failed(failure) => {
                        self.state = ResetStackState::StackWhnf(demand);
                        ResetStackPoll::Failed(TaskHalt::rooted_failure(failure))
                    }
                    WhnfOwnerPoll::External(boundary) => {
                        self.state = ResetStackState::StackWhnf(demand);
                        ResetStackPoll::Failed(TaskHalt::new(format!(
                            "reflection reset stack reached an unsupported {boundary:?} boundary"
                        )))
                    }
                }
            }
            ResetStackState::Frames(mut front) => {
                match poll_list_front(&mut front, poll_context, context, step_budget) {
                    ListFrontPoll::Ready(Some((frame, tail))) => {
                        self.state = ResetStackState::Frame {
                            remaining: ListFrontMachine::unowned(tail),
                            frame: Box::new(ResetFrameMachine::new(frame)),
                        };
                        ResetStackPoll::Continue
                    }
                    ListFrontPoll::Ready(None) => {
                        self.state = ResetStackState::Complete;
                        ResetStackPoll::Ready(DecodedResetStack {
                            serialized: self.serialized.clone(),
                            frames: std::mem::take(&mut self.frames),
                        })
                    }
                    ListFrontPoll::Pending(dependency) => {
                        self.state = ResetStackState::Frames(front);
                        ResetStackPoll::Pending(dependency)
                    }
                    ListFrontPoll::Yielded => {
                        self.state = ResetStackState::Frames(front);
                        ResetStackPoll::Yielded
                    }
                    ListFrontPoll::Failed(failure) => {
                        self.state = ResetStackState::Frames(front);
                        ResetStackPoll::Failed(TaskHalt::rooted_failure(failure))
                    }
                }
            }
            ResetStackState::Frame {
                remaining,
                mut frame,
            } => match frame.poll(poll_context, context, step_budget) {
                ResetFramePoll::Ready(decoded) => {
                    self.frames.push(decoded);
                    self.state = ResetStackState::Frames(remaining);
                    ResetStackPoll::Continue
                }
                ResetFramePoll::Continue => {
                    self.state = ResetStackState::Frame { remaining, frame };
                    ResetStackPoll::Continue
                }
                ResetFramePoll::Pending(dependency) => {
                    self.state = ResetStackState::Frame { remaining, frame };
                    ResetStackPoll::Pending(dependency)
                }
                ResetFramePoll::Yielded => {
                    self.state = ResetStackState::Frame { remaining, frame };
                    ResetStackPoll::Yielded
                }
                ResetFramePoll::Failed(error) => {
                    self.state = ResetStackState::Frame { remaining, frame };
                    ResetStackPoll::Failed(error)
                }
            },
            ResetStackState::Complete => {
                self.state = ResetStackState::Complete;
                panic!("reset-stack decoder was polled after completion")
            }
            ResetStackState::Poisoned => {
                panic!("reset-stack decoder was polled after losing its state")
            }
        }
    }
}

impl ResetFrameMachine {
    fn new(frame: RuntimeValueRoot) -> Self {
        Self {
            state: ResetFrameState::FrameWhnf(WhnfComputation::from_root(frame)),
        }
    }

    fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvalContext,
        step_budget: usize,
    ) -> ResetFramePoll {
        let state = std::mem::replace(&mut self.state, ResetFrameState::Poisoned);
        match state {
            ResetFrameState::FrameWhnf(mut demand) => {
                match poll_whnf_computation(&mut demand, poll_context, context, step_budget.max(1))
                {
                    WhnfOwnerPoll::Ready(frame) => {
                        let is_list = poll_context.evaluate(context, |evaluator| {
                            matches!(evaluator.project_root(&frame), Value::List(_))
                        });
                        if !is_list {
                            self.state = ResetFrameState::FrameWhnf(demand);
                            return ResetFramePoll::Failed(TaskHalt::new(
                                "reflection continuation frame must be a list",
                            ));
                        }
                        self.state = ResetFrameState::Fields {
                            front: ListFrontMachine::unowned(frame),
                            fields: Vec::with_capacity(4),
                        };
                        ResetFramePoll::Continue
                    }
                    WhnfOwnerPoll::Pending(dependency) => {
                        self.state = ResetFrameState::FrameWhnf(demand);
                        ResetFramePoll::Pending(dependency)
                    }
                    WhnfOwnerPoll::Yielded => {
                        self.state = ResetFrameState::FrameWhnf(demand);
                        ResetFramePoll::Yielded
                    }
                    WhnfOwnerPoll::Failed(failure) => {
                        self.state = ResetFrameState::FrameWhnf(demand);
                        ResetFramePoll::Failed(TaskHalt::rooted_failure(failure))
                    }
                    WhnfOwnerPoll::External(boundary) => {
                        self.state = ResetFrameState::FrameWhnf(demand);
                        ResetFramePoll::Failed(TaskHalt::new(format!(
                            "reflection continuation frame reached an unsupported {boundary:?} boundary"
                        )))
                    }
                }
            }
            ResetFrameState::Fields {
                mut front,
                mut fields,
            } => match poll_list_front(&mut front, poll_context, context, step_budget) {
                ListFrontPoll::Ready(Some((field, tail))) if fields.len() < 4 => {
                    fields.push(field);
                    self.state = ResetFrameState::Fields {
                        front: ListFrontMachine::unowned(tail),
                        fields,
                    };
                    ResetFramePoll::Continue
                }
                ListFrontPoll::Ready(Some(_)) => {
                    self.state = ResetFrameState::Fields { front, fields };
                    ResetFramePoll::Failed(TaskHalt::new(
                        "reflection continuation frame has the wrong size",
                    ))
                }
                ListFrontPoll::Ready(None) if fields.len() != 4 => {
                    self.state = ResetFrameState::Fields { front, fields };
                    ResetFramePoll::Failed(TaskHalt::new(
                        "reflection continuation frame has the wrong size",
                    ))
                }
                ListFrontPoll::Ready(None) => {
                    let [key, continuation, scope_depth, order] = fields
                        .try_into()
                        .expect("validated reset frame must contain four fields");
                    self.state = ResetFrameState::Key {
                        conversion: KeyConversionMachine::new(key, None),
                        continuation,
                        scope_depth,
                        order,
                    };
                    ResetFramePoll::Continue
                }
                ListFrontPoll::Pending(dependency) => {
                    self.state = ResetFrameState::Fields { front, fields };
                    ResetFramePoll::Pending(dependency)
                }
                ListFrontPoll::Yielded => {
                    self.state = ResetFrameState::Fields { front, fields };
                    ResetFramePoll::Yielded
                }
                ListFrontPoll::Failed(failure) => {
                    self.state = ResetFrameState::Fields { front, fields };
                    ResetFramePoll::Failed(TaskHalt::rooted_failure(failure))
                }
            },
            ResetFrameState::Key {
                mut conversion,
                continuation,
                scope_depth,
                order,
            } => {
                let poll = poll_context.evaluate(context, |evaluator| {
                    conversion.poll(poll_context, evaluator, context, step_budget.max(1))
                });
                match poll {
                    ConversionPoll::Ready(key) => {
                        self.state = ResetFrameState::Scope {
                            key,
                            continuation,
                            demand: WhnfComputation::from_root(scope_depth),
                            order,
                        };
                        ResetFramePoll::Continue
                    }
                    ConversionPoll::Pending(dependency) => {
                        self.state = ResetFrameState::Key {
                            conversion,
                            continuation,
                            scope_depth,
                            order,
                        };
                        ResetFramePoll::Pending(dependency)
                    }
                    ConversionPoll::Yielded => {
                        self.state = ResetFrameState::Key {
                            conversion,
                            continuation,
                            scope_depth,
                            order,
                        };
                        ResetFramePoll::Yielded
                    }
                    ConversionPoll::Failed(failure) => {
                        self.state = ResetFrameState::Key {
                            conversion,
                            continuation,
                            scope_depth,
                            order,
                        };
                        ResetFramePoll::Failed(TaskHalt::rooted_failure(failure))
                    }
                }
            }
            ResetFrameState::Scope {
                key,
                continuation,
                mut demand,
                order,
            } => match poll_usize(&mut demand, poll_context, context, step_budget, "scope") {
                UsizePoll::Ready(scope_depth) => {
                    self.state = ResetFrameState::Order {
                        key,
                        continuation,
                        scope_depth,
                        demand: WhnfComputation::from_root(order),
                    };
                    ResetFramePoll::Continue
                }
                UsizePoll::Pending(dependency) => {
                    self.state = ResetFrameState::Scope {
                        key,
                        continuation,
                        demand,
                        order,
                    };
                    ResetFramePoll::Pending(dependency)
                }
                UsizePoll::Yielded => {
                    self.state = ResetFrameState::Scope {
                        key,
                        continuation,
                        demand,
                        order,
                    };
                    ResetFramePoll::Yielded
                }
                UsizePoll::Failed(error) => {
                    self.state = ResetFrameState::Scope {
                        key,
                        continuation,
                        demand,
                        order,
                    };
                    ResetFramePoll::Failed(error)
                }
            },
            ResetFrameState::Order {
                key,
                continuation,
                scope_depth,
                mut demand,
            } => match poll_usize(&mut demand, poll_context, context, step_budget, "order") {
                UsizePoll::Ready(order) => {
                    self.state = ResetFrameState::Complete;
                    ResetFramePoll::Ready(ResetFrame {
                        key,
                        continuation,
                        scope_depth,
                        order,
                    })
                }
                UsizePoll::Pending(dependency) => {
                    self.state = ResetFrameState::Order {
                        key,
                        continuation,
                        scope_depth,
                        demand,
                    };
                    ResetFramePoll::Pending(dependency)
                }
                UsizePoll::Yielded => {
                    self.state = ResetFrameState::Order {
                        key,
                        continuation,
                        scope_depth,
                        demand,
                    };
                    ResetFramePoll::Yielded
                }
                UsizePoll::Failed(error) => {
                    self.state = ResetFrameState::Order {
                        key,
                        continuation,
                        scope_depth,
                        demand,
                    };
                    ResetFramePoll::Failed(error)
                }
            },
            ResetFrameState::Complete => {
                self.state = ResetFrameState::Complete;
                panic!("reset-frame decoder was polled after completion")
            }
            ResetFrameState::Poisoned => {
                panic!("reset-frame decoder was polled after losing its state")
            }
        }
    }
}

enum UsizePoll {
    Ready(usize),
    Pending(WorkDependency),
    Yielded,
    Failed(TaskHalt),
}

fn poll_usize(
    demand: &mut WhnfComputation,
    poll_context: &EvaluationPollContext,
    context: &EvalContext,
    step_budget: usize,
    field: &str,
) -> UsizePoll {
    let value = match poll_whnf_computation(demand, poll_context, context, step_budget.max(1)) {
        WhnfOwnerPoll::Ready(value) => value,
        WhnfOwnerPoll::Pending(dependency) => return UsizePoll::Pending(dependency),
        WhnfOwnerPoll::Yielded => return UsizePoll::Yielded,
        WhnfOwnerPoll::Failed(failure) => {
            return UsizePoll::Failed(TaskHalt::rooted_failure(failure));
        }
        WhnfOwnerPoll::External(boundary) => {
            return UsizePoll::Failed(TaskHalt::new(format!(
                "reflection continuation frame {field} reached an unsupported {boundary:?} boundary"
            )));
        }
    };
    poll_context.evaluate(context, |evaluator| {
        let Value::Number(number) = evaluator.project_root(&value) else {
            return UsizePoll::Failed(TaskHalt::new(format!(
                "reflection continuation frame has an invalid {field}"
            )));
        };
        number.to_usize_if_integer().map_or_else(
            || {
                UsizePoll::Failed(TaskHalt::new(format!(
                    "reflection continuation frame has an invalid {field}"
                )))
            },
            UsizePoll::Ready,
        )
    })
}

fn poll_list_front(
    front: &mut ListFrontMachine,
    poll_context: &EvaluationPollContext,
    context: &EvalContext,
    step_budget: usize,
) -> ListFrontPoll {
    poll_context.evaluate(context, |evaluator| {
        front.poll(poll_context, evaluator, context, step_budget.max(1))
    })
}

#[cfg(test)]
pub(super) fn assert_machine_shape(machine: &ResetStackMachine) {
    let ResetStackMachine {
        serialized,
        frames,
        state,
    } = machine;
    let _: &RuntimeValueRoot = serialized;
    let _: &Vec<ResetFrame> = frames;
    match state {
        ResetStackState::StackWhnf(demand) => {
            let _: &WhnfComputation = demand;
        }
        ResetStackState::Frames(front) => {
            let _: &ListFrontMachine = front;
        }
        ResetStackState::Frame { remaining, frame } => {
            let _: &ListFrontMachine = remaining;
            assert_frame_shape(frame);
        }
        ResetStackState::Complete | ResetStackState::Poisoned => {}
    }
}

#[cfg(test)]
fn assert_frame_shape(frame: &ResetFrameMachine) {
    match &frame.state {
        ResetFrameState::FrameWhnf(demand) => {
            let _: &WhnfComputation = demand;
        }
        ResetFrameState::Fields { front, fields } => {
            let _: &ListFrontMachine = front;
            let _: &Vec<RuntimeValueRoot> = fields;
        }
        ResetFrameState::Key {
            conversion,
            continuation,
            scope_depth,
            order,
        } => {
            let _: &KeyConversionMachine = conversion;
            let _ = (continuation, scope_depth, order);
        }
        ResetFrameState::Scope {
            key,
            continuation,
            demand,
            order,
        } => {
            let _: &WhnfComputation = demand;
            let _ = (key, continuation, order);
        }
        ResetFrameState::Order {
            key,
            continuation,
            scope_depth,
            demand,
        } => {
            let _: &WhnfComputation = demand;
            let _ = (key, continuation, scope_depth);
        }
        ResetFrameState::Complete | ResetFrameState::Poisoned => {}
    }
}
