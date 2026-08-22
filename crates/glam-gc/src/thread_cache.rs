use std::cell::RefCell;
use std::collections::HashMap;
use std::num::NonZeroU64;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

use crate::{
    Trace,
    arena::{RunAddress, RunLocation},
    heap::{HeapInner, MutatorAdmission},
    run::{AllocationClassId, RunGeometry},
};

const CACHED_CLASS_SLOTS: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AllocationLeaseEpoch(NonZeroU64);

impl AllocationLeaseEpoch {
    pub(crate) const INITIAL: Self = Self(NonZeroU64::MIN);

    pub(crate) fn from_raw(raw: u64) -> Option<Self> {
        NonZeroU64::new(raw).map(Self)
    }

    pub(crate) fn get(self) -> u64 {
        self.0.get()
    }
}

/// One worker-local cursor over one exclusively leased allocation-bitmap word.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AllocationCursor {
    pub(crate) class_id: AllocationClassId,
    pub(crate) location: RunLocation,
    pub(crate) run: RunAddress,
    pub(crate) geometry: RunGeometry,
    pub(crate) word_index: usize,
    pub(crate) free_mask: u64,
}

impl AllocationCursor {
    fn try_allocate<T: Trace>(&mut self, value: T) -> Result<NonNull<T>, T> {
        self.try_allocate_with(value, || {})
    }

    fn try_allocate_with<T: Trace>(
        &mut self,
        value: T,
        before_initialize: impl FnOnce(),
    ) -> Result<NonNull<T>, T> {
        if self.free_mask == 0 {
            return Err(value);
        }

        let bit_index = self.free_mask.trailing_zeros() as usize;
        let slot_index = self.word_index * u64::BITS as usize + bit_index;
        let offset = self
            .geometry
            .slot_offset(slot_index)
            .expect("leased free bit must identify a valid slot");
        assert!(
            std::mem::size_of::<T>() <= self.geometry.slot_stride,
            "cached allocation payload exceeds its slot"
        );
        // SAFETY: the synchronized slow path validated the run and geometry,
        // then exclusively leased this allocation word to this thread. The
        // bounded free bit identifies uninitialized payload storage wholly
        // inside that stable run.
        let pointer = unsafe { self.run.pointer().add(offset).cast::<T>() };
        assert_eq!(
            pointer.addr().get() % std::mem::align_of::<T>(),
            0,
            "cached allocation slot has the wrong payload alignment"
        );
        let allocation_word = self.allocation_word_pointer();
        // SAFETY: this cursor exclusively owns writes to the initialized
        // atomic allocation word.
        let current = unsafe { allocation_word.as_ref() }.load(Ordering::Relaxed);
        let bit = 1_u64 << bit_index;
        assert_eq!(current & bit, 0, "cached allocation bit is already set");
        let published = current | bit;
        let remaining = self.free_mask & !bit;

        // This is the final point at which test instrumentation—or any future
        // fallible preparation—may unwind. The cursor and authoritative word
        // are unchanged, and `value` remains an ordinary owned Rust value.
        before_initialize();

        // No panicking operation is permitted between initialization and bit
        // publication. These writes target disjoint validated ranges owned by
        // this worker-local cursor.
        // SAFETY: the proof above establishes a unique initialized destination
        // for `T`.
        unsafe { pointer.write(value) };
        // SAFETY: the cursor exclusively owns writes to this initialized
        // bitmap word. Release publishes the initialized payload to concurrent
        // root validation and later collection.
        unsafe { allocation_word.as_ref() }.store(published, Ordering::Release);
        self.free_mask = remaining;
        Ok(pointer)
    }

    fn allocation_word_pointer(&self) -> NonNull<AtomicU64> {
        assert!(
            self.word_index < self.geometry.allocation_bitmap.word_len,
            "cached allocation word is out of range"
        );
        // SAFETY: validated geometry places this aligned atomic allocation
        // word wholly inside the stable leased run, and run publication
        // initialized every allocation word as `AtomicU64`.
        unsafe {
            self.run
                .pointer()
                .add(self.geometry.allocation_bitmap.offset)
                .cast::<AtomicU64>()
                .add(self.word_index)
        }
    }
}

struct ClassCursorCache {
    slots: Box<[Option<AllocationCursor>]>,
}

impl Default for ClassCursorCache {
    fn default() -> Self {
        Self {
            // Build the comparatively wide cache directly in heap storage.
            // Besides keeping ordinary mutator-entry stack use small, this
            // lets Loom's deliberately small coroutine stacks exercise the
            // entry surface without making cache width part of that model.
            slots: vec![None; CACHED_CLASS_SLOTS].into_boxed_slice(),
        }
    }
}

impl ClassCursorCache {
    fn clear(&mut self) {
        self.slots.fill(None);
    }

    fn get_mut(&mut self, class_id: AllocationClassId) -> Option<&mut AllocationCursor> {
        self.slots[cursor_slot(class_id)]
            .as_mut()
            .filter(|cursor| cursor.class_id == class_id)
    }

