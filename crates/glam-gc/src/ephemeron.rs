//! Ephemerons and weak references (`opaque-resources-reclaimed-when-unreachable`).
//!
//! An ephemeron pairs a key with a value and keeps the value alive only
//! while the key is reachable by some other path. It does not keep its key
//! alive. It is a managed cell of two words, the key's and value's
//! addresses, and lives as any allocation does, while a root or a traced edge
//! reaches it. Marking marks a reached ephemeron but traces neither word: it
//! records the ephemeron instead. Once the worklist drains, it marks the value
//! of each recorded ephemeron whose key it marked, tracing from those values,
//! which may reach more ephemerons, and repeats until a round marks nothing
//! new: a value may reach another ephemeron's key. A recorded ephemeron whose
//! key stays unmarked is then cleared, before the sweep reclaims anything.
//! Clearing is silent: an owner notices a cleared ephemeron when it next uses
//! it, or when it next traces what holds it. An ephemeron that nothing
//! reaches is not recorded: it keeps nothing alive, and is swept as any
//! allocation is. A weak reference is an ephemeron whose key and value
//! coincide.

use std::fmt;
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicPtr, Ordering};

use crate::trace::ErasedGc;
use crate::{GcRef, Trace, Visitor};

/// An ephemeron's key and value addresses, null once cleared: what marking
/// reads and clears, whatever the ephemeron's types.
pub(crate) struct EphemeronPair {
    key: AtomicPtr<()>,
    value: AtomicPtr<()>,
}

impl EphemeronPair {
    /// The key's and value's addresses, unless a collection cleared them.
    pub(crate) fn load(&self) -> Option<(ErasedGc, ErasedGc)> {
        let key = NonNull::new(self.key.load(Ordering::Acquire))?;
        let value = NonNull::new(self.value.load(Ordering::Acquire))
            .expect("an ephemeron with a key holds its value");
        Some((ErasedGc::new(key), ErasedGc::new(value)))
    }

    /// Clears the pair, whose key its collection left unmarked. The caller
    /// holds exclusive collection, before any sweep.
    pub(crate) fn clear(&self) {
        self.key.store(std::ptr::null_mut(), Ordering::Release);
        self.value.store(std::ptr::null_mut(), Ordering::Release);
    }
}

/// An ephemeron that marking reached and recorded, for the rest of its
/// collection.
#[derive(Clone, Copy)]
pub(crate) struct ReachedEphemeron(NonNull<EphemeronPair>);

impl ReachedEphemeron {
    pub(crate) fn new(pair: &EphemeronPair) -> Self {
        Self(NonNull::from(pair))
    }

    /// The recorded pair.
    ///
    /// # Safety
    ///
    /// The collection that recorded it still holds exclusive authority and
    /// has swept nothing, so the ephemeron, a marked allocation that a
    /// non-moving heap keeps in place, is live.
    pub(crate) unsafe fn pair<'collection>(self) -> &'collection EphemeronPair {
        // SAFETY: the caller keeps the recording collection's exclusive
        // authority, unswept, so the marked ephemeron holding this pair stays
        // live and in place, and nothing else writes it meanwhile.
        unsafe { self.0.as_ref() }
    }
}

/// An ephemeron: a managed cell whose value lives while its key is
/// reachable by some other path, and which keeps neither alive by itself.
///
/// It lives as any managed value does, while a root or a traced edge reaches
/// it: [`crate::MutatorAccess::ephemeron`] allocates one, and a view of it
/// views its key and value ([`GcRef::entry`]). Once a collection finds the key
/// unreachable, it clears the ephemeron, silently: its owner notices when it
/// next views it.
pub struct Ephemeron<K: Trace, V: Trace> {
    pair: EphemeronPair,
    marker: PhantomData<fn() -> (K, V)>,
}

/// A weak reference: an ephemeron whose key is its value, which it views
/// while some other path reaches it ([`GcRef::target`]).
pub type Weak<T> = Ephemeron<T, T>;

impl<K: Trace, V: Trace> Ephemeron<K, V> {
    /// An ephemeron of `key` and `value`.
    ///
    /// # Safety
    ///
    /// `key` and `value` identify a live `K` and a live `V` in the heap that
    /// allocates the ephemeron, under the admission that allocates it.
    pub(crate) unsafe fn new(key: ErasedGc, value: ErasedGc) -> Self {
        Self {
            pair: EphemeronPair {
                key: AtomicPtr::new(key.as_ptr().as_ptr()),
                value: AtomicPtr::new(value.as_ptr().as_ptr()),
            },
            marker: PhantomData,
        }
    }

    /// Whether a collection has found the key unreachable.
    #[must_use]
    pub fn is_cleared(&self) -> bool {
        self.pair.load().is_none()
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

// SAFETY: an ephemeron's two words are the one managed representation that is
// not an edge, by design. Its trace reports neither: it reports the ephemeron
// itself, through a channel only the crate's marking reads, which marks the
// value only once the key is marked, and clears the pair before any sweep
// otherwise. It mutates nothing, and a panicking visitor leaves it intact.
unsafe impl<K: Trace, V: Trace> Trace for Ephemeron<K, V> {
    fn trace(&self, visitor: &mut Visitor<'_>) {
        visitor.ephemeron(&self.pair);
    }
}

impl<'m, K: Trace, V: Trace> GcRef<'m, Ephemeron<K, V>> {
    /// Views the key and value for as long as this view, or `None` once a
    /// collection found the key unreachable.
    #[must_use]
    pub fn entry(self) -> Option<(GcRef<'m, K>, GcRef<'m, V>)> {
        let (key, value) = self.get().pair.load()?;
        // SAFETY: the ephemeron was made from proof of a live `K` and a live
        // `V` in its heap, the heap that this view proves it in. Every
        // collection since reached it, or it would have been swept and not
        // viewed now, and either marked its key, and then its value from it,
        // so both survived the sweep, or left the key unmarked and cleared it
        // before sweeping. The key is not cleared, so both are still live,
        // and the view's borrowed admission excludes collection for `'m`.
        unsafe {
            Some((
                self.sibling(key.as_ptr().cast::<K>()),
                self.sibling(value.as_ptr().cast::<V>()),
            ))
        }
    }
}

impl<'m, T: Trace> GcRef<'m, Weak<T>> {
    /// Views the value for as long as this view, or `None` once a collection
    /// found the key unreachable: for a weak reference, once nothing else
    /// reaches its value.
    #[must_use]
    pub fn target(self) -> Option<GcRef<'m, T>> {
        self.entry().map(|(_, value)| value)
    }
}
