//! Ephemerons and weak references (`opaque-resources-reclaimed-when-unreachable`).
//!
//! An ephemeron pairs a key with a value and keeps the value alive only
//! while the key is reachable by some other path. It does not keep its key
//! alive. A collection first marks from the registered roots, then marks
//! the value of each live ephemeron whose key it marked, tracing from those
//! values, and repeats until a round marks nothing new: a value may reach
//! another ephemeron's key. An ephemeron whose key stays unmarked is then
//! cleared, before the sweep reclaims anything. Clearing is silent: an owner
//! notices a cleared ephemeron when it next uses it, or when it next traces
//! what holds it. A weak reference is an ephemeron whose key and value
//! coincide.
//!
//! An ephemeron is a slot in its heap's table: a reference count and the
//! key's and value's addresses. Handles share the slot as roots share
//! theirs, by its count. A slot is reused only once its count is zero, by the
//! next collection's rebuild of the free list, and nothing creates
//! ephemerons during a collection.

use std::fmt;
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

use crate::root_pool::HeapId;
use crate::trace::ErasedGc;
use crate::{GcRef, MutatorAccess, Trace};

const SLOTS_PER_CHUNK: usize = 64;

/// The most references one slot counts, as for roots.
const MAX_REFERENCES: usize = isize::MAX as usize;

/// One ephemeron: a reference count and the key's and value's addresses,
/// null once cleared.
struct EphemeronSlot {
    references: AtomicUsize,
    key: AtomicPtr<()>,
    value: AtomicPtr<()>,
}

impl EphemeronSlot {
    const fn free() -> Self {
        Self {
            references: AtomicUsize::new(0),
            key: AtomicPtr::new(std::ptr::null_mut()),
            value: AtomicPtr::new(std::ptr::null_mut()),
        }
    }
}

/// A chunk of slots of one heap, shared by the table and the handles of its
/// slots, so a handle outlives its heap safely: it can then only be dropped.
struct EphemeronChunk {
    heap: HeapId,
    slots: Box<[EphemeronSlot]>,
}

/// A heap's ephemerons, under its data mutex.
#[derive(Default)]
pub(crate) struct EphemeronTable {
    chunks: Vec<Arc<EphemeronChunk>>,
    /// Slots no handle references, by chunk and index, as the last
    /// collection found them, and the slots of chunks allocated since.
    free: Vec<(usize, usize)>,
}

/// A live ephemeron's addresses, for marking.
#[derive(Clone, Copy)]
pub(crate) struct EphemeronEntry {
    pub(crate) key: ErasedGc,
    pub(crate) value: ErasedGc,
    chunk: usize,
    index: usize,
}

impl EphemeronTable {
    /// Occupies a free slot with `key` and `value`, which the caller proved
    /// live, allocated and of their types in `heap`, under that heap's
    /// admission, so no collection runs meanwhile.
    pub(crate) fn occupy<K: Trace, V: Trace>(
        &mut self,
        heap: HeapId,
        key: ErasedGc,
        value: ErasedGc,
    ) -> Ephemeron<K, V> {
        let (chunk, index) = self.free.pop().unwrap_or_else(|| {
            let chunk = self.chunks.len();
            self.chunks.push(Arc::new(EphemeronChunk {
                heap,
                slots: (0..SLOTS_PER_CHUNK)
                    .map(|_| EphemeronSlot::free())
                    .collect(),
            }));
            self.free
                .extend((1..SLOTS_PER_CHUNK).rev().map(|index| (chunk, index)));
            (chunk, 0)
        });
        let chunk = Arc::clone(&self.chunks[chunk]);
        let slot = &chunk.slots[index];
        debug_assert_eq!(slot.references.load(Ordering::Relaxed), 0);
        // Relaxed suffices for the words: a collection or another thread reads
        // them only after synchronizing with this admission or with the
        // transfer of the handle.
        slot.key.store(key.as_ptr().as_ptr(), Ordering::Relaxed);
        slot.value.store(value.as_ptr().as_ptr(), Ordering::Relaxed);
        slot.references.store(1, Ordering::Relaxed);
        Ephemeron {
            chunk,
            index,
            marker: PhantomData,
        }
    }

    /// The ephemerons that handles still reference and no collection has
    /// cleared. The caller holds exclusive collection.
    pub(crate) fn live_entries(&self) -> Vec<EphemeronEntry> {
        let mut entries = Vec::new();
        for (chunk_index, chunk) in self.chunks.iter().enumerate() {
            for (index, slot) in chunk.slots.iter().enumerate() {
                if slot.references.load(Ordering::Acquire) == 0 {
                    continue;
                }
                let (Some(key), Some(value)) = (
                    NonNull::new(slot.key.load(Ordering::Acquire)),
                    NonNull::new(slot.value.load(Ordering::Acquire)),
                ) else {
                    continue;
                };
                entries.push(EphemeronEntry {
                    key: ErasedGc::new(key),
                    value: ErasedGc::new(value),
                    chunk: chunk_index,
                    index,
                });
            }
        }
        entries
    }

    /// Clears `entry`, whose key the collection left unmarked. The caller
    /// holds exclusive collection, before any sweep.
    pub(crate) fn clear(&self, entry: EphemeronEntry) {
        let slot = &self.chunks[entry.chunk].slots[entry.index];
        slot.key.store(std::ptr::null_mut(), Ordering::Release);
        slot.value.store(std::ptr::null_mut(), Ordering::Release);
    }