    #[cfg(test)]
    fn get(&self, class_id: AllocationClassId) -> Option<AllocationCursor> {
        self.slots[cursor_slot(class_id)].filter(|cursor| cursor.class_id == class_id)
    }

    fn insert(&mut self, cursor: AllocationCursor) {
        self.slots[cursor_slot(cursor.class_id)] = Some(cursor);
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.slots.iter().flatten().count()
    }
}

fn cursor_slot(class_id: AllocationClassId) -> usize {
    let dense_index = class_id.get() - 1;
    usize::try_from(dense_index % CACHED_CLASS_SLOTS as u64)
        .expect("bounded cursor slot always fits usize")
}

struct ThreadHeapState {
    heap: Weak<HeapInner>,
    recursive_depth: usize,
    captured_epoch: AllocationLeaseEpoch,
    cursors: ClassCursorCache,
}

impl ThreadHeapState {
    fn new(heap: &Arc<HeapInner>, captured_epoch: AllocationLeaseEpoch) -> Self {
        Self {
            heap: Arc::downgrade(heap),
            recursive_depth: 0,
            captured_epoch,
            cursors: ClassCursorCache::default(),
        }
    }

    fn begin_outer_entry(&mut self, current_epoch: AllocationLeaseEpoch) {
        debug_assert_eq!(self.recursive_depth, 0);
        if self.captured_epoch != current_epoch {
            // Stale records are inert. Forget them wholesale without reading a
            // run or attempting to return leases which collection revoked.
            self.cursors.clear();
            self.captured_epoch = current_epoch;
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct HeapCacheKey(usize);

impl HeapCacheKey {
    fn new(heap: &Arc<HeapInner>) -> Self {
        Self(Arc::as_ptr(heap) as usize)
    }
}

type SharedThreadHeapState = Rc<RefCell<ThreadHeapState>>;

thread_local! {
    static THREAD_HEAPS: RefCell<HashMap<HeapCacheKey, SharedThreadHeapState>> =
        RefCell::new(HashMap::new());
}

/// A hash-free handle from one active mutator region to its thread cache.
#[derive(Clone)]
pub(crate) struct ThreadCacheHandle {
    state: SharedThreadHeapState,
}

impl ThreadCacheHandle {
    pub(crate) fn try_allocate<T: Trace>(
        &self,
        class_id: AllocationClassId,
        value: T,
    ) -> Result<NonNull<T>, T> {
        let mut state = self.state.borrow_mut();
        assert_ne!(state.recursive_depth, 0, "mutator cache is not active");
        let Some(cursor) = state.cursors.get_mut(class_id) else {
            return Err(value);
        };
        cursor.try_allocate(value)
    }

    #[cfg(test)]
    pub(crate) fn try_allocate_with<T: Trace>(
        &self,
        class_id: AllocationClassId,
        value: T,
        before_initialize: impl FnOnce(),
    ) -> Result<NonNull<T>, T> {
        let mut state = self.state.borrow_mut();
        assert_ne!(state.recursive_depth, 0, "mutator cache is not active");
        let Some(cursor) = state.cursors.get_mut(class_id) else {
            return Err(value);
        };
        cursor.try_allocate_with(value, before_initialize)
    }

    pub(crate) fn install(&self, cursor: AllocationCursor) {
        let mut state = self.state.borrow_mut();
        assert_ne!(state.recursive_depth, 0, "mutator cache is not active");
        state.cursors.insert(cursor);
    }
}

/// A TLS entry found or created before coordinator admission.
///
/// Preparation never changes recursive depth or activates a cache, so a
/// blocked or panicking admission leaves no active thread-local state.
pub(crate) struct PreparedThreadHeapEntry {
    state: SharedThreadHeapState,
    outer: bool,
}

impl PreparedThreadHeapEntry {
    pub(crate) fn prepare(heap: &Arc<HeapInner>, epoch: AllocationLeaseEpoch) -> Self {
        let key = HeapCacheKey::new(heap);
        let state = THREAD_HEAPS.with_borrow_mut(|registry| {
            Rc::clone(
                registry
                    .entry(key)
                    .or_insert_with(|| Rc::new(RefCell::new(ThreadHeapState::new(heap, epoch)))),
            )
        });

        let outer = {
            let state = state.borrow();
            assert!(
                state.heap.ptr_eq(&Arc::downgrade(heap)),
                "thread heap-cache identity collision"
            );
            state.recursive_depth == 0
        };
        Self { state, outer }
    }

    pub(crate) fn is_outer(&self) -> bool {
        self.outer
    }

    pub(crate) fn activate<'heap>(
        self,
        epoch: AllocationLeaseEpoch,
        admission: Option<MutatorAdmission<'heap>>,
    ) -> ThreadHeapEntry<'heap> {
        assert_eq!(
            self.outer,
            admission.is_some(),
            "outer mutator preparation and coordinator admission disagree"
        );
        {
            let mut state = self.state.borrow_mut();
            if self.outer {
                state.begin_outer_entry(epoch);
            } else {
                debug_assert_eq!(
                    state.captured_epoch, epoch,
                    "allocation lease epoch changed inside a mutator region"
                );
            }
            state.recursive_depth = state
                .recursive_depth
                .checked_add(1)
                .expect("recursive mutator depth exhausted");
        }
        ThreadHeapEntry {
            state: self.state,
            outer_admission: admission,
            active: true,
        }
    }
}

/// Balances one activated same-thread heap entry, including recursive entries.
pub(crate) struct ThreadHeapEntry<'heap> {
    state: SharedThreadHeapState,
    outer_admission: Option<MutatorAdmission<'heap>>,
    active: bool,
}

