use std::fmt;
use std::marker::PhantomData;
use std::ops::Deref;
use std::ptr::NonNull;
use std::sync::Arc;

use crate::class::{ObjectMetadata, metadata_for};
use crate::run::{AllocationClassId, RunGeometry};
use crate::thread_cache::{ThreadCacheHandle, ThreadHeapEntry};
use crate::{Ephemeron, Gc, GcRef, Root, Trace, UnsupportedLayout, Weak, heap::HeapInner};

use crate::trace::ErasedGc;

/// A thread's owned heap admission for one [`crate::Heap`].
///
/// [`crate::Heap::mutate`] lends it to a closure. Access borrows it: it
/// dereferences to the [`MutatorAccess`] every heap operation takes, and
/// views ([`crate::GcRef`]) borrow it too. [`Mutator::suspend`] needs it
/// mutably, so no access or view is live across a suspension:
///
/// ```compile_fail,E0502
/// use glam_gc::Heap;
///
/// let heap = Heap::new();
/// heap.mutate(|mutator| {
///     let view = mutator.allocator::<u64>().unwrap().alloc_view(42_u64);
///     mutator.suspend(|| ());
///     let _ = *view;
/// });
/// ```
///
/// The brand `'mu` is invariant and fresh for each `mutate` call, so nothing
/// borrowed from a mutator escapes its closure:
///
/// ```compile_fail
/// use glam_gc::Heap;
///
/// let heap = Heap::new();
/// let root = heap.mutate(|mutator| {
///     let value = mutator.allocator::<u64>().unwrap().alloc_view(42_u64);
///     mutator.root(value)
/// });
/// let _escaped = heap.mutate(|mutator| root.as_gc(mutator));
/// ```
///
/// A mutator is neither `Send` nor `Sync`:
///
/// ```compile_fail
/// use glam_gc::Heap;
///
/// let heap = Heap::new();
/// heap.mutate(|mutator| {
///     std::thread::scope(|scope| {
///         scope.spawn(|| {
///             let _ = mutator.allocator::<u64>();
///         });
///     });
/// });
/// ```
pub struct Mutator<'mu> {
    access: MutatorAccess<'mu>,
    /// The thread's entry for the heap, absent only after a suspension
    /// failed to re-admit.
    entry: Option<ThreadHeapEntry<'mu>>,
    brand: PhantomData<fn(&'mu ()) -> &'mu ()>,
}

impl<'mu> Mutator<'mu> {
    pub(crate) fn new(heap: &'mu Arc<HeapInner>, entry: ThreadHeapEntry<'mu>) -> Self {
        Self {
            access: MutatorAccess::new(heap, entry.cache()),
            entry: Some(entry),
            brand: PhantomData,
        }
    }

    /// Releases heap admission, runs `operation`, and re-admits.
    ///
    /// A suspension is a scoped safepoint: collection may run while
    /// `operation` does, including one `operation` starts, and `operation`
    /// may take another mutator for this or another heap. Values that must
    /// survive the suspension need a [`crate::Root`] or a traced owner. A
    /// suspension belongs where the thread holds no lock a finalizer might
    /// need, since re-admission may wait for a collection.
    ///
    /// # Panics
    ///
    /// Re-admission panics if the heap became
    /// permanently poisoned; the mutator then holds no admission, and any
    /// later access through it panics. If `operation` unwinds, the mutator
    /// stays suspended in the same way.
    pub fn suspend<R>(&mut self, operation: impl FnOnce() -> R) -> R {
        let suspended = self
            .entry
            .take()
            .expect("a mutator that failed to re-admit cannot suspend")
            .suspend();
        let result = operation();
        self.entry = Some(self.access.heap().resume_owned_entry(suspended));
        result
    }
}

impl Mutator<'_> {
    /// Re-admits this mutator after a suspension whose `operation` unwound,
    /// once the caller has caught the unwind. A driver that lends its
    /// mutator to work behind a panic boundary keeps driving this way. Does
    /// nothing while the mutator holds its admission.
    ///
    /// # Panics
    ///
    /// Panics while the thread is unwinding, since re-admission may wait for
    /// a collection; if the thread has another entry for any heap; or if the
    /// heap is permanently poisoned, leaving the mutator without access.
    pub fn readmit(&mut self) {
        if self.entry.is_some() {
            return;
        }
        assert!(
            !std::thread::panicking(),
            "a mutator re-admits only once its unwind is caught"
        );
        let heap = self.access.heap();
        let entry = heap.admit_owned_entry();
        self.access = MutatorAccess::new(heap, entry.cache());
        self.entry = Some(entry);
    }
}

