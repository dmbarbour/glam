use std::alloc::Layout;
use std::num::NonZeroU64;

pub(crate) const RUN_SIZE: usize = 64 * 1024;
pub(crate) const RUN_HEADER_SIZE: usize = 64;
pub(crate) const METADATA_PAYLOAD_ALIGNMENT: usize = 128;
const BITMAP_WORD_BITS: usize = u64::BITS as usize;
const BITMAP_WORD_SIZE: usize = std::mem::size_of::<u64>();

/// Largest payload which can occupy a minimally aligned one-slot run.
pub(crate) const MAX_MANAGED_SIZE: usize = RUN_SIZE - METADATA_PAYLOAD_ALIGNMENT;

/// Largest Rust alignment for which one slot can begin and end in one run.
pub(crate) const MAX_MANAGED_ALIGNMENT: usize = RUN_SIZE / 2;

const _: () = assert!(RUN_SIZE.is_power_of_two());
const _: () = assert!(RUN_HEADER_SIZE.is_multiple_of(BITMAP_WORD_SIZE));
const _: () = assert!(METADATA_PAYLOAD_ALIGNMENT.is_power_of_two());
const _: () = assert!(RUN_HEADER_SIZE + 3 * BITMAP_WORD_SIZE <= METADATA_PAYLOAD_ALIGNMENT);

const RUN_HEADER_MAGIC: u64 = 0x474c_414d_5255_4e31;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AllocationClassId(NonZeroU64);

impl AllocationClassId {
    pub(crate) fn new(id: u64) -> Option<Self> {
        NonZeroU64::new(id).map(Self)
    }

