//! Logical adapters for compatibility persistent collections.

use rpds::RedBlackTreeMapSync;

use super::CompatibilityValueEdges;
use crate::core::{Dict, Key, List, ListThunk, Value};
use crate::list::{LogicalListPart, LogicalListVisitStats};

/// Trace-work counters retained for I7's persistent-representation audit.
///
/// Counts are logical visits rather than unique physical nodes. Reusing one
/// persistent spine in two positions intentionally counts both traversals.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[allow(
    dead_code,
    reason = "I4D installs counters consumed by focused fixtures and the later I7 audit"
)]
pub(crate) struct PersistentEdgeVisitStats {
    pub(crate) map_entries: usize,
    pub(crate) key_nodes: usize,
    pub(crate) semantic_edges: usize,
    pub(crate) list: LogicalListVisitStats,
}

fn visit_map_entries<K: Ord, V>(
    map: &RedBlackTreeMapSync<K, V>,
    visit: &mut impl FnMut(&K, &V),
) -> usize {
    let mut entries = 0;
    for (key, value) in map.iter() {
        entries += 1;
        visit(key, value);
    }
    entries
}

/// Exhaustively walks the recursively structured, value-free key language.
///
/// This reports no semantic edge. Keeping the match explicit makes adding a
/// key representation capable of hiding a `Value` a compile-time I4/I7 audit
/// event rather than silently treating it as a leaf.
fn key_node_count(root: &Key) -> usize {
    let mut count = 0;
    let mut worklist = vec![root];

    while let Some(key) = worklist.pop() {
        count += 1;
        match key {
            Key::Atom(atom) => worklist.push(atom.key()),
            Key::Number(_) | Key::Binary(_) | Key::AbstractGlobalPath(_) => {}
            Key::List(items) => worklist.extend(items.iter().rev()),
            Key::Dict(entries) => {
                for (key, value) in entries.iter().rev() {
                    worklist.push(value);
                    worklist.push(key);
                }
            }
        }
    }

    count
}

pub(crate) fn visit_dict_edges(
    dict: &Dict,
    visit: &mut dyn FnMut(&Value),
) -> PersistentEdgeVisitStats {
    let mut stats = PersistentEdgeVisitStats::default();
    stats.map_entries = visit_map_entries(dict, &mut |key, value| {
        stats.key_nodes += key_node_count(key);
        stats.semantic_edges += 1;
        visit(value);
    });
    stats
}

pub(crate) fn visit_list_edges(
    list: &List,
    visit: &mut dyn FnMut(&Value),
) -> PersistentEdgeVisitStats {
    let mut stats = PersistentEdgeVisitStats::default();
    stats.list = list.visit_logical_parts(&mut |part| match part {
        LogicalListPart::Bytes => {}
        LogicalListPart::Values(values) => {
            stats.semantic_edges += values.len();
            for value in values {
                visit(value);
            }
        }
        LogicalListPart::Thunk(thunk) => {
            stats.semantic_edges += 1;
            let edge = match thunk {
                ListThunk::Lazy(lazy) => Value::Lazy(lazy.clone()),
                ListThunk::Promised(promise) => Value::Promised(promise.clone()),
            };
            visit(&edge);
        }
    });
    stats
}

impl CompatibilityValueEdges for Dict {
    fn visit_compatibility_value_edges(&self, visit: &mut dyn FnMut(&Value)) {
        let _ = visit_dict_edges(self, visit);
    }
}