impl<'mu> Deref for Mutator<'mu> {
    type Target = MutatorAccess<'mu>;

    #[inline(always)]
    fn deref(&self) -> &MutatorAccess<'mu> {
        assert!(
            self.entry.is_some(),
            "a mutator that failed to re-admit grants no access"
        );
        &self.access
    }
}

impl fmt::Debug for Mutator<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Mutator")
            .field("admitted", &self.entry.is_some())
            .finish_non_exhaustive()
    }
}

/// Access to one [`crate::Heap`], borrowed from an admitted mutator.
///
/// A [`Mutator`] dereferences to one; the collector lends one to
/// finalization.
///
/// It is intentionally neither `Send` nor `Sync`. Every operation remains
/// qualified by the heap which created the token; holding another heap's token
/// grants no authority over this heap's allocations.
///
/// A mutator cannot be shared with another thread, even through a scoped
/// thread:
///
/// ```compile_fail
/// use glam_gc::Heap;
///
/// let heap = Heap::new();
/// heap.mutate(|mutator| {
///     std::thread::scope(|scope| {
///         scope.spawn(move || {
///             let _ = mutator.allocator::<u64>();
///         });
///     });
/// });
/// ```
pub struct MutatorAccess<'heap> {
    heap: &'heap Arc<HeapInner>,
    cache: ThreadCacheHandle,
    marker: PhantomData<&'heap HeapInner>,
}

impl<'heap> MutatorAccess<'heap> {
    pub(crate) fn new(heap: &'heap Arc<HeapInner>, cache: ThreadCacheHandle) -> Self {
        Self {
            heap,
            cache,
            marker: PhantomData,
        }
    }

