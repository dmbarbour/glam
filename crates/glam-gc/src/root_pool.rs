//! Pooled root slots (`pooled-roots`).
//!
//! A root is a slot in one of its heap's root chunks: a reference count and
//! one word. While the count is nonzero the word holds the rooted value;
//! while the slot is free it links to the next free slot. A [`crate::Root`]
//! is a pointer to its slot, so cloning or dropping one touches only that
//! count, and creating one takes a slot from the creating thread's private
//! free list without a lock or a shared atomic.
//!
//! Nobody creates roots during a collection, so each collection rebuilds
//! every chunk's free list from its zero-count slots, just before it publishes
//! the next allocation lease epoch, which discards the free lists threads
//! took earlier. A count never rises from zero: only a live root can be
//! cloned. A heap dropped while roots remain orphans their chunks to a
//! process-wide list, which later heap drops sweep.

use std::cell::UnsafeCell;
use std::num::NonZeroU64;
use std::ptr::NonNull;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use crate::trace::ErasedGc;

/// A heap's identity, never reused within the process.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct HeapId(NonZeroU64);

impl HeapId {
    pub(crate) fn fresh() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        Self(NonZeroU64::new(id).expect("heap identities exhausted"))
    }
}

const CHUNK_BYTES: usize = 4096;
const SLOTS_PER_CHUNK: usize = CHUNK_BYTES / size_of::<RootSlot>() - 1;

/// The most references one slot counts. As for `Arc`, a clone beyond it
/// aborts rather than risk the count wrapping to zero.
const MAX_REFERENCES: usize = isize::MAX as usize;

/// One root: a reference count and one word.
#[repr(C)]
pub(crate) struct RootSlot {
    references: AtomicUsize,
    /// The rooted value while `references` is nonzero; the next free slot,
    /// or null, while the slot is free.
    word: UnsafeCell<*mut ()>,
}

impl RootSlot {
    const fn free() -> Self {
        Self {
            references: AtomicUsize::new(0),
            word: UnsafeCell::new(std::ptr::null_mut()),
        }
    }

    /// Roots `value` in a free slot, with one reference.
    ///
    /// # Safety
    ///
    /// `slot` must be a free slot the caller took from a free list it owns
    /// exclusively, in a chunk of the heap that holds `value`, and the caller
    /// must hold that heap's admission.
    pub(crate) unsafe fn occupy(slot: NonNull<Self>, value: ErasedGc) {
        // SAFETY: the caller owns this free slot exclusively, so nothing else
        // reads or writes its word, and the chunk is live.
        unsafe {
            debug_assert_eq!(slot.as_ref().references.load(Ordering::Relaxed), 0);
            *slot.as_ref().word.get() = value.as_ptr().as_ptr();
            // Relaxed suffices: a collection or another thread reads the word
            // only after synchronizing with this admission or with the
            // transfer of the root.
            slot.as_ref().references.store(1, Ordering::Relaxed);
        }
    }

    /// The next slot on a free list that the caller owns.
    ///
    /// # Safety
    ///
    /// `slot` must be on a free list the caller owns exclusively, in a live
    /// chunk.
    pub(crate) unsafe fn next_free(slot: NonNull<Self>) -> Option<NonNull<Self>> {
        // SAFETY: the caller owns the free list, so nothing writes this word.
        NonNull::new(unsafe { *slot.as_ref().word.get() }.cast::<Self>())
    }

    /// The value a live slot roots.
    ///
    /// # Safety
    ///
    /// The caller must hold a reference to `slot`.
    pub(crate) unsafe fn value(slot: NonNull<Self>) -> ErasedGc {
        // SAFETY: a referenced slot is live, its chunk stays allocated, and
        // its word is written only while it is free.
        let word = unsafe { *slot.as_ref().word.get() };
        ErasedGc::new(NonNull::new(word).expect("a live root slot holds its value"))
    }

    /// Adds a reference to a live slot.
    ///
    /// # Safety
    ///
    /// The caller must hold a reference to `slot`.
    pub(crate) unsafe fn retain(slot: NonNull<Self>) {
        // SAFETY: the caller's reference keeps the slot live and its chunk
        // allocated. Relaxed suffices, as for `Arc::clone`: the new
        // reference comes from an existing one.
        let prior = unsafe { slot.as_ref() }
            .references
            .fetch_add(1, Ordering::Relaxed);
        if prior > MAX_REFERENCES {
            std::process::abort();
        }
    }

