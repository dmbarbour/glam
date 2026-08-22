use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::ptr::NonNull;

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
}

#[derive(Default)]
pub(crate) struct Arena {
    chunks: Vec<ArenaChunk>,
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

impl Arena {
    pub(crate) fn reserve_chunk(&mut self) -> Result<usize, ArenaError> {
        let candidate = ArenaChunk::allocate()?;
        if self
            .chunks
            .iter()
            .any(|chunk| chunk.range().overlaps(candidate.range()))
        {
            return Err(ArenaError::AddressOverlap);
        }

        let index = self.chunks.len();
        self.chunks.push(candidate);
        Ok(index)
    }

    pub(crate) fn run_address(&self, chunk: usize, run: usize) -> Option<RunAddress> {
        self.chunks.get(chunk)?.run_address(run)
    }

    pub(crate) fn run_at(&self, location: RunLocation) -> Option<RunAddress> {
        self.run_address(location.chunk, location.run)
    }

    pub(crate) fn find_run(&self, address: usize) -> Option<RunAddress> {
        self.chunks
            .iter()
            .find_map(|chunk| chunk.run_containing(address))
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
        if self
            .chunks
            .iter()
            .any(|chunk| chunk.range().overlaps(candidate.range()))
        {
            return Err(RunPublicationError::Arena(ArenaError::AddressOverlap));
        }
        let chunk = self.chunks.len();
        self.chunks.push(candidate);
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
        let (chunk_index, chunk) = self
            .chunks
            .iter()
            .enumerate()
            .find(|(_, chunk)| chunk.range().contains(address))?;
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
        for bitmap in [
            geometry.allocation_bitmap,
            geometry.lease_bitmap,
            geometry.mark_bitmap,
        ] {
            // SAFETY: validated geometry keeps each side-bitmap range within
            // this live run and disjoint from its header and payload slots.
            let start = unsafe { address.pointer().add(bitmap.offset) };
            // SAFETY: the same validated byte range is live untyped arena
            // storage and may be initialized to the requested byte value.
            unsafe { std::ptr::write_bytes(start.as_ptr(), value, bitmap.byte_len()) };
        }
    }

    #[cfg(test)]
    fn side_metadata(&self, run: usize, geometry: RunGeometry) -> Vec<u8> {
        debug_assert!(geometry.is_structurally_valid());
        let address = self.run_address(run).unwrap();
        let mut bytes = Vec::new();
        for bitmap in [
            geometry.allocation_bitmap,
            geometry.lease_bitmap,
            geometry.mark_bitmap,
        ] {
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

#[cfg(test)]
mod tests {
    use std::alloc::Layout;

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
        assert!(arena.initialized_runs().is_empty());

        let published = arena.publish_run(class(1), geometry(32, 8)).unwrap();
        assert_eq!(published, RunLocation { chunk: 0, run: 0 });
        assert_eq!(arena.initialized_runs().len(), 1);
    }
}