    /// Discovers or reuses this heap's allocation class for `T`.
    ///
    /// Process-wide metadata is shared by every heap, while the returned dense
    /// class identity and eventual run pool belong only to this mutator's heap.
    /// Discovery requires mutator admission because it may extend heap-local
    /// class topology. The returned allocator borrows this mutator and cannot
    /// escape its admitted region:
    ///
    /// ```compile_fail
    /// use glam_gc::Heap;
    ///
    /// let heap = Heap::new();
    /// let _allocator = heap.mutate(|mutator| {
    ///     mutator.allocator::<u64>().unwrap()
    /// });
    /// ```
    pub fn allocator<T: Trace>(&self) -> Result<Allocator<'_, T>, UnsupportedLayout> {
        let (metadata, id) = match self.cache.cached_class_key::<T>() {
            Some(key) => key,
            None => self.discover_class::<T>()?,
        };
        Ok(Allocator {
            heap: self.heap.as_ref(),
            cache: &self.cache,
            metadata,
            id,
            marker: PhantomData,
        })
    }

    /// Resolves `T`'s class in this heap for the first time on this thread
    /// and caches it, off the allocator's hot path.
    #[cold]
    #[inline(never)]
    fn discover_class<T: Trace>(
        &self,
    ) -> Result<(&'static ObjectMetadata, AllocationClassId), UnsupportedLayout> {
        let metadata = metadata_for::<T>();
        let geometry = RunGeometry::derive(metadata.layout(), metadata.requested_slot_size())
            .map_err(UnsupportedLayout::from_validated_geometry)?;
        let class = self.heap.discover_class::<T>(metadata, geometry);
        self.cache.remember_class(&class);
        Ok((class.metadata(), class.id()))
    }

    /// Constructs and publishes an external root for one viewed value.
    ///
    /// The view is the evidence: it proves the value live, allocated and of
    /// type `T` in this heap, the one heap the thread holds, so release
    /// builds register the root without validating it. Debug builds validate
    /// it as for any access. Publication into the heap's weak root registry
    /// completes before this method returns.
    pub fn root<T: Trace>(&self, value: GcRef<'_, T>) -> Root<T> {
        let pointer = value.pointer();
        self.debug_assert_access(pointer);
        self.heap
            .register_proven_root(&self.cache, ErasedGc::new(pointer.cast()))
    }

    /// Constructs and publishes an external root for a bare managed pointer,
    /// which proves nothing.
    ///
    /// Every build checks that the value belongs to this mutator's heap, is
    /// allocated, and has the canonical representation for `T`, and panics
    /// if not. Publication into the heap's weak root registry completes
    /// before this method returns. Prefer [`MutatorAccess::root`] where a
    /// view proves the value live.
    pub fn root_checked<T: Trace>(&self, value: Gc<T>) -> Root<T> {
        let expected = self
            .cache
            .cached_metadata::<T>()
            .unwrap_or_else(metadata_for::<T>);
        self.heap.register_root(&self.cache, value, expected)
    }

    /// Constructs an ephemeron of `key` and `value`: the value lives while
    /// some other path reaches the key, and neither lives by the ephemeron
    /// alone. Once a collection finds the key unreachable, it clears the
    /// ephemeron and queues `notice` for
    /// [`Heap::take_ephemeron_notices`](crate::Heap::take_ephemeron_notices).
    /// The views prove both live.
    pub fn ephemeron<K: Trace, V: Trace>(
        &self,
        key: GcRef<'_, K>,
        value: GcRef<'_, V>,
        notice: u64,
    ) -> Ephemeron<K, V> {
        let (key, value) = (key.pointer(), value.pointer());
        self.debug_assert_access(key);
        self.debug_assert_access(value);
        self.heap.register_ephemeron(
            ErasedGc::new(key.cast()),
            ErasedGc::new(value.cast()),
            notice,
        )
    }

    /// As [`Self::ephemeron`], for bare managed pointers, which prove
    /// nothing: every build checks both as for [`Self::root_checked`].
    pub fn ephemeron_checked<K: Trace, V: Trace>(
        &self,
        key: Gc<K>,
        value: Gc<V>,
        notice: u64,
    ) -> Ephemeron<K, V> {
        let expected = (
            self.cache
                .cached_metadata::<K>()
                .unwrap_or_else(metadata_for::<K>),
            self.cache
                .cached_metadata::<V>()
                .unwrap_or_else(metadata_for::<V>),
        );
        self.heap
            .register_checked_ephemeron(key, value, notice, expected)
    }

    /// Constructs a weak reference to `value`: an ephemeron whose key is its
    /// value, which it views while some other path reaches it.
    pub fn weak<T: Trace>(&self, value: GcRef<'_, T>, notice: u64) -> Weak<T> {
        Weak::new(self.ephemeron(value, value, notice))
    }

    /// As [`Self::weak`], for a bare managed pointer, checked as for
    /// [`Self::root_checked`].
    pub fn weak_checked<T: Trace>(&self, value: Gc<T>, notice: u64) -> Weak<T> {
        Weak::new(self.ephemeron_checked(value.duplicate_in(self), value, notice))
    }

    pub(crate) fn debug_assert_access<T: Trace>(&self, pointer: NonNull<T>) {
        self.heap.debug_assert_access(pointer);
    }

    pub(crate) fn heap(&self) -> &'heap Arc<HeapInner> {
        self.heap
    }

    #[cfg(feature = "deterministic-test-hooks")]
    pub(crate) fn assert_observed_edge_for_test(&self, edge: ErasedGc) {
        self.heap.assert_observed_edge(edge);
    }

    #[cfg(feature = "deterministic-test-hooks")]
    pub(crate) fn is_rootable_for_measurement<T: Trace>(&self, value: &Gc<T>) -> bool {
        self.heap.is_rootable_for_measurement(value)
    }
}

/// Typed allocation authority borrowed from one admitted mutator region.
///
/// The heap owns the durable allocation class and run topology. This value
/// retains neither the heap nor any allocation, and safe Rust cannot use it
/// after the mutator which created it leaves scope.
///
/// It also cannot cross a thread boundary, including through a scoped thread:
///
/// ```compile_fail
/// use glam_gc::Heap;
///
/// let heap = Heap::new();
/// heap.mutate(|mutator| {
///     let allocator = mutator.allocator::<u64>().unwrap();
///     std::thread::scope(|scope| {
///         scope.spawn(move || {
///             let _ = allocator.alloc(42);
///         });
///     });
/// });
/// ```
#[must_use = "a scoped allocator is required for managed allocation"]
pub struct Allocator<'mutator, T: Trace> {
    heap: &'mutator HeapInner,
    cache: &'mutator ThreadCacheHandle,
    // The class's metadata and id, which its thread's cache resolved for this
    // heap. A refill takes the class's shared state from that cache, so
    // acquiring an allocator clones no `Arc`.
    metadata: &'static ObjectMetadata,
    id: AllocationClassId,
    marker: PhantomData<fn() -> T>,
}