impl<'heap> ThreadHeapEntry<'heap> {
    pub(crate) fn prepare(
        heap: &Arc<HeapInner>,
        epoch: AllocationLeaseEpoch,
    ) -> PreparedThreadHeapEntry {
        PreparedThreadHeapEntry::prepare(heap, epoch)
    }

    pub(crate) fn cache(&self) -> ThreadCacheHandle {
        ThreadCacheHandle {
            state: Rc::clone(&self.state),
        }
    }

    /// Deactivates an outer TLS entry while preserving its coordinator lease.
    ///
    /// The collector uses this to transfer its finalizer mutator obligation
    /// directly into the outer mutator entry which elected collection.
    pub(crate) fn into_outer_admission(mut self) -> MutatorAdmission<'heap> {
        let was_outer = self.deactivate();
        assert!(
            was_outer,
            "only an outer mutator admission can be transferred"
        );
        self.outer_admission
            .take()
            .expect("outer mutator entry must own one coordinator obligation")
    }

    fn deactivate(&mut self) -> bool {
        assert!(self.active, "mutator entry is already inactive");
        let mut state = self.state.borrow_mut();
        let prior_depth = state.recursive_depth;
        state.recursive_depth = prior_depth
            .checked_sub(1)
            .expect("mutator entry depth underflow");
        assert_eq!(
            self.outer_admission.is_some(),
            prior_depth == 1,
            "coordinator obligation does not match outer mutator exit"
        );
        self.active = false;
        prior_depth == 1
    }
}

pub(crate) fn release_current_thread_caches() -> usize {
    THREAD_HEAPS.with_borrow_mut(|registry| {
        // Validate the complete registry before mutation. A caller which is
        // still inside any mutator region gets a contract panic while every
        // cache record remains available for normal unwinding and exit.
        for state in registry.values() {
            assert_eq!(
                state.borrow().recursive_depth,
                0,
                "cannot release thread caches while a mutator is active"
            );
        }

        let released = registry.len();
        registry.clear();
        released
    })
}

pub(crate) fn thread_has_any_active_mutator() -> bool {
    THREAD_HEAPS.with_borrow(|registry| {
        registry
            .values()
            .any(|state| state.borrow().recursive_depth != 0)
    })
}

pub(crate) fn remove_inactive_thread_cache(heap: &Arc<HeapInner>) {
    let key = HeapCacheKey::new(heap);
    THREAD_HEAPS.with_borrow_mut(|registry| {
        let Some(state) = registry.get(&key) else {
            return;
        };
        assert_eq!(
            state.borrow().recursive_depth,
            0,
            "collector cannot remove an active thread cache"
        );
        registry.remove(&key);
    });
}

impl Drop for ThreadHeapEntry<'_> {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        self.deactivate();
        // Cache quiescence above must precede retirement of the coordinator
        // obligation, because zero active mutators admits exclusive work.
        drop(self.outer_admission.take());
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CacheSnapshot {
    pub(crate) recursive_depth: usize,
    pub(crate) captured_epoch: AllocationLeaseEpoch,
    pub(crate) cursor_count: usize,
}

#[cfg(test)]
fn state_for(heap: &Arc<HeapInner>) -> Option<SharedThreadHeapState> {
    let key = HeapCacheKey::new(heap);
    THREAD_HEAPS.with_borrow(|registry| registry.get(&key).map(Rc::clone))
}

#[cfg(test)]
pub(crate) fn cache_snapshot(heap: &Arc<HeapInner>) -> Option<CacheSnapshot> {
    let state = state_for(heap)?;
    let state = state.borrow();
    Some(CacheSnapshot {
        recursive_depth: state.recursive_depth,
        captured_epoch: state.captured_epoch,
        cursor_count: state.cursors.len(),
    })
}

#[cfg(test)]
pub(crate) fn insert_cursor(heap: &Arc<HeapInner>, cursor: AllocationCursor) {
    let state = state_for(heap).expect("test cursor insertion requires a heap-cache entry");
    let mut state = state.borrow_mut();
    assert_ne!(state.recursive_depth, 0);
    state.cursors.insert(cursor);
}

#[cfg(test)]
pub(crate) fn cursor(
    heap: &Arc<HeapInner>,
    class_id: AllocationClassId,
) -> Option<AllocationCursor> {
    state_for(heap)?.borrow().cursors.get(class_id)
}

#[cfg(test)]
pub(crate) fn registry_contains(heap_address: usize) -> bool {
    THREAD_HEAPS.with_borrow(|registry| registry.contains_key(&HeapCacheKey(heap_address)))
}