    /// Drops one reference. This is the caller's last access to the slot.
    ///
    /// # Safety
    ///
    /// The caller must hold a reference to `slot`, which it gives up.
    pub(crate) unsafe fn release(slot: NonNull<Self>) {
        // SAFETY: the caller's reference keeps the chunk allocated until this
        // decrement. Release orders the caller's reads of the word before a
        // collection or sweep that observes the count at zero with Acquire
        // and reuses or frees the slot.
        let prior = unsafe { slot.as_ref() }
            .references
            .fetch_sub(1, Ordering::Release);
        debug_assert_ne!(prior, 0, "root slot reference count underflow");
    }

    /// The identity of the heap whose chunk holds `slot`.
    ///
    /// # Safety
    ///
    /// The caller must hold a reference to `slot`.
    pub(crate) unsafe fn heap(slot: NonNull<Self>) -> HeapId {
        let chunk = slot
            .as_ptr()
            .map_addr(|address| address & !(CHUNK_BYTES - 1))
            .cast::<RootChunk>();
        // SAFETY: chunks are allocated aligned to their size, so masking a
        // slot's address finds its chunk within the same allocation, and the
        // caller's reference keeps that chunk allocated. The header is
        // immutable after allocation.
        unsafe { (*chunk).heap }
    }
}

/// A chunk of root slots: a header the size of one slot, then the slots.
#[repr(C, align(4096))]
struct RootChunk {
    heap: HeapId,
    _reserved: u64,
    slots: [RootSlot; SLOTS_PER_CHUNK],
}

const _: () = assert!(size_of::<RootChunk>() == CHUNK_BYTES);
const _: () = assert!(align_of::<RootChunk>() == CHUNK_BYTES);

impl RootChunk {
    /// Allocates a chunk whose slots form one free list, in address order.
    fn allocate(heap: HeapId) -> (NonNull<Self>, NonNull<RootSlot>) {
        let chunk = NonNull::from(Box::leak(Box::new(Self {
            heap,
            _reserved: 0,
            slots: [const { RootSlot::free() }; SLOTS_PER_CHUNK],
        })));
        for index in 0..SLOTS_PER_CHUNK - 1 {
            // SAFETY: the chunk is new and unpublished, so this thread owns
            // every slot's word.
            unsafe {
                *Self::slot(chunk, index).as_ref().word.get() =
                    Self::slot(chunk, index + 1).as_ptr().cast()
            };
        }
        (chunk, Self::slot(chunk, 0))
    }

    /// The slot at `index`, derived from the chunk's pointer so that it keeps
    /// the provenance of the whole chunk.
    fn slot(chunk: NonNull<Self>, index: usize) -> NonNull<RootSlot> {
        assert!(index < SLOTS_PER_CHUNK, "root slot index out of range");
        // SAFETY: `index` is in bounds of the chunk's slot array, so the
        // projection stays inside the chunk's allocation.
        unsafe {
            NonNull::new_unchecked(
                (&raw mut (*chunk.as_ptr()).slots)
                    .cast::<RootSlot>()
                    .add(index),
            )
        }
    }

    /// Whether any slot still holds a reference.
    ///
    /// # Safety
    ///
    /// `chunk` must be allocated.
    unsafe fn has_references(chunk: NonNull<Self>) -> bool {
        (0..SLOTS_PER_CHUNK).any(|index| {
            // SAFETY: the chunk is allocated. Acquire pairs with each final
            // release, so a chunk found unreferenced is no longer in use.
            unsafe { Self::slot(chunk, index).as_ref() }
                .references
                .load(Ordering::Acquire)
                != 0
        })
    }

    /// Frees an unreferenced chunk.
    ///
    /// # Safety
    ///
    /// `chunk` must come from [`Self::allocate`], hold no references, and
    /// have no slot on a free list anyone will use again.
    unsafe fn free(chunk: NonNull<Self>) {
        // SAFETY: the caller's contract; `allocate` leaked this box.
        drop(unsafe { Box::from_raw(chunk.as_ptr()) });
    }
}

/// A heap's root chunks, under its data mutex.
#[derive(Default)]
pub(crate) struct RootPool {
    chunks: Vec<NonNull<RootChunk>>,
    /// Each chunk's free list as the last collection rebuilt it, or null once
    /// a thread took it.
    free_heads: Vec<*mut RootSlot>,
    /// Indices of chunks whose free list is not empty, lowest last.
    with_free: Vec<usize>,
}

// SAFETY: the pool owns its chunks, and the heap's data mutex serializes every
// access to the pool itself. Slots are shared through their atomic counts.
unsafe impl Send for RootPool {}

impl RootPool {
    /// Takes a whole free list for one thread's cache: the lowest chunk's
    /// that has one, or a new chunk's.
    pub(crate) fn take_free_list(&mut self, heap: HeapId) -> NonNull<RootSlot> {
        while let Some(index) = self.with_free.pop() {
            let head = std::mem::replace(&mut self.free_heads[index], std::ptr::null_mut());
            if let Some(head) = NonNull::new(head) {
                return head;
            }
        }
        let (chunk, head) = RootChunk::allocate(heap);
        self.chunks.push(chunk);
        self.free_heads.push(std::ptr::null_mut());
        head
    }

