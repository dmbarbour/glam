use std::alloc::{Layout, alloc_zeroed, dealloc};
#[cfg(test)]
use std::cell::Cell;
use std::collections::HashMap;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(test)]
use crate::Trace;
use crate::run::{AllocationClassId, RUN_SIZE, RunGeometry, RunHeader};

pub(crate) const ARENA_CHUNK_SIZE: usize = 8 * 1024 * 1024;
pub(crate) const RUNS_PER_CHUNK: usize = ARENA_CHUNK_SIZE / RUN_SIZE;

const _: () = assert!(ARENA_CHUNK_SIZE.is_power_of_two());
const _: () = assert!(ARENA_CHUNK_SIZE.is_multiple_of(RUN_SIZE));

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ArenaError {
    AllocationFailed,
    AddressOverlap,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RunInitializationError {
    MissingChunk,
    MissingRun,
    InvalidHeader,
    InvalidGeometry,
    AlreadyInitialized,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RunPublicationError {
    Arena(ArenaError),
    Initialization(RunInitializationError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub(crate) struct RunLocation {
    pub(crate) chunk: usize,
    pub(crate) run: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RunAddress {
    pointer: NonNull<u8>,
}

impl RunAddress {
    pub(crate) fn address(self) -> usize {
        self.pointer.addr().get()
    }

    pub(crate) fn pointer(self) -> NonNull<u8> {
        self.pointer
    }

    #[cfg(test)]
    pub(crate) fn dangling_for_cache_test() -> Self {
        Self {
            pointer: NonNull::dangling(),
        }
    }
}

#[derive(Default)]
pub(crate) struct Arena {
    chunks: Vec<ArenaChunk>,
    chunk_indices: HashMap<usize, usize>,
    #[cfg(test)]
    indexed_lookup_count: Cell<usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RunOwner {
    pub(crate) location: RunLocation,
    pub(crate) run: RunAddress,
    pub(crate) class_id: AllocationClassId,
    pub(crate) geometry: RunGeometry,
    pub(crate) slot_index: usize,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct InitializedRun {
    pub(crate) location: RunLocation,
    pub(crate) class_id: AllocationClassId,
    pub(crate) geometry: RunGeometry,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ClaimedAllocationWord {
    pub(crate) location: RunLocation,
    pub(crate) run: RunAddress,
    pub(crate) geometry: RunGeometry,
    pub(crate) word_index: usize,
    pub(crate) free_mask: u64,
}

/// Stable, heap-owned topology needed to claim allocation words from one run.
///
/// The record itself does not retain its arena. It remains private to allocator
/// paths which retain the owning heap for the complete claim operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RunClaimTarget {
    pub(crate) location: RunLocation,
    pub(crate) run: RunAddress,
    pub(crate) geometry: RunGeometry,
}

impl RunClaimTarget {
    pub(crate) fn claim_allocation_word(self) -> Option<ClaimedAllocationWord> {
        for lease_word_index in 0..self.geometry.lease_bitmap.word_len {
            let lease = lease_word_pointer(self.run, self.geometry.lease_bitmap, lease_word_index);
            // Initial run visibility comes from the class frontier's Acquire
            // load or from the managed-data mutex, not from this distinct
            // atomic. Acquire observes the collector's exact post-sweep
            // Release lease publication. The CAS is the ownership transition
            // for the selected word.
            let mut observed = unsafe { lease.as_ref() }.load(Ordering::Acquire);
            loop {
                let candidates = !observed;
                if candidates == 0 {
                    break;
                }
                let lease_bit_index = candidates.trailing_zeros() as usize;
                let word_index = lease_word_index * u64::BITS as usize + lease_bit_index;
                if word_index >= self.geometry.lease_bitmap.bit_len {
                    // Invalid bits form only the suffix of the final lease
                    // word. Reaching one proves every valid candidate in this
                    // word was already claimed.
                    break;
                }
                let bit = 1_u64 << lease_bit_index;
                match unsafe { lease.as_ref() }.compare_exchange_weak(
                    observed,
                    observed | bit,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => {
                        let free_mask = free_mask_for_word(self.run, self.geometry, word_index);
                        if free_mask != 0 {
                            return Some(ClaimedAllocationWord {
                                location: self.location,
                                run: self.run,
                                geometry: self.geometry,
                                word_index,
                                free_mask,
                            });
                        }

                        // A full allocation word stays leased. Continue from
                        // the state this worker just published rather than
                        // making it available for repeated futile claims.
                        observed |= bit;
                    }
                    Err(actual) => observed = actual,
                }
            }
        }
        None
    }
}

// SAFETY: `RunClaimTarget` is constructed only for a published run in stable
// heap-owned arena storage. Shared operations mutate lease words atomically;
// one successful lease bit grants one thread exclusive write access to the
// matching atomic allocation word. Private callers retain the heap while using
// it.
unsafe impl Send for RunClaimTarget {}
// SAFETY: the same published-run and atomic-lease invariants permit concurrent
// claims through shared copies of this immutable topology record.
unsafe impl Sync for RunClaimTarget {}

impl Arena {
    pub(crate) fn run_capacity(&self) -> usize {
        self.chunks
            .len()
            .checked_mul(RUNS_PER_CHUNK)
            .expect("arena run capacity exhausted")
    }

    #[cfg(test)]
    pub(crate) fn reserve_chunk(&mut self) -> Result<usize, ArenaError> {
        let candidate = ArenaChunk::allocate()?;
        self.publish_chunk(candidate)
    }

    #[cfg(test)]
    pub(crate) fn run_address(&self, chunk: usize, run: usize) -> Option<RunAddress> {
        self.chunks.get(chunk)?.run_address(run)
    }

    #[cfg(test)]
    pub(crate) fn run_at(&self, location: RunLocation) -> Option<RunAddress> {
        self.run_address(location.chunk, location.run)
    }

    #[cfg(test)]
    pub(crate) fn find_run(&self, address: usize) -> Option<RunAddress> {
        self.chunk_containing(address)?.1.run_containing(address)
    }

    pub(crate) fn initialize_run(
        &mut self,
        chunk: usize,
        run: usize,
        class_id: AllocationClassId,
        geometry: RunGeometry,
    ) -> Result<(), RunInitializationError> {
        self.chunks
            .get_mut(chunk)
            .ok_or(RunInitializationError::MissingChunk)?
            .initialize_run(run, class_id, geometry)
    }

    /// Validates one stable typed-run target against authoritative arena state.
    ///
    /// Keeping this validation separate lets collector transitions validate a
    /// complete batch before resetting a header or allocation word.
    pub(crate) fn validate_run_target(&self, target: RunClaimTarget, class_id: AllocationClassId) {
        let chunk = self
            .chunks
            .get(target.location.chunk)
            .expect("validated run lost its arena chunk");
        let run = chunk
            .run_address(target.location.run)
            .expect("validated run lost its arena location");
        assert_eq!(run, target.run, "validated run changed addresses");
        let header = chunk
            .header_for(run)
            .expect("validated run lost its initialized header");
        assert_eq!(
            header.class_id(),
            Some(class_id),
            "validated run changed allocation classes"
        );
        assert_eq!(
            header.geometry(),
            Some(target.geometry),
            "validated run changed geometry"
        );
    }

    /// Clears a prevalidated, wholly dead run and restores its empty header.
    ///
    /// The caller must hold exclusive collection authority, must have removed
    /// every class and frontier selector for `target`, and must have proved
    /// that no initialized payload in the run requires destruction. Payload
    /// bytes remain unspecified and become unreachable when the header is
    /// reset; every authoritative allocation, lease, and mark bit is cleared.
    pub(crate) fn reset_recyclable_run(
        &mut self,
        target: RunClaimTarget,
        class_id: AllocationClassId,
    ) {
        self.validate_run_target(target, class_id);
        self.chunks[target.location.chunk]
            .reset_recyclable_run(target.location.run, target.geometry);
    }

    pub(crate) fn publish_run(
        &mut self,
        class_id: AllocationClassId,
        geometry: RunGeometry,
    ) -> Result<RunLocation, RunPublicationError> {
        if !geometry.is_structurally_valid() {
            return Err(RunPublicationError::Initialization(
                RunInitializationError::InvalidGeometry,
            ));
        }

        for (chunk_index, chunk) in self.chunks.iter_mut().enumerate() {
            if let Some(run) = chunk.first_empty_run() {
                chunk
                    .initialize_run(run, class_id, geometry)
                    .map_err(RunPublicationError::Initialization)?;
                return Ok(RunLocation {
                    chunk: chunk_index,
                    run,
                });
            }
        }

        // Initialize the candidate before it enters the arena. Allocation,
        // overlap validation, or initialization failure therefore publishes
        // neither a chunk nor a typed run.
        let mut candidate = ArenaChunk::allocate().map_err(RunPublicationError::Arena)?;
        candidate
            .initialize_run(0, class_id, geometry)
            .map_err(RunPublicationError::Initialization)?;
        let chunk = self
            .publish_chunk(candidate)
            .map_err(RunPublicationError::Arena)?;
        Ok(RunLocation { chunk, run: 0 })
    }

    #[cfg(test)]
    pub(crate) fn initialized_runs(&self) -> Vec<InitializedRun> {
        let mut runs = Vec::new();
        for (chunk_index, chunk) in self.chunks.iter().enumerate() {
            for run in 0..RUNS_PER_CHUNK {
                let Some(header) = chunk.header_for_index(run) else {
                    continue;
                };
                let Some(class_id) = header.class_id() else {
                    continue;
                };
                let Some(geometry) = header.geometry() else {
                    continue;
                };
                let location = RunLocation {
                    chunk: chunk_index,
                    run,
                };
                runs.push(InitializedRun {
                    location,
                    class_id,
                    geometry,
                });
            }
        }
        runs
    }

    pub(crate) fn checked_slot_owner(&self, address: usize) -> Option<RunOwner> {
        let (chunk_index, chunk) = self.chunk_containing(address)?;
        let run = chunk.run_containing(address)?;
        let header = chunk.header_for(run)?;
        let class_id = header.class_id()?;
        let geometry = header.geometry()?;
        let slot_index = geometry.slot_index(address - run.address())?;
        let run_index = (run.address() - chunk.range().start) / RUN_SIZE;
        Some(RunOwner {
            location: RunLocation {
                chunk: chunk_index,
                run: run_index,
            },
            run,
            class_id,
            geometry,
            slot_index,
        })
    }

    pub(crate) fn owner_slot_is_allocated(&self, owner: RunOwner) -> bool {
        let (chunk, run) = self.resolved_owner_run(owner);
        chunk.slot_is_allocated(run, owner.geometry, owner.slot_index)
    }

    pub(crate) fn owner_slot_pointer(&self, owner: RunOwner) -> NonNull<()> {
        let (chunk, run) = self.resolved_owner_run(owner);
        assert_eq!(
            chunk.header_for(run).and_then(RunHeader::class_id),
            Some(owner.class_id),
            "resolved allocation owner changed classes"
        );
        assert_eq!(
            chunk.header_for(run).and_then(RunHeader::geometry),
            Some(owner.geometry),
            "resolved allocation owner changed geometry"
        );
        let offset = owner
            .geometry
            .slot_offset(owner.slot_index)
            .expect("resolved allocation owner lost its slot");
        // SAFETY: the retained run and its initialized header were validated
        // above, and bounded slot geometry places this pointer at the exact
        // payload start inside the live arena allocation.
        unsafe { run.pointer().add(offset).cast() }
    }

    /// Retires one terminally destroyed allocation from a finalizer-owned word.
    ///
    /// The caller must own the exact finalization reservation for this word,
    /// so no ordinary allocation-word writer may race this update. Root and
    /// debug access may still read the atomic word concurrently.
    pub(crate) fn clear_owner_allocation(&self, owner: RunOwner) -> bool {
        let (chunk, run) = self.resolved_owner_run(owner);
        assert_eq!(
            chunk.header_for(run).and_then(RunHeader::class_id),
            Some(owner.class_id),
            "finalized allocation changed classes"
        );
        assert_eq!(
            chunk.header_for(run).and_then(RunHeader::geometry),
            Some(owner.geometry),
            "finalized allocation changed geometry"
        );
        let (word_index, bit) = bitmap_bit(owner.geometry.allocation_bitmap, owner.slot_index);
        let allocation = allocation_word_pointer(run, owner.geometry, word_index);
        // SAFETY: validated geometry places this initialized atomic word in
        // the retained run. The finalization reservation makes this caller its
        // sole writer; AcqRel preserves payload publication for concurrent
        // readers and orders retirement after `Drop` either returns or reaches
        // the collector's unwind boundary.
        unsafe { allocation.as_ref() }.fetch_and(!bit, Ordering::AcqRel) & bit != 0
    }

    /// Releases one allocation word after its last pending destructor
    /// completes.
    ///
    /// The finalization batch must still own the exact word when this method
    /// begins. Its lease bit was therefore unavailable before ordinary
    /// admission reopened, and no allocation cursor can own the word. Clearing
    /// that one lease bit publishes the allocation-word retirement performed
    /// by finalization without disturbing concurrent claims for neighboring
    /// words represented by the same atomic lease word.
    pub(crate) fn release_finalized_allocation_word(
        &self,
        target: RunClaimTarget,
        class_id: AllocationClassId,
        word_index: usize,
    ) {
        let (_, run) = self.resolved_claim_target(target, class_id);
        assert!(
            word_index < target.geometry.allocation_bitmap.word_len,
            "released finalization word is outside its run"
        );
        assert_ne!(
            free_mask_for_word(run, target.geometry, word_index),
            0,
            "completed finalization word has no reusable slot"
        );
        let (lease_word_index, bit) = bitmap_bit(target.geometry.lease_bitmap, word_index);
        let lease = lease_word_pointer(run, target.geometry.lease_bitmap, lease_word_index);
        // SAFETY: validated geometry places this initialized atomic lease word
        // in the retained run. The finalization reservation proves this bit has
        // no allocation owner, while atomic RMW preserves concurrent claims in
        // every neighboring bit. Release publishes the cleared allocation bit
        // before a frontier can expose this word.
        let previous = unsafe { lease.as_ref() }.fetch_and(!bit, Ordering::Release);
        assert_ne!(
            previous & bit,
            0,
            "completed finalization word was already allocator-visible"
        );
    }

    pub(crate) fn clear_assigned_mark_bitmaps(&mut self) -> usize {
        let mut cleared = 0;
        for chunk in &mut self.chunks {
            cleared += chunk.clear_assigned_mark_bitmaps();
        }
        cleared
    }

    #[cfg(test)]
    pub(crate) fn owner_slot_is_marked(&self, owner: RunOwner) -> bool {
        let (chunk, run) = self.resolved_owner_run(owner);
        chunk.slot_is_marked(run, owner.geometry, owner.slot_index)
    }

    /// Visits the authoritative allocation and mark words for one published run.
    ///
    /// The caller must hold exclusive collection authority while observing the
    /// ordinary mark bitmap. Invalid suffix bits are masked from both words,
    /// so every reported bit maps to one payload slot in `target`.
    pub(crate) fn visit_allocation_mark_words(
        &self,
        target: RunClaimTarget,
        class_id: AllocationClassId,
        mut visit: impl FnMut(usize, u64, u64),
    ) {
        let (_, run) = self.resolved_claim_target(target, class_id);

        for word_index in 0..target.geometry.allocation_bitmap.word_len {
            let valid = valid_slot_mask(target.geometry.slot_count, word_index);
            let allocation = allocation_word_pointer(run, target.geometry, word_index);
            // SAFETY: validated published-run geometry places this initialized
            // atomic allocation word in the retained arena. Acquire observes
            // every payload publication before its allocation bit.
            let allocated = unsafe { allocation.as_ref() }.load(Ordering::Acquire) & valid;
            let marked = mark_word_pointer(run, target.geometry, word_index);
            // SAFETY: the caller's exclusive collection authority excludes
            // concurrent mark writers, while validated geometry keeps this
            // initialized ordinary word inside the run.
            let marked = unsafe { marked.read() } & valid;
            visit(word_index, allocated, marked);
        }
    }

    /// Retains exactly the marked allocations in one partial no-drop run.
    ///
    /// The caller must hold exclusive collection authority with every cursor
    /// owner drained, must have withdrawn ordinary frontiers, and must have
    /// proved that clearing dead allocation bits requires no destructor. It
    /// must publish a new cursor epoch before ordinary admission resumes.
    /// Payload storage is neither enumerated nor accessed.
    pub(crate) fn retain_marked_allocations(
        &self,
        target: RunClaimTarget,
        class_id: AllocationClassId,
    ) {
        let (_, run) = self.resolved_claim_target(target, class_id);

        for word_index in 0..target.geometry.allocation_bitmap.word_len {
            let valid = valid_slot_mask(target.geometry.slot_count, word_index);
            let allocation = allocation_word_pointer(run, target.geometry, word_index);
            // SAFETY: validated published-run geometry places this initialized
            // atomic allocation word in the retained arena. Exclusive
            // collection excludes its leased writer; Acquire observes the
            // payload publications whose bits are being retained or cleared.
            let allocated = unsafe { allocation.as_ref() }.load(Ordering::Acquire) & valid;
            let mark = mark_word_pointer(run, target.geometry, word_index);
            // SAFETY: exclusive collection is the sole mark-word reader and
            // writer, and validated geometry bounds this ordinary word.
            let marked = unsafe { mark.read() } & valid;
            debug_assert_eq!(marked & allocated, marked);

            let retained = allocated & marked;
            // SAFETY: the same validated allocation word has no concurrent
            // writer under Exclusive. Release publishes the swept allocation
            // state before C6A.3b makes rebuilt leases/frontiers visible.
            unsafe { allocation.as_ref() }.store(retained, Ordering::Release);
        }
    }

    /// Publishes the final post-sweep lease view for one retained run.
    ///
    /// `reserved` identifies allocation words retained for finalization. Full
    /// words and reserved words are unavailable; every other valid word is
    /// directly claimable. The caller must hold exclusive collection
    /// authority and publish the heap-wide lease epoch only after rebuilding
    /// every run and class frontier. Returns whether the run has at least one
    /// claimable word.
    pub(crate) fn publish_allocation_word_leases(
        &self,
        target: RunClaimTarget,
        class_id: AllocationClassId,
        mut reserved: impl FnMut(usize) -> bool,
    ) -> bool {
        let (_, run) = self.resolved_claim_target(target, class_id);
        let mut has_available_word = false;

        for lease_word_index in 0..target.geometry.lease_bitmap.word_len {
            let valid = valid_slot_mask(target.geometry.lease_bitmap.bit_len, lease_word_index);
            let mut unavailable = 0_u64;
            for lease_bit_index in 0..u64::BITS as usize {
                let bit = 1_u64 << lease_bit_index;
                if valid & bit == 0 {
                    break;
                }
                let word_index = lease_word_index * u64::BITS as usize + lease_bit_index;
                let unavailable_word = reserved(word_index)
                    || free_mask_for_word(run, target.geometry, word_index) == 0;
                if unavailable_word {
                    unavailable |= bit;
                } else {
                    has_available_word = true;
                }
            }

            let lease = lease_word_pointer(run, target.geometry.lease_bitmap, lease_word_index);
            // SAFETY: the stable typed target was prevalidated while
            // Exclusive excludes every lease owner. This initialized atomic
            // lease word is bounded by its geometry. Release publishes the
            // exact swept/reserved view before the later epoch advance.
            unsafe { lease.as_ref() }.store(unavailable, Ordering::Release);
        }

        has_available_word
    }

    pub(crate) fn mark_owner_slot(&mut self, owner: RunOwner) -> bool {
        let chunk = self
            .chunks
            .get_mut(owner.location.chunk)
            .unwrap_or_else(|| panic!("resolved mark owner lost its arena chunk"));
        let run = chunk
            .run_address(owner.location.run)
            .expect("resolved mark owner lost its run");
        assert_eq!(run, owner.run, "resolved mark owner changed runs");
        chunk.mark_slot(run, owner.geometry, owner.slot_index)
    }

    #[cfg(test)]
    pub(crate) fn first_free_slot(&self, location: RunLocation) -> Option<usize> {
        self.chunks
            .get(location.chunk)?
            .first_free_slot(location.run)
    }

    #[cfg(test)]
    pub(crate) fn initialize_slot<T: Trace>(
        &mut self,
        location: RunLocation,
        class_id: AllocationClassId,
        geometry: RunGeometry,
        slot_index: usize,
        value: T,
    ) -> NonNull<T> {
        self.chunks
            .get_mut(location.chunk)
            .expect("published run must retain its arena chunk")
            .initialize_slot(location.run, class_id, geometry, slot_index, value)
    }

    pub(crate) fn allocated_slot_pointers(&self, location: RunLocation) -> Vec<NonNull<()>> {
        self.chunks
            .get(location.chunk)
            .expect("published run must retain its arena chunk")
            .allocated_slot_pointers(location.run)
    }

    #[cfg(test)]
    pub(crate) fn run_side_metadata_for_test(
        &self,
        location: RunLocation,
        geometry: RunGeometry,
    ) -> Vec<u8> {
        self.chunks
            .get(location.chunk)
            .expect("published run must retain its arena chunk")
            .side_metadata(location.run, geometry)
    }

    #[cfg(test)]
    pub(crate) fn run_is_empty_for_test(&self, location: RunLocation) -> bool {
        self.chunks
            .get(location.chunk)
            .and_then(|chunk| chunk.header_for_index(location.run))
            .is_some_and(RunHeader::is_empty)
    }

    #[cfg(test)]
    pub(crate) fn claim_allocation_word(
        &self,
        location: RunLocation,
    ) -> Option<ClaimedAllocationWord> {
        self.run_claim_target(location)?.claim_allocation_word()
    }

    pub(crate) fn run_claim_target(&self, location: RunLocation) -> Option<RunClaimTarget> {
        let chunk = self.chunks.get(location.chunk)?;
        let run = chunk.run_address(location.run)?;
        let geometry = chunk.header_for(run)?.geometry()?;
        Some(RunClaimTarget {
            location,
            run,
            geometry,
        })
    }

    fn resolved_owner_run(&self, owner: RunOwner) -> (&ArenaChunk, RunAddress) {
        let chunk = self
            .chunks
            .get(owner.location.chunk)
            .unwrap_or_else(|| panic!("resolved allocation owner lost its arena chunk"));
        let run = chunk
            .run_address(owner.location.run)
            .expect("resolved allocation owner lost its run");
        assert_eq!(run, owner.run, "resolved allocation owner changed runs");
        (chunk, run)
    }

    fn resolved_claim_target(
        &self,
        target: RunClaimTarget,
        class_id: AllocationClassId,
    ) -> (&ArenaChunk, RunAddress) {
        let chunk = self
            .chunks
            .get(target.location.chunk)
            .expect("published run must retain its arena chunk");
        let run = chunk
            .run_address(target.location.run)
            .expect("published run must retain its arena location");
        assert_eq!(run, target.run, "published run changed addresses");
        assert_eq!(
            chunk.header_for(run).and_then(RunHeader::class_id),
            Some(class_id),
            "published run changed allocation classes"
        );
        assert_eq!(
            chunk.header_for(run).and_then(RunHeader::geometry),
            Some(target.geometry),
            "published run changed geometry"
        );
        (chunk, run)
    }

    fn publish_chunk(&mut self, candidate: ArenaChunk) -> Result<usize, ArenaError> {
        let base = candidate.range().start;
        if self.chunk_indices.contains_key(&base) {
            return Err(ArenaError::AddressOverlap);
        }

        // Reserve both fallible allocations before either collection exposes
        // the candidate. From here through insertion, the operations cannot
        // allocate, and the arena is exclusively borrowed under the
        // managed-data mutex. Equal aligned bases are the only way fixed-size chunks can
        // overlap.
        self.chunks
            .try_reserve(1)
            .map_err(|_| ArenaError::AllocationFailed)?;
        self.chunk_indices
            .try_reserve(1)
            .map_err(|_| ArenaError::AllocationFailed)?;

        let index = self.chunks.len();
        self.chunks.push(candidate);
        let prior = self.chunk_indices.insert(base, index);
        debug_assert!(prior.is_none());
        debug_assert_eq!(self.chunks.len(), self.chunk_indices.len());
        Ok(index)
    }

    fn chunk_containing(&self, address: usize) -> Option<(usize, &ArenaChunk)> {
        #[cfg(test)]
        self.indexed_lookup_count
            .set(self.indexed_lookup_count.get() + 1);

        let base = address & !(ARENA_CHUNK_SIZE - 1);
        let index = *self.chunk_indices.get(&base)?;
        let chunk = self.chunks.get(index)?;
        debug_assert_eq!(chunk.range().start, base);
        chunk.range().contains(address).then_some((index, chunk))
    }

    #[cfg(test)]
    fn chunk_range(&self, chunk: usize) -> Option<AddressRange> {
        self.chunks.get(chunk).map(ArenaChunk::range)
    }

    #[cfg(test)]
    fn header_for_test(&self, chunk: usize, run: usize) -> Option<&RunHeader> {
        let chunk = self.chunks.get(chunk)?;
        chunk.header_for_index(run)
    }

    #[cfg(test)]
    fn fill_side_metadata_for_test(
        &mut self,
        chunk: usize,
        run: usize,
        geometry: RunGeometry,
        value: u8,
    ) {
        self.chunks[chunk].fill_side_metadata(run, geometry, value);
    }

    #[cfg(test)]
    fn side_metadata_for_test(&self, chunk: usize, run: usize, geometry: RunGeometry) -> Vec<u8> {
        self.chunks[chunk].side_metadata(run, geometry)
    }

    #[cfg(test)]
    fn reset_indexed_lookup_count(&self) {
        self.indexed_lookup_count.set(0);
    }

    #[cfg(test)]
    fn indexed_lookup_count(&self) -> usize {
        self.indexed_lookup_count.get()
    }
}

struct ArenaChunk {
    base: NonNull<u8>,
}

impl ArenaChunk {
    fn allocate() -> Result<Self, ArenaError> {
        let layout = chunk_layout();
        // SAFETY: `layout` has nonzero size and supported power-of-two
        // alignment. A successful result owns exactly that uninitialized byte
        // allocation until `Drop` returns it with the identical layout.
        let base =
            NonNull::new(unsafe { alloc_zeroed(layout) }).ok_or(ArenaError::AllocationFailed)?;
        debug_assert!(base.addr().get().is_multiple_of(RUN_SIZE));
        let mut chunk = Self { base };
        chunk.initialize_headers();
        Ok(chunk)
    }

    fn range(&self) -> AddressRange {
        AddressRange::new(self.base.addr().get())
            .expect("a live arena allocation must fit the address space")
    }

    fn run_address(&self, run: usize) -> Option<RunAddress> {
        if run >= RUNS_PER_CHUNK {
            return None;
        }
        let offset = run * RUN_SIZE;
        // SAFETY: `run < RUNS_PER_CHUNK` proves `offset < ARENA_CHUNK_SIZE`,
        // so the derived pointer remains inside this live allocation.
        let pointer = unsafe { self.base.add(offset) };
        Some(RunAddress { pointer })
    }

    fn run_containing(&self, address: usize) -> Option<RunAddress> {
        let range = self.range();
        let run_base = range.run_base(address)?;
        let offset = run_base - range.start;
        // SAFETY: numeric range validation happened before pointer derivation;
        // `run_base` is one aligned run start inside this live chunk.
        let pointer = unsafe { self.base.add(offset) };
        Some(RunAddress { pointer })
    }

    fn initialize_headers(&mut self) {
        for run in 0..RUNS_PER_CHUNK {
            let address = self
                .run_address(run)
                .expect("fixed run index must belong to its chunk");
            let header = address.pointer().cast::<RunHeader>();
            // SAFETY: every run begins at `RunHeader` alignment, the runs are
            // disjoint, and no typed value exists in this freshly allocated
            // zeroed storage. This initializes each header exactly once.
            unsafe { header.write(RunHeader::empty()) };
        }
    }

    fn header_for(&self, run: RunAddress) -> Option<&RunHeader> {
        if !self.range().contains(run.address()) || !run.address().is_multiple_of(RUN_SIZE) {
            return None;
        }
        // SAFETY: every live chunk initializes a valid integer-only
        // `RunHeader` at every run start before publication. The returned
        // reference is bounded by this shared chunk borrow.
        Some(unsafe { run.pointer().cast::<RunHeader>().as_ref() })
    }

    fn header_for_index(&self, run: usize) -> Option<&RunHeader> {
        self.header_for(self.run_address(run)?)
    }

    fn first_empty_run(&self) -> Option<usize> {
        (0..RUNS_PER_CHUNK).find(|&run| self.header_for_index(run).is_some_and(RunHeader::is_empty))
    }

    #[cfg(test)]
    fn first_free_slot(&self, run: usize) -> Option<usize> {
        let address = self.run_address(run)?;
        let geometry = self.header_for(address)?.geometry()?;
        (0..geometry.slot_count)
            .find(|&slot_index| !self.slot_is_allocated(address, geometry, slot_index))
    }

    fn clear_assigned_mark_bitmaps(&mut self) -> usize {
        let mut cleared = 0;
        for run_index in 0..RUNS_PER_CHUNK {
            let run = self
                .run_address(run_index)
                .expect("fixed run index must belong to its chunk");
            let Some(geometry) = self.header_for(run).and_then(RunHeader::geometry) else {
                continue;
            };
            self.clear_mark_bitmap(run, geometry);
            cleared += 1;
        }
        cleared
    }

    fn clear_mark_bitmap(&mut self, run: RunAddress, geometry: RunGeometry) {
        let bitmap = geometry.mark_bitmap;
        // SAFETY: exclusive collection gives this mutable chunk sole access
        // to the initialized ordinary mark words, and validated geometry keeps
        // the complete contiguous range inside the run.
        let words = unsafe {
            std::slice::from_raw_parts_mut(
                run.pointer().add(bitmap.offset).cast::<u64>().as_ptr(),
                bitmap.word_len,
            )
        };
        words.fill(0);
    }

    #[cfg(test)]
    fn initialize_slot<T: Trace>(
        &mut self,
        run: usize,
        class_id: AllocationClassId,
        geometry: RunGeometry,
        slot_index: usize,
        value: T,
    ) -> NonNull<T> {
        let address = self
            .run_address(run)
            .expect("published run must belong to its arena chunk");
        let header = self
            .header_for(address)
            .expect("published run must have a valid header");
        assert_eq!(
            header.class_id(),
            Some(class_id),
            "allocation run has the wrong class"
        );
        assert_eq!(
            header.geometry(),
            Some(geometry),
            "allocation run has the wrong geometry"
        );
        assert!(
            slot_index < geometry.slot_count,
            "allocation slot is outside its run"
        );
        assert!(
            !self.slot_is_allocated(address, geometry, slot_index),
            "allocation slot is already occupied"
        );
        assert!(
            std::mem::size_of::<T>() <= geometry.slot_stride,
            "allocation payload exceeds its slot"
        );

        let offset = geometry
            .slot_offset(slot_index)
            .expect("validated slot must have an in-run offset");
        // SAFETY: validated run geometry places this exact slot wholly inside
        // the live chunk, aligned for the class payload. The allocation bit is
        // still clear and the arena is exclusively borrowed under managed
        // data, so no initialized `T` aliases this storage.
        let pointer = unsafe { address.pointer().add(offset).cast::<T>() };
        assert_eq!(
            pointer.addr().get() % std::mem::align_of::<T>(),
            0,
            "allocation slot has the wrong payload alignment"
        );
        let (allocation_word, published_word) =
            self.allocation_word_update(address, geometry, slot_index);

        // No panicking operation is permitted between payload initialization
        // and publication. Both raw writes are infallible for the validated,
        // exclusively held, disjoint ranges.
        // SAFETY: the proof above establishes a unique initialized destination
        // for `T`.
        unsafe { pointer.write(value) };
        // SAFETY: `allocation_word` identifies the initialized atomic
        // allocation-bitmap word for this run. The synchronized allocator is
        // its only writer, while release publication makes the initialized
        // payload visible to root validation and later collection.
        unsafe { allocation_word.as_ref() }.store(published_word, Ordering::Release);
        pointer
    }

    fn allocated_slot_pointers(&self, run: usize) -> Vec<NonNull<()>> {
        let address = self
            .run_address(run)
            .expect("published run must belong to its arena chunk");
        let geometry = self
            .header_for(address)
            .and_then(RunHeader::geometry)
            .expect("published run must have valid geometry");
        (0..geometry.slot_count)
            .filter(|&slot_index| self.slot_is_allocated(address, geometry, slot_index))
            .map(|slot_index| {
                let offset = geometry
                    .slot_offset(slot_index)
                    .expect("allocated slot must have an in-run offset");
                // SAFETY: validated geometry and a bounded slot index place
                // the pointer at an initialized payload start inside the run.
                unsafe { address.pointer().add(offset).cast() }
            })
            .collect()
    }

    fn slot_is_allocated(&self, run: RunAddress, geometry: RunGeometry, slot_index: usize) -> bool {
        let word_index = slot_index / u64::BITS as usize;
        let bit_index = slot_index % u64::BITS as usize;
        debug_assert!(word_index < geometry.allocation_bitmap.word_len);
        let pointer = allocation_word_pointer(run, geometry, word_index);
        // SAFETY: validated geometry places this initialized atomic word
        // inside the live run. Acquire observes release publication of the
        // payload before its allocation bit.
        let word = unsafe { pointer.as_ref() }.load(Ordering::Acquire);
        word & (1_u64 << bit_index) != 0
    }

    #[cfg(test)]
    fn slot_is_marked(&self, run: RunAddress, geometry: RunGeometry, slot_index: usize) -> bool {
        let (word_index, bit) = bitmap_bit(geometry.mark_bitmap, slot_index);
        let pointer = mark_word_pointer(run, geometry, word_index);
        // SAFETY: exclusive collection keeps every mutator out while ordinary
        // mark words are read. Validated geometry places this initialized word
        // wholly inside the live run.
        let word = unsafe { pointer.read() };
        word & bit != 0
    }

    fn mark_slot(&mut self, run: RunAddress, geometry: RunGeometry, slot_index: usize) -> bool {
        let (word_index, bit) = bitmap_bit(geometry.mark_bitmap, slot_index);
        let pointer = mark_word_pointer(run, geometry, word_index);
        // SAFETY: the exclusive collector is the sole mark-word writer, and
        // validated geometry identifies one initialized ordinary `u64`.
        let prior = unsafe { pointer.read() };
        if prior & bit != 0 {
            return false;
        }
        // SAFETY: as above, this updates that same initialized mark word while
        // no mutator or other collector can access it.
        unsafe { pointer.write(prior | bit) };
        true
    }

    #[cfg(test)]
    fn allocation_word_update(
        &mut self,
        run: RunAddress,
        geometry: RunGeometry,
        slot_index: usize,
    ) -> (NonNull<AtomicU64>, u64) {
        let word_index = slot_index / u64::BITS as usize;
        let bit_index = slot_index % u64::BITS as usize;
        debug_assert!(word_index < geometry.allocation_bitmap.word_len);
        let pointer = allocation_word_pointer(run, geometry, word_index);
        // SAFETY: the synchronized allocator is the only writer to this
        // initialized word. Relaxed ordering is sufficient for its local
        // read-modify-store sequence; the later store publishes with Release.
        let current = unsafe { pointer.as_ref() }.load(Ordering::Relaxed);
        (pointer, current | (1_u64 << bit_index))
    }

    fn initialize_run(
        &mut self,
        run: usize,
        class_id: AllocationClassId,
        geometry: RunGeometry,
    ) -> Result<(), RunInitializationError> {
        let address = self
            .run_address(run)
            .ok_or(RunInitializationError::MissingRun)?;
        if !geometry.is_structurally_valid() {
            return Err(RunInitializationError::InvalidGeometry);
        }
        let header_pointer = address.pointer().cast::<RunHeader>();
        // SAFETY: `address` names this exclusively borrowed chunk's initialized
        // header. The shared view ends before any side metadata is changed.
        let header = unsafe { header_pointer.as_ref() };
        if !header.is_valid() {
            return Err(RunInitializationError::InvalidHeader);
        }
        if !header.is_empty() {
            return Err(RunInitializationError::AlreadyInitialized);
        }

        self.fill_side_metadata(run, geometry, 0);
        // SAFETY: this overwrites the initialized integer-only empty header
        // after all fallible validation and disjoint side-metadata setup.
        unsafe { header_pointer.write(RunHeader::initialized(class_id, geometry)) };
        Ok(())
    }

    fn reset_recyclable_run(&mut self, run: usize, geometry: RunGeometry) {
        let address = self
            .run_address(run)
            .expect("prevalidated recyclable run must belong to its chunk");
        let header_pointer = address.pointer().cast::<RunHeader>();

        // Reinitializing every side word is safe only because exclusive
        // collection has drained mutators and the old class record and raw
        // frontier have already been retired. The words become unpublished
        // storage again before the header loses its geometry.
        self.fill_side_metadata(run, geometry, 0);
        // SAFETY: prevalidation proved this is the initialized integer-only
        // header for `geometry`; all old side state and allocator selectors
        // are gone, so replacing it with an empty header makes the run
        // untyped without invalidating an accessible Rust reference.
        unsafe { header_pointer.write(RunHeader::empty()) };
    }

    fn fill_side_metadata(&mut self, run: usize, geometry: RunGeometry, value: u8) {
        debug_assert!(geometry.is_structurally_valid());
        let address = self
            .run_address(run)
            .expect("validated run must belong to its chunk");
        let word_value = u64::from_ne_bytes([value; std::mem::size_of::<u64>()]);
        for word_index in 0..geometry.allocation_bitmap.word_len {
            let pointer = allocation_word_pointer(address, geometry, word_index);
            // SAFETY: the run is exclusively borrowed and is not accessible
            // through an allocator selector. For a virgin or newly retyped
            // run this initializes aligned raw storage; for collector reset it
            // overwrites an inaccessible `AtomicU64`, which has no destructor.
            // Publication happens only after all side words and the typed
            // header are authoritative.
            unsafe { pointer.write(AtomicU64::new(word_value)) };
        }
        for word_index in 0..geometry.lease_bitmap.word_len {
            let pointer = lease_word_pointer(address, geometry.lease_bitmap, word_index);
            // SAFETY: the same unpublished or exclusively retired run proof
            // permits initializing or overwriting this destructor-free atomic
            // lease word before any allocator can observe it.
            unsafe { pointer.write(AtomicU64::new(word_value)) };
        }
        let bitmap = geometry.mark_bitmap;
        // SAFETY: validated geometry keeps the mark-bitmap range within this
        // live run and disjoint from its header and payload slots.
        let start = unsafe { address.pointer().add(bitmap.offset) };
        // SAFETY: the same validated byte range is live untyped arena storage
        // and may be initialized to the requested byte value.
        unsafe { std::ptr::write_bytes(start.as_ptr(), value, bitmap.byte_len()) };
    }

    #[cfg(test)]
    fn side_metadata(&self, run: usize, geometry: RunGeometry) -> Vec<u8> {
        debug_assert!(geometry.is_structurally_valid());
        let address = self.run_address(run).unwrap();
        let mut bytes = Vec::new();
        {
            let bitmap = geometry.allocation_bitmap;
            for word_index in 0..bitmap.word_len {
                let pointer = allocation_word_pointer(address, geometry, word_index);
                let word = unsafe { pointer.as_ref() }.load(Ordering::Acquire);
                bytes.extend_from_slice(&word.to_ne_bytes());
            }
        }
        for word_index in 0..geometry.lease_bitmap.word_len {
            let pointer = lease_word_pointer(address, geometry.lease_bitmap, word_index);
            let word = unsafe { pointer.as_ref() }.load(Ordering::Acquire);
            bytes.extend_from_slice(&word.to_ne_bytes());
        }
        {
            let bitmap = geometry.mark_bitmap;
            // SAFETY: the chunk remains borrowed, and validated geometry names
            // initialized bytes inside this run's side metadata.
            let start = unsafe { address.pointer().add(bitmap.offset) };
            // SAFETY: `start` and `byte_len` describe that live initialized
            // side-metadata range; it is copied before the borrow ends.
            let range = unsafe { std::slice::from_raw_parts(start.as_ptr(), bitmap.byte_len()) };
            bytes.extend_from_slice(range);
        }
        bytes
    }
}

fn free_mask_for_word(run: RunAddress, geometry: RunGeometry, word_index: usize) -> u64 {
    let allocation = allocation_word_pointer(run, geometry, word_index);
    // SAFETY: winning the corresponding lease bit grants exclusive write
    // access to this initialized atomic allocation word. Acquire also observes
    // any allocation state rebuilt before a future lease reset.
    let allocation = unsafe { allocation.as_ref() }.load(Ordering::Acquire);
    !allocation & valid_slot_mask(geometry.slot_count, word_index)
}

fn allocation_word_pointer(
    run: RunAddress,
    geometry: RunGeometry,
    word_index: usize,
) -> NonNull<AtomicU64> {
    let bitmap = geometry.allocation_bitmap;
    assert!(
        word_index < bitmap.word_len,
        "allocation word is out of range"
    );
    // SAFETY: validated geometry places every aligned allocation word wholly
    // inside this live run. Run initialization constructs an `AtomicU64` at
    // each such destination before publication.
    unsafe {
        run.pointer()
            .add(bitmap.offset)
            .cast::<AtomicU64>()
            .add(word_index)
    }
}

fn bitmap_bit(bitmap: crate::run::BitmapGeometry, bit_index: usize) -> (usize, u64) {
    assert!(bit_index < bitmap.bit_len, "bitmap bit is out of range");
    let word_index = bit_index / u64::BITS as usize;
    let bit = 1_u64 << (bit_index % u64::BITS as usize);
    debug_assert!(word_index < bitmap.word_len);
    (word_index, bit)
}

fn mark_word_pointer(run: RunAddress, geometry: RunGeometry, word_index: usize) -> NonNull<u64> {
    let bitmap = geometry.mark_bitmap;
    assert!(word_index < bitmap.word_len, "mark word is out of range");
    // SAFETY: compile-time run alignment and validated bitmap geometry place
    // this initialized ordinary word wholly inside the live run.
    unsafe {
        run.pointer()
            .add(bitmap.offset)
            .cast::<u64>()
            .add(word_index)
    }
}

fn lease_word_pointer(
    run: RunAddress,
    bitmap: crate::run::BitmapGeometry,
    word_index: usize,
) -> NonNull<AtomicU64> {
    assert!(word_index < bitmap.word_len, "lease word is out of range");
    // SAFETY: compile-time geometry assertions make the run boundary and word
    // stride suitable for `AtomicU64`; validated bitmap geometry keeps this
    // particular aligned word wholly inside the run.
    unsafe {
        run.pointer()
            .add(bitmap.offset)
            .cast::<AtomicU64>()
            .add(word_index)
    }
}

// SAFETY: an arena chunk is exclusive owned storage with no exposed Rust
// references. Moving ownership between threads does not access its bytes, and
// its one eventual deallocation still occurs exactly once from `Drop`.
unsafe impl Send for ArenaChunk {}

impl Drop for ArenaChunk {
    fn drop(&mut self) {
        // SAFETY: `base` came from `alloc_zeroed` with this exact layout and is
        // still uniquely owned by this chunk.
        unsafe { dealloc(self.base.as_ptr(), chunk_layout()) };
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AddressRange {
    start: usize,
    end: usize,
}

impl AddressRange {
    fn new(start: usize) -> Option<Self> {
        if !start.is_multiple_of(ARENA_CHUNK_SIZE) {
            return None;
        }
        Some(Self {
            start,
            end: start.checked_add(ARENA_CHUNK_SIZE)?,
        })
    }

    fn contains(self, address: usize) -> bool {
        (self.start..self.end).contains(&address)
    }

    #[cfg(test)]
    fn overlaps(self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }

    fn run_base(self, address: usize) -> Option<usize> {
        if !self.contains(address) {
            return None;
        }
        let masked = address & !(RUN_SIZE - 1);
        debug_assert!(masked >= self.start);
        debug_assert!(masked < self.end);
        Some(masked)
    }
}

fn chunk_layout() -> Layout {
    Layout::from_size_align(ARENA_CHUNK_SIZE, ARENA_CHUNK_SIZE)
        .expect("fixed arena chunk geometry must be a valid Rust layout")
}

fn valid_slot_mask(slot_count: usize, word_index: usize) -> u64 {
    let first_slot = word_index * u64::BITS as usize;
    let remaining = slot_count.saturating_sub(first_slot);
    if remaining >= u64::BITS as usize {
        u64::MAX
    } else if remaining == 0 {
        0
    } else {
        (1_u64 << remaining) - 1
    }
}

#[cfg(test)]
mod tests {
    use std::alloc::Layout;
    use std::collections::HashSet;
    use std::sync::{Arc, Barrier, Mutex};

    use super::*;

    fn geometry(size: usize, alignment: usize) -> RunGeometry {
        RunGeometry::derive(
            Layout::from_size_align(size, alignment).expect("test layout should be valid"),
            None,
        )
        .unwrap()
    }

    fn class(id: u64) -> AllocationClassId {
        AllocationClassId::new(id).expect("test class ID must be nonzero")
    }

    #[test]
    fn aligned_chunk_boundaries_map_to_exact_runs() {
        let mut arena = Arena::default();
        let chunk = arena.reserve_chunk().unwrap();
        let range = arena.chunk_range(chunk).unwrap();
        assert!(range.start.is_multiple_of(ARENA_CHUNK_SIZE));

        let first = arena.run_address(chunk, 0).unwrap();
        let second = arena.run_address(chunk, 1).unwrap();
        let last = arena.run_address(chunk, RUNS_PER_CHUNK - 1).unwrap();
        assert_eq!(first.address(), range.start);
        assert_eq!(second.address(), range.start + RUN_SIZE);
        assert_eq!(last.address(), range.end - RUN_SIZE);
        assert_eq!(arena.find_run(range.start), Some(first));
        assert_eq!(arena.find_run(range.start + RUN_SIZE - 1), Some(first));
        assert_eq!(arena.find_run(range.start + RUN_SIZE), Some(second));
        assert_eq!(arena.find_run(range.end - 1), Some(last));
        assert_eq!(arena.find_run(range.end), None);
        assert_eq!(arena.run_address(chunk, RUNS_PER_CHUNK), None);
        if let Some(before) = range.start.checked_sub(1) {
            assert_eq!(arena.find_run(before), None);
        }
    }

    #[test]
    fn live_chunks_and_their_runs_never_alias() {
        let mut arena = Arena::default();
        let first = arena.reserve_chunk().unwrap();
        let second = arena.reserve_chunk().unwrap();
        let first_range = arena.chunk_range(first).unwrap();
        let second_range = arena.chunk_range(second).unwrap();
        assert!(!first_range.overlaps(second_range));

        for run in 0..RUNS_PER_CHUNK {
            let first_run = arena.run_address(first, run).unwrap();
            let second_run = arena.run_address(second, run).unwrap();
            assert_ne!(first_run, second_run);
            assert_eq!(arena.find_run(first_run.address()), Some(first_run));
            assert_eq!(arena.find_run(second_run.address()), Some(second_run));
        }
    }

    #[test]
    fn indexed_lookup_cost_is_independent_of_chunk_count_and_order() {
        let mut arena = Arena::default();
        let chunks = (0..4)
            .map(|_| arena.reserve_chunk().unwrap())
            .collect::<Vec<_>>();

        for chunk in chunks.into_iter().rev() {
            let range = arena.chunk_range(chunk).unwrap();
            for address in [range.start, range.end - 1] {
                arena.reset_indexed_lookup_count();
                assert!(arena.find_run(address).is_some());
                assert_eq!(arena.indexed_lookup_count(), 1);
            }
        }

        let arbitrary = 1usize;
        arena.reset_indexed_lookup_count();
        assert_eq!(arena.find_run(arbitrary), None);
        assert_eq!(arena.indexed_lookup_count(), 1);
    }

    #[test]
    fn independent_arenas_reject_each_others_ranges() {
        let mut first = Arena::default();
        let mut second = Arena::default();
        let first_chunk = first.reserve_chunk().unwrap();
        let second_chunk = second.reserve_chunk().unwrap();
        let first_run = first.run_address(first_chunk, 0).unwrap();
        let second_run = second.run_address(second_chunk, 0).unwrap();

        assert_eq!(first.find_run(second_run.address()), None);
        assert_eq!(second.find_run(first_run.address()), None);
    }

    #[test]
    fn numeric_masking_handles_the_highest_representable_chunk_range() {
        let high_start = (usize::MAX - 2 * ARENA_CHUNK_SIZE + 1) & !(ARENA_CHUNK_SIZE - 1);
        let range = AddressRange::new(high_start).unwrap();
        assert_eq!(range.run_base(range.start), Some(range.start));
        assert_eq!(range.run_base(range.end - 1), Some(range.end - RUN_SIZE));
        assert_eq!(range.run_base(range.end), None);
        assert_eq!(
            AddressRange::new(usize::MAX & !(ARENA_CHUNK_SIZE - 1)),
            None
        );
    }

    #[test]
    fn every_reserved_run_begins_with_an_empty_valid_header() {
        let mut arena = Arena::default();
        let chunk = arena.reserve_chunk().unwrap();
        for run in 0..RUNS_PER_CHUNK {
            let header = arena.header_for_test(chunk, run).unwrap();
            assert!(header.is_valid());
            assert!(header.is_empty());
            assert_eq!(header.class_id(), None);
            assert_eq!(header.geometry(), None);
        }
    }

    #[test]
    fn initialized_headers_and_zeroed_side_metadata_describe_exact_slots() {
        let mut arena = Arena::default();
        let chunk = arena.reserve_chunk().unwrap();
        let geometry = geometry(24, 8);
        arena.fill_side_metadata_for_test(chunk, 0, geometry, 0xff);
        arena.initialize_run(chunk, 0, class(7), geometry).unwrap();

        let header = arena.header_for_test(chunk, 0).unwrap();
        assert_eq!(header.class_id(), Some(class(7)));
        assert_eq!(header.geometry(), Some(geometry));
        assert!(
            arena
                .side_metadata_for_test(chunk, 0, geometry)
                .iter()
                .all(|byte| *byte == 0)
        );

        let run = arena.run_address(chunk, 0).unwrap();
        let first = run.address() + geometry.first_slot_offset;
        let last_index = geometry.slot_count - 1;
        let last = run.address() + geometry.slot_offset(last_index).unwrap();
        assert_eq!(arena.checked_slot_owner(first).unwrap().slot_index, 0);
        assert_eq!(
            arena.checked_slot_owner(last).unwrap().slot_index,
            last_index
        );
        assert_eq!(arena.checked_slot_owner(run.address()), None);
        assert_eq!(arena.checked_slot_owner(first - 1), None);
        assert_eq!(arena.checked_slot_owner(first + 1), None);
        assert_eq!(arena.checked_slot_owner(run.address() + RUN_SIZE), None);
    }

    #[test]
    fn adjacent_initialized_runs_keep_distinct_classes_and_geometry() {
        let mut arena = Arena::default();
        let first_chunk = arena.reserve_chunk().unwrap();
        let second_chunk = arena.reserve_chunk().unwrap();
        let first_geometry = geometry(16, 8);
        let second_geometry = geometry(32, 8);

        arena
            .initialize_run(first_chunk, RUNS_PER_CHUNK - 1, class(11), first_geometry)
            .unwrap();
        arena
            .initialize_run(second_chunk, 0, class(12), second_geometry)
            .unwrap();

        let first_run = arena.run_address(first_chunk, RUNS_PER_CHUNK - 1).unwrap();
        let second_run = arena.run_address(second_chunk, 0).unwrap();
        let first_owner = arena
            .checked_slot_owner(first_run.address() + first_geometry.first_slot_offset)
            .unwrap();
        let second_owner = arena
            .checked_slot_owner(second_run.address() + second_geometry.first_slot_offset)
            .unwrap();
        assert_eq!(first_owner.class_id, class(11));
        assert_eq!(first_owner.geometry, first_geometry);
        assert_eq!(first_owner.run, first_run);
        assert_eq!(second_owner.class_id, class(12));
        assert_eq!(second_owner.geometry, second_geometry);
        assert_eq!(second_owner.run, second_run);
    }

    #[test]
    fn indexed_owner_lookup_resolves_boundary_slots_across_chunks() {
        let mut arena = Arena::default();
        let chunks = (0..3)
            .map(|_| arena.reserve_chunk().unwrap())
            .collect::<Vec<_>>();
        let geometry = geometry(24, 8);

        for (ordinal, &chunk) in chunks.iter().enumerate() {
            for run in [0, RUNS_PER_CHUNK - 1] {
                let class_id = class((ordinal * 2 + usize::from(run != 0) + 1) as u64);
                arena
                    .initialize_run(chunk, run, class_id, geometry)
                    .unwrap();
                let run_address = arena.run_address(chunk, run).unwrap();
                for slot_index in [0, geometry.slot_count - 1] {
                    let address = run_address.address() + geometry.slot_offset(slot_index).unwrap();
                    arena.reset_indexed_lookup_count();
                    let owner = arena.checked_slot_owner(address).unwrap();
                    assert_eq!(owner.location, RunLocation { chunk, run });
                    assert_eq!(owner.class_id, class_id);
                    assert_eq!(owner.slot_index, slot_index);
                    assert_eq!(arena.indexed_lookup_count(), 1);
                }
            }
        }
    }

    #[test]
    fn failed_or_repeated_initialization_does_not_republish_a_run() {
        let mut arena = Arena::default();
        let chunk = arena.reserve_chunk().unwrap();
        let geometry = geometry(16, 8);
        let mut invalid = geometry;
        invalid.slot_stride = 0;

        assert_eq!(
            arena.initialize_run(chunk, 0, class(1), invalid),
            Err(RunInitializationError::InvalidGeometry)
        );
        assert!(arena.header_for_test(chunk, 0).unwrap().is_empty());

        arena.initialize_run(chunk, 0, class(1), geometry).unwrap();
        assert_eq!(
            arena.initialize_run(chunk, 0, class(2), geometry),
            Err(RunInitializationError::AlreadyInitialized)
        );
        assert_eq!(
            arena.header_for_test(chunk, 0).unwrap().class_id(),
            Some(class(1))
        );
        assert_eq!(
            arena.initialize_run(chunk, RUNS_PER_CHUNK, class(2), geometry),
            Err(RunInitializationError::MissingRun)
        );
        assert_eq!(
            arena.initialize_run(chunk + 1, 0, class(2), geometry),
            Err(RunInitializationError::MissingChunk)
        );
    }

    #[test]
    fn failed_publication_adds_neither_a_chunk_nor_a_typed_run() {
        let mut arena = Arena::default();
        let mut invalid = geometry(32, 8);
        invalid.slot_count = 0;

        assert_eq!(
            arena.publish_run(class(1), invalid),
            Err(RunPublicationError::Initialization(
                RunInitializationError::InvalidGeometry
            ))
        );
        assert!(arena.chunks.is_empty());
        assert!(arena.chunk_indices.is_empty());
        assert!(arena.initialized_runs().is_empty());

        let published = arena.publish_run(class(1), geometry(32, 8)).unwrap();
        assert_eq!(published, RunLocation { chunk: 0, run: 0 });
        assert_eq!(arena.chunks.len(), arena.chunk_indices.len());
        assert_eq!(arena.initialized_runs().len(), 1);
    }

    #[test]
    fn allocation_word_leases_are_disjoint_and_exhaust_the_run() {
        let mut arena = Arena::default();
        let geometry = geometry(16, 8);
        let location = arena.publish_run(class(1), geometry).unwrap();

        let claims = (0..geometry.allocation_bitmap.word_len)
            .map(|expected_word| {
                let claimed = arena
                    .claim_allocation_word(location)
                    .expect("each allocation word should be claimable once");
                assert_eq!(claimed.location, location);
                assert_eq!(claimed.word_index, expected_word);
                assert_eq!(
                    claimed.free_mask,
                    valid_slot_mask(geometry.slot_count, expected_word)
                );
                claimed
            })
            .collect::<Vec<_>>();

        assert_eq!(claims.len(), geometry.allocation_bitmap.word_len);
        assert!(arena.claim_allocation_word(location).is_none());
    }

    #[test]
    fn concurrent_claimers_atomically_partition_one_run() {
        const THREADS: usize = 12;

        let mut arena = Arena::default();
        let geometry = geometry(1, 1);
        assert!(geometry.lease_bitmap.word_len > 1);
        assert!(
            !geometry
                .lease_bitmap
                .bit_len
                .is_multiple_of(u64::BITS as usize)
        );
        let location = arena.publish_run(class(1), geometry).unwrap();
        let target = arena.run_claim_target(location).unwrap();
        let barrier = Arc::new(Barrier::new(THREADS));
        let claimed = Arc::new(Mutex::new(Vec::new()));

        std::thread::scope(|scope| {
            for _ in 0..THREADS {
                let barrier = Arc::clone(&barrier);
                let claimed = Arc::clone(&claimed);
                scope.spawn(move || {
                    barrier.wait();
                    let mut local = Vec::new();
                    while let Some(word) = target.claim_allocation_word() {
                        local.push(word.word_index);
                    }
                    claimed.lock().unwrap().extend(local);
                });
            }
        });

        let claimed = claimed.lock().unwrap();
        assert_eq!(claimed.len(), geometry.allocation_bitmap.word_len);
        assert_eq!(
            claimed.iter().copied().collect::<HashSet<_>>().len(),
            geometry.allocation_bitmap.word_len
        );
        assert!(
            claimed
                .iter()
                .all(|&word| word < geometry.allocation_bitmap.word_len)
        );
    }

    #[test]
    fn full_word_stays_leased_and_claiming_continues() {
        let mut arena = Arena::default();
        let geometry = geometry(16, 8);
        assert!(geometry.allocation_bitmap.word_len > 1);
        let location = arena.publish_run(class(1), geometry).unwrap();
        let target = arena.run_claim_target(location).unwrap();
        let first_allocation = allocation_word_pointer(target.run, target.geometry, 0);
        // SAFETY: the run is not shared yet, and the first atomic allocation
        // word is initialized, unleased storage in this test-owned arena.
        unsafe { first_allocation.as_ref() }.store(u64::MAX, Ordering::Release);

        let claimed = target.claim_allocation_word().unwrap();
        assert_eq!(claimed.word_index, 1);
        let lease = lease_word_pointer(target.run, target.geometry.lease_bitmap, 0);
        let lease = unsafe { lease.as_ref() }.load(Ordering::Acquire);
        assert_eq!(lease & 0b11, 0b11);
    }

    #[test]
    fn allocation_word_tail_mask_never_exposes_padding_as_a_slot() {
        let mut arena = Arena::default();
        let geometry = geometry(24, 8);
        assert!(!geometry.slot_count.is_multiple_of(u64::BITS as usize));
        let location = arena.publish_run(class(1), geometry).unwrap();

        let last = (0..geometry.allocation_bitmap.word_len)
            .map_while(|_| arena.claim_allocation_word(location))
            .last()
            .unwrap();
        let expected = valid_slot_mask(geometry.slot_count, last.word_index);
        assert_eq!(last.free_mask, expected);
        assert_ne!(expected, u64::MAX);
        assert_eq!(last.free_mask & !expected, 0);
    }
}