    /// Rebuilds the free list from the slots no handle references. The
    /// caller holds exclusive collection: no handle is cloned from a slot
    /// whose count is zero, and nothing occupies a slot meanwhile.
    pub(crate) fn rebuild_free_list(&mut self) {
        self.free.clear();
        for (chunk_index, chunk) in self.chunks.iter().enumerate().rev() {
            for (index, slot) in chunk.slots.iter().enumerate().rev() {
                if slot.references.load(Ordering::Acquire) == 0 {
                    slot.key.store(std::ptr::null_mut(), Ordering::Relaxed);
                    slot.value.store(std::ptr::null_mut(), Ordering::Relaxed);
                    self.free.push((chunk_index, index));
                }
            }
        }
    }

    /// How many slots handles reference, cleared or not.
    #[cfg(test)]
    pub(crate) fn referenced_count(&self) -> usize {
        self.chunks
            .iter()
            .flat_map(|chunk| chunk.slots.iter())
            .filter(|slot| slot.references.load(Ordering::Acquire) != 0)
            .count()
    }
}

/// A shareable ephemeron in a live heap: its value lives while its key is
/// reachable by some other path, and it keeps neither alive by itself.
///
/// Once a collection finds the key unreachable, the ephemeron is cleared,
/// silently: its owner notices when it next views it. Like a root, it may
/// outlive its heap, and can then only be cloned and dropped.
#[must_use = "an ephemeron keeps nothing alive"]
pub struct Ephemeron<K: Trace, V: Trace> {
    chunk: Arc<EphemeronChunk>,
    index: usize,
    marker: PhantomData<fn() -> (K, V)>,
}

impl<K: Trace, V: Trace> Ephemeron<K, V> {
    fn slot(&self) -> &EphemeronSlot {
        &self.chunk.slots[self.index]
    }

    /// Views the key and value while `mutator` stays borrowed, or `None`
    /// once a collection found the key unreachable.
    ///
    /// Panics if `mutator` belongs to another heap.
    pub fn get<'m>(&self, mutator: &'m MutatorAccess<'_>) -> Option<(GcRef<'m, K>, GcRef<'m, V>)> {
        assert!(
            self.chunk.heap == mutator.heap().id(),
            "ephemeron does not belong to this heap"
        );
        let slot = self.slot();
        let key = NonNull::new(slot.key.load(Ordering::Acquire))?;
        let value = NonNull::new(slot.value.load(Ordering::Acquire))
            .expect("an ephemeron with a key holds its value");
        // SAFETY: the slot was occupied from views proving a live `K` and a
        // live `V` in this heap, and is not reused while this handle
        // references it. Every collection since either marked the key, and
        // then marked the value from it, so both survived its sweep, or left
        // the key unmarked and cleared the slot before sweeping. The key is
        // not cleared, so both are still live, and the borrowed admission
        // excludes collection while the views exist.
        unsafe {
            Some((
                GcRef::new(key.cast::<K>(), mutator),
                GcRef::new(value.cast::<V>(), mutator),
            ))
        }
    }

    /// Whether a collection has found the key unreachable.
    #[must_use]
    pub fn is_cleared(&self) -> bool {
        self.slot().key.load(Ordering::Acquire).is_null()
    }
}

impl<K: Trace, V: Trace> Clone for Ephemeron<K, V> {
    fn clone(&self) -> Self {
        // Relaxed suffices, as for `Arc::clone`: the new reference comes from
        // an existing one.
        let prior = self.slot().references.fetch_add(1, Ordering::Relaxed);
        if prior >= MAX_REFERENCES {
            std::process::abort();
        }
        Self {
            chunk: Arc::clone(&self.chunk),
            index: self.index,
            marker: PhantomData,
        }
    }
}

impl<K: Trace, V: Trace> Drop for Ephemeron<K, V> {
    fn drop(&mut self) {
        self.slot().references.fetch_sub(1, Ordering::Release);
    }
}

impl<K: Trace, V: Trace> fmt::Debug for Ephemeron<K, V> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Ephemeron")
            .field("cleared", &self.is_cleared())
            .finish_non_exhaustive()
    }
}

/// A weak reference: an ephemeron whose key is its value, which it views
/// while some other path reaches it.
#[must_use = "a weak reference keeps nothing alive"]
pub struct Weak<T: Trace>(Ephemeron<T, T>);

impl<T: Trace> Weak<T> {
    pub(crate) fn new(ephemeron: Ephemeron<T, T>) -> Self {
        Self(ephemeron)
    }

    /// Views the value while `mutator` stays borrowed, or `None` once a
    /// collection found it unreachable.
    ///
    /// Panics if `mutator` belongs to another heap.
    pub fn get<'m>(&self, mutator: &'m MutatorAccess<'_>) -> Option<GcRef<'m, T>> {
        self.0.get(mutator).map(|(_, value)| value)
    }

    /// Whether a collection has found the value unreachable.
    #[must_use]
    pub fn is_cleared(&self) -> bool {
        self.0.is_cleared()
    }
}

impl<T: Trace> Clone for Weak<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T: Trace> fmt::Debug for Weak<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Weak")
            .field("cleared", &self.is_cleared())
            .finish_non_exhaustive()
    }
}