    /// Visits each live root's value and returns how many it visited.
    ///
    /// The caller must hold exclusive collection authority. A root dropped
    /// concurrently may still be visited, which only retains its value for
    /// one more collection.
    pub(crate) fn visit_live(&self, mut visit: impl FnMut(ErasedGc)) -> usize {
        let mut visited = 0;
        self.for_each_slot(|slot, references| {
            if references != 0 {
                // SAFETY: the slot was live at the Acquire load, and its word
                // is written only while free, which needs a collection.
                visit(unsafe { RootSlot::value(slot) });
                visited += 1;
            }
        });
        visited
    }

    /// Counts the live roots. The caller must hold exclusive collection
    /// authority; concurrent drops can only lower the count.
    pub(crate) fn live_count(&self) -> usize {
        let mut live = 0;
        self.for_each_slot(|_, references| live += usize::from(references != 0));
        live
    }

    /// Rebuilds every chunk's free list from its zero-count slots, in address
    /// order, and forgets the lists threads took.
    ///
    /// The caller must hold exclusive collection authority past its point of
    /// no return, and publish the next allocation lease epoch before any
    /// mutator is admitted, so that no thread uses a list it took before.
    pub(crate) fn rebuild_free_lists(&mut self) {
        self.with_free.clear();
        for (index, &chunk) in self.chunks.iter().enumerate().rev() {
            let mut head: *mut RootSlot = std::ptr::null_mut();
            for slot_index in (0..SLOTS_PER_CHUNK).rev() {
                let slot = RootChunk::slot(chunk, slot_index);
                // SAFETY: the pool's chunks are allocated. A zero count stays
                // zero: no root references the slot, and none is created
                // during the collection, so nothing else touches its word.
                unsafe {
                    if slot.as_ref().references.load(Ordering::Acquire) == 0 {
                        *slot.as_ref().word.get() = head.cast();
                        head = slot.as_ptr();
                    }
                }
            }
            self.free_heads[index] = head;
            if !head.is_null() {
                self.with_free.push(index);
            }
        }
    }

    fn for_each_slot(&self, mut each: impl FnMut(NonNull<RootSlot>, usize)) {
        for &chunk in &self.chunks {
            for index in 0..SLOTS_PER_CHUNK {
                let slot = RootChunk::slot(chunk, index);
                // SAFETY: the pool's chunks are allocated. Acquire pairs with
                // each final release.
                let references = unsafe { slot.as_ref() }.references.load(Ordering::Acquire);
                each(slot, references);
            }
        }
    }
}

/// Chunks of dropped heaps that still held references when their heap
/// dropped.
static ORPHANED_CHUNKS: Mutex<Vec<OrphanedChunk>> = Mutex::new(Vec::new());

struct OrphanedChunk(NonNull<RootChunk>);

// SAFETY: an orphaned chunk is reached only through this list's mutex, to read
// its atomic counts and to free it once none remains.
unsafe impl Send for OrphanedChunk {}

impl Drop for RootPool {
    /// Frees the chunks no root references and orphans the rest, then frees
    /// any earlier heap's orphaned chunks that no root references any more.
    fn drop(&mut self) {
        let mut orphaned = ORPHANED_CHUNKS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        orphaned.retain(|chunk| {
            // SAFETY: an orphaned chunk stays allocated until this sweep
            // finds it unreferenced. A count never rises from zero, and a
            // root's final release is its last access, so then nothing can
            // reach it again.
            unsafe {
                if RootChunk::has_references(chunk.0) {
                    return true;
                }
                RootChunk::free(chunk.0);
            }
            false
        });
        for &chunk in &self.chunks {
            // SAFETY: the dropping heap owns its chunks, and no mutator can
            // take a free list from it any more.
            unsafe {
                if RootChunk::has_references(chunk) {
                    orphaned.push(OrphanedChunk(chunk));
                } else {
                    RootChunk::free(chunk);
                }
            }
        }
    }
}

/// How many of `heap`'s chunks remain orphaned.
#[cfg(test)]
pub(crate) fn orphaned_chunks_of(heap: HeapId) -> usize {
    ORPHANED_CHUNKS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        // SAFETY: orphaned chunks stay allocated while listed, and their
        // header is immutable.
        .filter(|chunk| unsafe { (*chunk.0.as_ptr()).heap } == heap)
        .count()
}

/// Sweeps the orphaned chunks without dropping a heap.
#[cfg(test)]
pub(crate) fn sweep_orphaned_chunks_for_test() {
    drop(RootPool::default());
}
