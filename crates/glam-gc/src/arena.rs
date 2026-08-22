use std::alloc::{Layout, alloc_zeroed, dealloc};
#[cfg(test)]
use std::cell::Cell;
use std::collections::HashMap;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};

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
            // Acquire pairs with publication of a stable run record. The CAS
            // is the ownership transition for the selected allocation word.
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
// one successful lease bit grants one thread exclusive access to the matching
// ordinary allocation word. Private callers retain the heap while using it.
unsafe impl Send for RunClaimTarget {}
// SAFETY: the same published-run and atomic-lease invariants permit concurrent
// claims through shared copies of this immutable topology record.
unsafe impl Sync for RunClaimTarget {}

impl Arena {
    pub(crate) fn reserve_chunk(&mut self) -> Result<usize, ArenaError> {
        let candidate = ArenaChunk::allocate()?;
        self.publish_chunk(candidate)
    }

    pub(crate) fn run_address(&self, chunk: usize, run: usize) -> Option<RunAddress> {
        self.chunks.get(chunk)?.run_address(run)
    }

    pub(crate) fn run_at(&self, location: RunLocation) -> Option<RunAddress> {
        self.run_address(location.chunk, location.run)
    }

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

    pub(crate) fn first_free_slot(&self, location: RunLocation) -> Option<usize> {
        self.chunks
            .get(location.chunk)?
            .first_free_slot(location.run)
    }

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

