//! Release-code-generation fixture for the persistent managed-edge API.
//!
//! This is inspected by `scripts/check-persistent-edge-codegen.sh`. Keep the
//! wrappers free of setup or observation policy so their optimized bodies show
//! the exact cost of the two operations.

use glam_gc::{Gc, Mutator};

#[unsafe(no_mangle)]
#[inline(never)]
pub fn glam_gc_codegen_duplicate_u64(value: &Gc<u64>, mutator: &Mutator<'_>) -> Gc<u64> {
    value.duplicate_in(mutator)
}

#[unsafe(no_mangle)]
#[inline(never)]
pub fn glam_gc_codegen_same_allocation_u64(
    left: &Gc<u64>,
    right: &Gc<u64>,
    mutator: &Mutator<'_>,
) -> bool {
    left.same_allocation_in(right, mutator)
}

fn main() {
    std::hint::black_box(glam_gc_codegen_duplicate_u64 as *const ());
    std::hint::black_box(glam_gc_codegen_same_allocation_u64 as *const ());
}