    pub(crate) fn get(self) -> u64 {
        self.0.get()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GeometryError {
    ZeroSized,
    RequestedSlotTooSmall,
    ArithmeticOverflow,
    NoSlots,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct BitmapGeometry {
    pub(crate) offset: usize,
    pub(crate) bit_len: usize,
    pub(crate) word_len: usize,
}

impl BitmapGeometry {
    pub(crate) fn byte_len(self) -> usize {
        self.word_len * BITMAP_WORD_SIZE
    }

    pub(crate) fn end(self) -> usize {
        self.offset + self.byte_len()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RunGeometry {
    pub(crate) slot_stride: usize,
    pub(crate) first_slot_offset: usize,
    pub(crate) slot_count: usize,
    pub(crate) allocation_bitmap: BitmapGeometry,
    pub(crate) lease_bitmap: BitmapGeometry,
    pub(crate) mark_bitmap: BitmapGeometry,
}

impl RunGeometry {
    pub(crate) fn derive(
        payload: Layout,
        requested_slot_size: Option<usize>,
    ) -> Result<Self, GeometryError> {
        if payload.size() == 0 {
            return Err(GeometryError::ZeroSized);
        }

        let requested_slot_size = requested_slot_size.unwrap_or(payload.size());
        if requested_slot_size < payload.size() {
            return Err(GeometryError::RequestedSlotTooSmall);
        }
        let slot_stride = align_up(requested_slot_size, payload.align())
            .ok_or(GeometryError::ArithmeticOverflow)?;
        let maximum_slots = (RUN_SIZE - RUN_HEADER_SIZE) / slot_stride;
        if maximum_slots == 0 {
            return Err(GeometryError::NoSlots);
        }

        let mut lower = 1;
        let mut upper = maximum_slots;
        let mut best = None;
        while lower <= upper {
            let candidate = lower + (upper - lower) / 2;
            match Self::for_slot_count(slot_stride, payload.align(), candidate)? {
                Some(geometry) => {
                    best = Some(geometry);
                    lower = candidate + 1;
                }
                None => upper = candidate - 1,
            }
        }

        best.ok_or(GeometryError::NoSlots)
    }

    fn for_slot_count(
        slot_stride: usize,
        payload_alignment: usize,
        slot_count: usize,
    ) -> Result<Option<Self>, GeometryError> {
        let allocation_words = bitmap_words(slot_count);
        let lease_words = bitmap_words(allocation_words);

        let allocation_bitmap = BitmapGeometry {
            offset: RUN_HEADER_SIZE,
            bit_len: slot_count,
            word_len: allocation_words,
        };
        let lease_bitmap = BitmapGeometry {
            offset: allocation_bitmap.end(),
            bit_len: allocation_words,
            word_len: lease_words,
        };
        let mark_bitmap = BitmapGeometry {
            offset: lease_bitmap.end(),
            bit_len: slot_count,
            word_len: allocation_words,
        };
        let first_slot_offset = align_up(
            mark_bitmap.end(),
            payload_alignment.max(METADATA_PAYLOAD_ALIGNMENT),
        )
        .ok_or(GeometryError::ArithmeticOverflow)?;
        let slots_end = slot_count
            .checked_mul(slot_stride)
            .and_then(|bytes| first_slot_offset.checked_add(bytes))
            .ok_or(GeometryError::ArithmeticOverflow)?;
        if slots_end > RUN_SIZE {
            return Ok(None);
        }

        Ok(Some(Self {
            slot_stride,
            first_slot_offset,
            slot_count,
            allocation_bitmap,
            lease_bitmap,
            mark_bitmap,
        }))
    }

    pub(crate) fn side_metadata_bytes(self) -> usize {
        self.mark_bitmap.end() - RUN_HEADER_SIZE
    }

    pub(crate) fn slot_index(self, run_offset: usize) -> Option<usize> {
        let slot_offset = run_offset.checked_sub(self.first_slot_offset)?;
        if !slot_offset.is_multiple_of(self.slot_stride) {
            return None;
        }
        let index = slot_offset / self.slot_stride;
        (index < self.slot_count).then_some(index)
    }

    pub(crate) fn slot_offset(self, index: usize) -> Option<usize> {
        if index >= self.slot_count {
            return None;
        }
        index
            .checked_mul(self.slot_stride)
            .and_then(|offset| self.first_slot_offset.checked_add(offset))
    }

    pub(crate) fn is_structurally_valid(self) -> bool {
        if self.slot_stride == 0 || self.slot_count == 0 {
            return false;
        }
        let allocation_words = bitmap_words(self.slot_count);
        let lease_words = bitmap_words(allocation_words);
        let allocation_bitmap = BitmapGeometry {
            offset: RUN_HEADER_SIZE,
            bit_len: self.slot_count,
            word_len: allocation_words,
        };
        if self.allocation_bitmap != allocation_bitmap {
            return false;
        }
        let Some(allocation_end) = checked_bitmap_end(allocation_bitmap) else {
            return false;
        };
        let lease_bitmap = BitmapGeometry {
            offset: allocation_end,
            bit_len: allocation_words,
            word_len: lease_words,
        };
        if self.lease_bitmap != lease_bitmap {
            return false;
        }
        let Some(lease_end) = checked_bitmap_end(lease_bitmap) else {
            return false;
        };
        let mark_bitmap = BitmapGeometry {
            offset: lease_end,
            bit_len: self.slot_count,
            word_len: allocation_words,
        };
        if self.mark_bitmap != mark_bitmap {
            return false;
        }
        let Some(mark_end) = checked_bitmap_end(mark_bitmap) else {
            return false;
        };
        if self.first_slot_offset < mark_end
            || !self
                .first_slot_offset
                .is_multiple_of(METADATA_PAYLOAD_ALIGNMENT)
        {
            return false;
        }
        self.slot_count
            .checked_mul(self.slot_stride)
            .and_then(|bytes| self.first_slot_offset.checked_add(bytes))
            .is_some_and(|end| end <= RUN_SIZE)
    }
}

#[repr(C, align(64))]
pub(crate) struct RunHeader {
    magic: u64,
    class_id: u64,
    slot_stride: u32,
    first_slot_offset: u32,
    slot_count: u32,
    allocation_bitmap_offset: u32,
    allocation_bitmap_words: u32,
    lease_bitmap_offset: u32,
    lease_bitmap_words: u32,
    mark_bitmap_offset: u32,
    mark_bitmap_words: u32,
    reserved: [u8; 12],
}

const _: () = assert!(std::mem::size_of::<RunHeader>() == RUN_HEADER_SIZE);
const _: () = assert!(std::mem::align_of::<RunHeader>() == RUN_HEADER_SIZE);

impl RunHeader {
    pub(crate) const fn empty() -> Self {
        Self {
            magic: RUN_HEADER_MAGIC,
            class_id: 0,
            slot_stride: 0,
            first_slot_offset: 0,
            slot_count: 0,
            allocation_bitmap_offset: 0,
            allocation_bitmap_words: 0,
            lease_bitmap_offset: 0,
            lease_bitmap_words: 0,
            mark_bitmap_offset: 0,
            mark_bitmap_words: 0,
            reserved: [0; 12],
        }
    }

    pub(crate) fn initialized(class_id: AllocationClassId, geometry: RunGeometry) -> Self {
        debug_assert!(geometry.is_structurally_valid());
        Self {
            magic: RUN_HEADER_MAGIC,
            class_id: class_id.get(),
            slot_stride: narrow(geometry.slot_stride),
            first_slot_offset: narrow(geometry.first_slot_offset),
            slot_count: narrow(geometry.slot_count),
            allocation_bitmap_offset: narrow(geometry.allocation_bitmap.offset),
            allocation_bitmap_words: narrow(geometry.allocation_bitmap.word_len),
            lease_bitmap_offset: narrow(geometry.lease_bitmap.offset),
            lease_bitmap_words: narrow(geometry.lease_bitmap.word_len),
            mark_bitmap_offset: narrow(geometry.mark_bitmap.offset),
            mark_bitmap_words: narrow(geometry.mark_bitmap.word_len),
            reserved: [0; 12],
        }
    }

    pub(crate) fn is_valid(&self) -> bool {
        self.magic == RUN_HEADER_MAGIC
    }

    pub(crate) fn class_id(&self) -> Option<AllocationClassId> {
        if !self.is_valid() {
            return None;
        }
        AllocationClassId::new(self.class_id)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.is_valid() && self.class_id == 0
    }

    pub(crate) fn geometry(&self) -> Option<RunGeometry> {
        self.class_id()?;
        let slot_count = self.slot_count as usize;
        let allocation_words = self.allocation_bitmap_words as usize;
        let geometry = RunGeometry {
            slot_stride: self.slot_stride as usize,
            first_slot_offset: self.first_slot_offset as usize,
            slot_count,
            allocation_bitmap: BitmapGeometry {
                offset: self.allocation_bitmap_offset as usize,
                bit_len: slot_count,
                word_len: allocation_words,
            },
            lease_bitmap: BitmapGeometry {
                offset: self.lease_bitmap_offset as usize,
                bit_len: allocation_words,
                word_len: self.lease_bitmap_words as usize,
            },
            mark_bitmap: BitmapGeometry {
                offset: self.mark_bitmap_offset as usize,
                bit_len: slot_count,
                word_len: self.mark_bitmap_words as usize,
            },
        };
        geometry.is_structurally_valid().then_some(geometry)
    }
}

fn narrow(value: usize) -> u32 {
    u32::try_from(value).expect("fixed run geometry always fits in u32")
}

fn checked_bitmap_end(bitmap: BitmapGeometry) -> Option<usize> {
    bitmap
        .word_len
        .checked_mul(BITMAP_WORD_SIZE)
        .and_then(|bytes| bitmap.offset.checked_add(bytes))
}

fn bitmap_words(bits: usize) -> usize {
    bits.div_ceil(BITMAP_WORD_BITS)
}

fn align_up(value: usize, alignment: usize) -> Option<usize> {
    debug_assert!(alignment.is_power_of_two());
    value
        .checked_add(alignment - 1)
        .map(|rounded| rounded & !(alignment - 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(size: usize, alignment: usize) -> Layout {
        Layout::from_size_align(size, alignment).expect("test layout should be valid")
    }

    #[test]
    fn representative_layouts_share_one_run_size_and_derive_independent_slots() {
        let sixteen = RunGeometry::derive(layout(16, 8), None).unwrap();
        let twenty_four = RunGeometry::derive(layout(24, 8), None).unwrap();
        let thirty_two = RunGeometry::derive(layout(32, 8), None).unwrap();

        assert_eq!(sixteen.slot_stride, 16);
        assert_eq!(twenty_four.slot_stride, 24);
        assert_eq!(thirty_two.slot_stride, 32);
        assert!(sixteen.slot_count > twenty_four.slot_count);
        assert!(twenty_four.slot_count > thirty_two.slot_count);
        for geometry in [sixteen, twenty_four, thirty_two] {
            assert!(geometry.first_slot_offset < RUN_SIZE);
            let last = geometry.slot_offset(geometry.slot_count - 1).unwrap();
            assert!(last + geometry.slot_stride <= RUN_SIZE);
        }
    }

    #[test]
    fn requested_total_slot_extent_is_not_additive_and_rounds_for_alignment() {
        let geometry = RunGeometry::derive(layout(8, 8), Some(24)).unwrap();
        assert_eq!(geometry.slot_stride, 24);
        assert!(geometry.first_slot_offset.is_multiple_of(8));

        let rounded = RunGeometry::derive(layout(16, 16), Some(17)).unwrap();
        assert_eq!(rounded.slot_stride, 32);
        assert!(rounded.first_slot_offset.is_multiple_of(16));

        assert_eq!(
            RunGeometry::derive(layout(16, 8), Some(15)),
            Err(GeometryError::RequestedSlotTooSmall)
        );
    }

    #[test]
    fn payload_slots_do_not_share_a_128_byte_region_with_side_metadata() {
        for payload in [layout(1, 1), layout(8, 8), layout(24, 8), layout(256, 256)] {
            let geometry = RunGeometry::derive(payload, None).unwrap();
            assert!(
                geometry
                    .first_slot_offset
                    .is_multiple_of(METADATA_PAYLOAD_ALIGNMENT.max(payload.align()))
            );
            assert!(
                (geometry.mark_bitmap.end() - 1) / METADATA_PAYLOAD_ALIGNMENT
                    < geometry.first_slot_offset / METADATA_PAYLOAD_ALIGNMENT
            );
        }
    }

    #[test]
    fn structural_validation_rejects_an_unaligned_metadata_payload_boundary() {
        let mut geometry = RunGeometry::derive(layout(24, 8), None).unwrap();
        geometry.first_slot_offset -= 1;
        assert!(geometry.first_slot_offset >= geometry.mark_bitmap.end());
        assert!(!geometry.is_structurally_valid());
    }

    #[test]
    fn small_slots_pay_bitmap_overhead_instead_of_being_rejected() {
        let byte = RunGeometry::derive(layout(1, 1), None).unwrap();
        let word = RunGeometry::derive(layout(32, 8), None).unwrap();

        assert_eq!(byte.slot_stride, 1);
        assert!(byte.slot_count > word.slot_count);
        assert!(byte.side_metadata_bytes() > word.side_metadata_bytes());
        assert_eq!(byte.lease_bitmap.bit_len, byte.allocation_bitmap.word_len);
        assert_eq!(byte.mark_bitmap.bit_len, byte.slot_count);
    }

    #[test]
    fn slot_indices_reject_metadata_padding_interiors_and_the_run_end() {
        let geometry = RunGeometry::derive(layout(24, 8), None).unwrap();
        assert_eq!(geometry.slot_index(geometry.first_slot_offset), Some(0));

        let last = geometry.slot_count - 1;
        assert_eq!(
            geometry.slot_index(geometry.slot_offset(last).unwrap()),
            Some(last)
        );
        assert_eq!(geometry.slot_index(0), None);
        assert_eq!(geometry.slot_index(geometry.first_slot_offset - 1), None);
        assert_eq!(geometry.slot_index(geometry.first_slot_offset + 1), None);
        assert_eq!(geometry.slot_index(RUN_SIZE), None);
        assert_eq!(geometry.slot_offset(geometry.slot_count), None);
    }

    #[test]
    fn maximum_size_and_alignment_follow_the_fixed_run_geometry() {
        let maximum_size = RunGeometry::derive(layout(MAX_MANAGED_SIZE, 1), None).unwrap();
        assert_eq!(maximum_size.slot_count, 1);
        assert_eq!(
            RunGeometry::derive(layout(MAX_MANAGED_SIZE + 1, 1), None),
            Err(GeometryError::NoSlots)
        );

        let maximum_alignment =
            RunGeometry::derive(layout(1, MAX_MANAGED_ALIGNMENT), None).unwrap();
        assert_eq!(maximum_alignment.slot_count, 1);
        assert_eq!(
            RunGeometry::derive(layout(1, RUN_SIZE), None),
            Err(GeometryError::NoSlots)
        );
    }

    #[test]
    fn invalid_and_overflowing_requests_fail_without_state() {
        assert_eq!(
            RunGeometry::derive(Layout::new::<()>(), None),
            Err(GeometryError::ZeroSized)
        );
        assert_eq!(
            RunGeometry::derive(layout(1, 8), Some(usize::MAX)),
            Err(GeometryError::ArithmeticOverflow)
        );
    }

    #[test]
    fn sampled_geometry_is_self_consistent() {
        for alignment in [1, 2, 4, 8, 16, 32, 64, 256, 4096] {
            for size in [1, 2, 7, 8, 15, 16, 24, 32, 63, 64, 192, 256, 4096] {
                let payload = layout(size, alignment);
                let Ok(geometry) = RunGeometry::derive(payload, None) else {
                    continue;
                };

                assert!(geometry.slot_stride >= size);
                assert!(geometry.slot_stride.is_multiple_of(alignment));
                assert!(geometry.first_slot_offset.is_multiple_of(alignment));
                assert_eq!(geometry.allocation_bitmap.bit_len, geometry.slot_count);
                assert_eq!(geometry.mark_bitmap.bit_len, geometry.slot_count);
                assert_eq!(
                    geometry.lease_bitmap.bit_len,
                    geometry.allocation_bitmap.word_len
                );
                for index in [0, geometry.slot_count / 2, geometry.slot_count - 1] {
                    let offset = geometry.slot_offset(index).unwrap();
                    assert_eq!(geometry.slot_index(offset), Some(index));
                }
            }
        }
    }
}