    fn publish_chunk(&mut self, candidate: ArenaChunk) -> Result<usize, ArenaError> {
        let base = candidate.range().start;
        if self.chunk_indices.contains_key(&base) {
            return Err(ArenaError::AddressOverlap);
        }

        // Reserve both fallible allocations before either collection exposes
        // the candidate. From here through insertion, the operations cannot
        // allocate, and the arena is exclusively borrowed under the heap-state
        // mutex. Equal aligned bases are the only way fixed-size chunks can
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

    fn first_free_slot(&self, run: usize) -> Option<usize> {
        let address = self.run_address(run)?;
        let geometry = self.header_for(address)?.geometry()?;
        (0..geometry.slot_count)
            .find(|&slot_index| !self.slot_is_allocated(address, geometry, slot_index))
    }

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
        // still clear and the arena is exclusively borrowed under heap state,
        // so no initialized `T` aliases this storage.
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
        // SAFETY: `allocation_word` identifies the initialized allocation
        // bitmap word for this run, and exclusive arena access serializes the
        // synchronized allocator.
        unsafe { allocation_word.write(published_word) };
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

    fn read_bitmap_word(
        &self,
        run: RunAddress,
        bitmap: crate::run::BitmapGeometry,
        word_index: usize,
    ) -> u64 {
        let pointer = self.bitmap_word_pointer(run, bitmap, word_index);
        // SAFETY: the pointer names one initialized aligned bitmap word. Heap
        // state excludes lease mutation, and an unleased allocation word has
        // no worker-local writer.
        unsafe { pointer.read() }
    }

    fn bitmap_word_pointer(
        &self,
        run: RunAddress,
        bitmap: crate::run::BitmapGeometry,
        word_index: usize,
    ) -> NonNull<u64> {
        assert!(word_index < bitmap.word_len, "bitmap word is out of range");
        // SAFETY: validated bitmap geometry places every aligned word wholly
        // inside this live run.
        unsafe {
            run.pointer()
                .add(bitmap.offset)
                .cast::<u64>()
                .add(word_index)
        }
    }

    fn slot_is_allocated(&self, run: RunAddress, geometry: RunGeometry, slot_index: usize) -> bool {
        let word_index = slot_index / u64::BITS as usize;
        let bit_index = slot_index % u64::BITS as usize;
        debug_assert!(word_index < geometry.allocation_bitmap.word_len);
        // SAFETY: validated geometry places the aligned allocation bitmap word
        // inside this initialized live run. Shared reads occur only while the
        // heap-state mutex excludes bitmap mutation.
        let word = unsafe {
            run.pointer()
                .add(geometry.allocation_bitmap.offset)
                .cast::<u64>()
                .add(word_index)
                .read()
        };
        word & (1_u64 << bit_index) != 0
    }

    fn allocation_word_update(
        &mut self,
        run: RunAddress,
        geometry: RunGeometry,
        slot_index: usize,
    ) -> (NonNull<u64>, u64) {
        let word_index = slot_index / u64::BITS as usize;
        let bit_index = slot_index % u64::BITS as usize;
        debug_assert!(word_index < geometry.allocation_bitmap.word_len);
        // SAFETY: validated geometry places the aligned allocation bitmap word
        // inside this exclusively borrowed, initialized run.
        let pointer = unsafe {
            run.pointer()
                .add(geometry.allocation_bitmap.offset)
                .cast::<u64>()
                .add(word_index)
        };
        // SAFETY: the pointer proof above permits reading this initialized
        // integer bitmap word under exclusive arena access.
        let current = unsafe { pointer.read() };
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

    fn fill_side_metadata(&mut self, run: usize, geometry: RunGeometry, value: u8) {
        debug_assert!(geometry.is_structurally_valid());
        let address = self
            .run_address(run)
            .expect("validated run must belong to its chunk");
        for bitmap in [geometry.allocation_bitmap, geometry.mark_bitmap] {
            // SAFETY: validated geometry keeps each side-bitmap range within
            // this live run and disjoint from its header and payload slots.
            let start = unsafe { address.pointer().add(bitmap.offset) };
            // SAFETY: the same validated byte range is live untyped arena
            // storage and may be initialized to the requested byte value.
            unsafe { std::ptr::write_bytes(start.as_ptr(), value, bitmap.byte_len()) };
        }
        let lease_value = u64::from_ne_bytes([value; std::mem::size_of::<u64>()]);
        for word_index in 0..geometry.lease_bitmap.word_len {
            let pointer = lease_word_pointer(address, geometry.lease_bitmap, word_index);
            // SAFETY: the run is exclusively borrowed and not yet published
            // with this geometry. Each aligned raw lease-word destination is
            // initialized exactly once as an atomic value before publication.
            unsafe { pointer.write(AtomicU64::new(lease_value)) };
        }
    }

    #[cfg(test)]
    fn side_metadata(&self, run: usize, geometry: RunGeometry) -> Vec<u8> {
        debug_assert!(geometry.is_structurally_valid());
        let address = self.run_address(run).unwrap();
        let mut bytes = Vec::new();
        {
            let bitmap = geometry.allocation_bitmap;
            // SAFETY: the chunk remains borrowed, and validated geometry names
            // initialized bytes inside this run's side metadata.
            let start = unsafe { address.pointer().add(bitmap.offset) };
            // SAFETY: `start` and `byte_len` describe that live initialized
            // side-metadata range; it is copied before the borrow ends.
            let range = unsafe { std::slice::from_raw_parts(start.as_ptr(), bitmap.byte_len()) };
            bytes.extend_from_slice(range);
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
    let allocation = bitmap_word_pointer(run, geometry.allocation_bitmap, word_index);
    // SAFETY: winning the corresponding lease bit grants exclusive access to
    // this initialized ordinary allocation word.
    let allocation = unsafe { allocation.read() };
    !allocation & valid_slot_mask(geometry.slot_count, word_index)
}

fn bitmap_word_pointer(
    run: RunAddress,
    bitmap: crate::run::BitmapGeometry,
    word_index: usize,
) -> NonNull<u64> {
    assert!(word_index < bitmap.word_len, "bitmap word is out of range");
    // SAFETY: validated bitmap geometry places every aligned word wholly
    // inside this live run.
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
        let first_allocation =
            bitmap_word_pointer(target.run, target.geometry.allocation_bitmap, 0);
        // SAFETY: the run is not shared yet, and the first ordinary allocation
        // word is initialized, unleased storage in this test-owned arena.
        unsafe { first_allocation.write(u64::MAX) };

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
