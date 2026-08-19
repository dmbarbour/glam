use std::marker::PhantomData;
use std::rc::Rc;

use crate::heap::HeapInner;

/// Scoped authority to access one [`crate::Heap`].
///
/// C0 provides no operations on this token. It is intentionally neither
/// `Send` nor `Sync`; C1 will verify and document the complete access contract
/// before managed pointers exist.
pub struct Mutator<'heap> {
    marker: PhantomData<(&'heap HeapInner, Rc<()>)>,
}

impl<'heap> Mutator<'heap> {
    pub(crate) fn new(_heap: &'heap HeapInner) -> Self {
        Self {
            marker: PhantomData,
        }
    }
}