impl CompatibilityValueEdges for List {
    fn visit_compatibility_value_edges(&self, visit: &mut dyn FnMut(&Value)) {
        let _ = visit_list_edges(self, visit);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use bytes::Bytes;
    use glam_gc::{Gc, Trace, Visitor};

    use super::*;
    use crate::core::{
        CoreValueFactory, EvaluationHalt, LazyValue, ManagedDropRecord, ManagedFamily,
        PromisedValue,
    };
    use crate::runtime::{RuntimeIds, allocate_evaluation_runtime_id};

    fn number(value: i64) -> Value {
        Value::Number(value.into())
    }

    fn edges(value: &impl CompatibilityValueEdges) -> Vec<Value> {
        let mut edges = Vec::new();
        value.visit_compatibility_value_edges(&mut |value| edges.push(value.clone()));
        edges
    }

    #[test]
    fn persistent_adapter_traces_empty_singleton_and_shared_spines() {
        let first = number(1);
        let second = number(2);

        let empty = List::empty();
        let mut empty_edges = Vec::new();
        let empty_stats = visit_list_edges(&empty, &mut |value| empty_edges.push(value.clone()));
        assert!(empty_edges.is_empty());
        assert_eq!(empty_stats.list.node_visits, 1);

        let singleton = List::from_values(vec![first.clone()]);
        assert_eq!(edges(&singleton), vec![first.clone()]);

        let shared = List::from_values(vec![first.clone(), second.clone()]);
        let shared_twice = List::concat(shared.clone(), shared);
        let mut shared_edges = Vec::new();
        let shared_stats =
            visit_list_edges(&shared_twice, &mut |value| shared_edges.push(value.clone()));
        assert_eq!(
            shared_edges,
            [first.clone(), second.clone(), first.clone(), second.clone()]
        );
        assert_eq!(shared_stats.list.node_visits, 3);
        assert_eq!(shared_stats.list.shared_value_slices, 2);
        assert_eq!(shared_stats.list.value_items, 4);
        assert_eq!(shared_stats.semantic_edges, 4);

        let finger = List::concat(
            List::from_bytes(Bytes::from_static(b"bytes")),
            List::from_values(vec![second.clone()]),
        )
        .balanced();
        let mut finger_edges = Vec::new();
        let finger_stats = visit_list_edges(&finger, &mut |value| finger_edges.push(value.clone()));
        assert_eq!(finger_edges, vec![second.clone()]);
        assert_eq!(finger_stats.list.node_visits, 1);
        assert_eq!(finger_stats.list.chunk_visits, 2);
        assert_eq!(finger_stats.list.byte_segments, 1);
        assert_eq!(finger_stats.list.shared_value_slices, 1);

        let forced = Arc::new(AtomicBool::new(false));
        let forced_by_thunk = forced.clone();
        let lazy = LazyValue::semantic_thunk(
            &crate::core::test_value_factory(),
            "persistent adapter thunk sentinel",
            move |_| -> Result<Value, EvaluationHalt> {
                forced_by_thunk.store(true, Ordering::Release);
                panic!("persistent collection tracing must not force a thunk")
            },
        );
        let thunk = List::from_thunk(ListThunk::Lazy(lazy.clone()));
        let mut thunk_edges = Vec::new();
        let thunk_stats = visit_list_edges(&thunk, &mut |value| thunk_edges.push(value.clone()));
        assert_eq!(thunk_edges, [Value::Lazy(lazy)]);
        assert_eq!(thunk_stats.list.thunk_items, 1);
        assert_eq!(thunk_stats.semantic_edges, 1);
        assert!(!forced.load(Ordering::Acquire));

        let promise = PromisedValue::new(
            &crate::core::test_value_factory(),
            "persistent adapter promise",
        );
        let promise_thunk = List::from_thunk(ListThunk::Promised(promise.clone()));
        assert_eq!(edges(&promise_thunk), [Value::Promised(promise)]);

        let nested_key = Key::Dict(Arc::from([(
            Key::List(Arc::from([Key::Number(3.into()), Key::Number(4.into())])),
            Key::Atom(crate::core::Atom::from_key(&Key::binary_from_text("label"))),
        )]));
        let base = Dict::new_sync().insert(nested_key, first.clone());
        let version = base
            .clone()
            .insert(Key::binary_from_text("second"), second.clone());
        let mut base_edges = Vec::new();
        let base_stats = visit_dict_edges(&base, &mut |value| base_edges.push(value.clone()));
        assert_eq!(base_edges, vec![first.clone()]);
        assert_eq!(base_stats.map_entries, 1);
        assert_eq!(base_stats.key_nodes, 6);
        assert_eq!(base_stats.semantic_edges, 1);
        assert_eq!(edges(&version).len(), 2);
        assert!(edges(&Dict::new_sync()).is_empty());
    }

    enum PersistentFixturePayload {
        Empty,
        List(crate::list::List<Gc<PersistentFixtureNode>, Gc<PersistentFixtureNode>>),
        Dict(RedBlackTreeMapSync<Key, Gc<PersistentFixtureNode>>),
    }

    struct PersistentFixtureNode {
        payload: Mutex<PersistentFixturePayload>,
        drops: Arc<AtomicUsize>,
    }

    impl PersistentFixtureNode {
        fn empty(drops: &Arc<AtomicUsize>) -> Self {
            Self {
                payload: Mutex::new(PersistentFixturePayload::Empty),
                drops: drops.clone(),
            }
        }
    }

    impl Drop for PersistentFixtureNode {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::Relaxed);
        }
    }

    // SAFETY: `payload` contains every managed edge represented by the
    // fixture. The list's logical-part walk and the RPDS iterator enumerate
    // those edges without forcing or mutating either collection. Exclusive
    // collection excludes the test's gateway-protected payload replacement.
    unsafe impl Trace for PersistentFixtureNode {
        fn trace(&self, visitor: &mut Visitor<'_>) {
            let payload = self
                .payload
                .lock()
                .expect("persistent fixture payload should not be poisoned");
            match &*payload {
                PersistentFixturePayload::Empty => {}
                PersistentFixturePayload::List(list) => {
                    list.visit_logical_parts(&mut |part| match part {
                        LogicalListPart::Bytes => {}
                        LogicalListPart::Values(values) => {
                            for edge in values {
                                visitor.visit(*edge);
                            }
                        }
                        LogicalListPart::Thunk(edge) => visitor.visit(*edge),
                    });
                }
                PersistentFixturePayload::Dict(dict) => {
                    visit_map_entries(dict, &mut |_, edge| visitor.visit(*edge));
                }
            }
        }
    }

    // SAFETY: direct Drop only updates an external atomic counter. The mutex,
    // persistent collection spines, key data, and inert `Gc` edges all destroy
    // passively without acquiring a Glam service or observing a dying edge.
    unsafe impl ManagedFamily for PersistentFixtureNode {
        const DROP_RECORD: ManagedDropRecord = ManagedDropRecord::passive(
            "I4D closed persistent adapter fixture",
            "src/core/managed/payload_edges/persistent.rs",
            "direct Drop updates only an external atomic counter",
            "collection spines, mutex, keys, and Gc edges drop passively",
        );
    }

    fn isolated_values() -> CoreValueFactory {
        CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new())
    }

    #[test]
    fn persistent_adapter_cycle_reclaims_in_isolated_heap() {
        let values = isolated_values();
        let baseline = values
            .collect_managed_for_test()
            .expect("canonical roots should collect before the persistent fixture");
        let drops = Arc::new(AtomicUsize::new(0));
        let root = values.with_managed_values(|scope| {
            let allocator = scope
                .allocator::<PersistentFixtureNode>()
                .expect("the persistent fixture layout should be supported");
            let list_node = allocator.alloc(PersistentFixtureNode::empty(&drops));
            let dict_node = allocator.alloc(PersistentFixtureNode::empty(&drops));

            // SAFETY: both pointers are live in this scope's exact heap. Each
            // closure performs one initially-empty to one-edge replacement.
            unsafe {
                let list_owner = scope.get_traced_edge(list_node);
                scope
                    .mutator
                    .with_edge_replacement(list_node, None, Some(dict_node), || {
                        *list_owner
                            .payload
                            .lock()
                            .expect("persistent fixture payload should not be poisoned") =
                            PersistentFixturePayload::List(crate::list::List::concat(
                                crate::list::List::from_bytes(Bytes::from_static(b"leaf")),
                                crate::list::List::from_thunk(dict_node),
                            ));
                    });

                let dict_owner = scope.get_traced_edge(dict_node);
                scope
                    .mutator
                    .with_edge_replacement(dict_node, None, Some(list_node), || {
                        *dict_owner
                            .payload
                            .lock()
                            .expect("persistent fixture payload should not be poisoned") =
                            PersistentFixturePayload::Dict(
                                RedBlackTreeMapSync::new_sync()
                                    .insert(Key::binary_from_text("backedge"), list_node),
                            );
                    });
            }

            scope.root(list_node)
        });

        let live = values
            .collect_managed_for_test()
            .expect("the rooted persistent cycle should collect");
        assert_eq!(live.marked_slots(), baseline.marked_slots() + 2);
        assert_eq!(drops.load(Ordering::Relaxed), 0);

        drop(root);
        let dead = values
            .collect_managed_for_test()
            .expect("the unrooted persistent cycle should be reclaimed");
        assert_eq!(dead.finalized_slots(), 2);
        assert_eq!(drops.load(Ordering::Relaxed), 2);
    }
}