impl<'mutator, T: Trace> Allocator<'mutator, T> {
    /// Allocates one value through this scoped heap-local allocation class.
    ///
    /// The surrounding admitted mutator region is the liveness witness for
    /// the returned pointer and for any unpublished graph assembled from it.
    /// `alloc` does not publish a root. A pointer which must survive the end of
    /// the region must first be installed beneath an already traced owner or
    /// registered as a root.
    ///
    /// Zero-sized managed types are unsupported:
    ///
    /// ```compile_fail,E0080
    /// use glam_gc::Heap;
    ///
    /// let heap = Heap::new();
    /// heap.mutate(|mutator| {
    ///     let allocator = mutator.allocator::<()>().unwrap();
    ///     let _ = allocator.alloc(());
    /// });
    /// ```
    pub fn alloc(&self, value: T) -> Gc<T> {
        const {
            assert!(
                std::mem::size_of::<T>() != 0,
                "zero-sized managed types are unsupported"
            );
        }

        match self.cache.try_allocate(self.id, value) {
            Ok(pointer) => {
                // SAFETY: the worker-local allocator initialized `T` in its
                // exclusively leased range and published the allocation bit.
                unsafe { Gc::from_raw(pointer) }
            }
            Err(value) => self.alloc_after_refill(value),
        }
    }

    /// Claims a fresh cursor for this class and allocates from it.
    #[cold]
    #[inline(never)]
    fn alloc_after_refill(&self, value: T) -> Gc<T> {
        let class = self
            .cache
            .cached_class::<T>(self.heap)
            .expect("an allocator's class stays cached for its heap");
        let cursor = self.heap.claim_allocation_cursor(&class);
        self.cache.install(cursor);
        let pointer = self
            .cache
            .try_allocate_after_refill(self.id, value)
            .unwrap_or_else(|_| panic!("fresh allocation cursor contains no free slot"));

        // SAFETY: the worker-local allocator initialized `T` in its exclusively
        // leased range and published the allocation bit.
        unsafe { Gc::from_raw(pointer) }
    }

    /// Allocates one value, as [`Allocator::alloc`], and views it.
    ///
    /// A fresh allocation is live for as long as the allocator's access
    /// stays borrowed, so the view is evidence for [`MutatorAccess::root`].
    pub fn alloc_view(&self, value: T) -> GcRef<'mutator, T> {
        let edge = self.alloc(value);
        // SAFETY: `alloc` just initialized a live `T` in this allocator's
        // heap, and the allocator borrows its admitted access for
        // `'mutator`.
        unsafe { GcRef::fresh(edge) }
    }

    #[cfg(test)]
    pub(crate) fn id(&self) -> AllocationClassId {
        self.id
    }

    #[cfg(test)]
    pub(crate) fn metadata(&self) -> &'static ObjectMetadata {
        self.metadata
    }

    #[cfg(test)]
    pub(crate) fn belongs_to(&self, heap: &HeapInner) -> bool {
        std::ptr::eq(self.heap, heap)
    }

    #[cfg(test)]
    pub(crate) fn alloc_with_before_initialize(
        &self,
        value: T,
        before_initialize: impl FnOnce(),
    ) -> Gc<T> {
        let pointer = self
            .cache
            .try_allocate_with(self.id, value, before_initialize)
            .unwrap_or_else(|_| panic!("test allocation requires a retained cursor"));
        // SAFETY: the retained cursor returns only after initialization and
        // allocation-bit publication, as in the public allocation path.
        unsafe { Gc::from_raw(pointer) }
    }
}

impl<T: Trace> fmt::Debug for Allocator<'_, T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Allocator")
            .field("type_name", &self.metadata.type_name())
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}
