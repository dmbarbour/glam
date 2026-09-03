//! Runtime-owned storage for active Rust owners referenced by managed values.
//!
//! Managed nodes retain only [`ExternalOwnerHandle`]: a scalar ID and an
//! ordinary lease token. The active owner remains in this registry. A dead
//! lease is detached while the registry is locked and destroyed afterward,
//! keeping callback and runtime retirement outside collector finalization.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

#[derive(Clone)]
pub(crate) struct ExternalOwnerHandle {
    id: NonZeroU64,
    #[allow(
        dead_code,
        reason = "retaining the lease is the handle's semantic ownership operation"
    )]
    lease: Arc<()>,
}

struct ExternalOwnerEntry {
    family: TypeId,
    lease: Weak<()>,
    owner: Box<dyn Any + Send + Sync>,
}

pub(crate) struct ExternalOwnerRegistry {
    next_id: AtomicU64,
    owners: Mutex<HashMap<NonZeroU64, ExternalOwnerEntry>>,
}

impl ExternalOwnerRegistry {
    pub(crate) fn new() -> Self {
        Self {
            next_id: AtomicU64::new(1),
            owners: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn insert<T>(&self, owner: Arc<T>) -> ExternalOwnerHandle
    where
        T: Any + Send + Sync,
    {
        let id = NonZeroU64::new(self.next_id.fetch_add(1, Ordering::Relaxed))
            .expect("external owner IDs exhausted for one value domain");
        let lease = Arc::new(());
        let previous = self
            .owners
            .lock()
            .expect("external owner registry was poisoned")
            .insert(
                id,
                ExternalOwnerEntry {
                    family: TypeId::of::<T>(),
                    lease: Arc::downgrade(&lease),
                    owner: Box::new(owner),
                },
            );
        assert!(previous.is_none(), "external owner IDs remain unique");
        ExternalOwnerHandle { id, lease }
    }

    pub(crate) fn get<T>(&self, handle: &ExternalOwnerHandle) -> Arc<T>
    where
        T: Any + Send + Sync,
    {
        let owners = self
            .owners
            .lock()
            .expect("external owner registry was poisoned");
        let entry = owners
            .get(&handle.id)
            .expect("a live external owner lease must retain its registry entry");
        assert_eq!(
            entry.family,
            TypeId::of::<T>(),
            "an external owner handle must be opened as its registered family"
        );
        entry
            .owner
            .downcast_ref::<Arc<T>>()
            .expect("an external owner entry must retain its recorded family")
            .clone()
    }

    /// Detaches dead entries under the registry lock and destroys their active
    /// owners only after releasing it.
    pub(crate) fn drain_retired(&self) -> usize {
        let retired = {
            let mut owners = self
                .owners
                .lock()
                .expect("external owner registry was poisoned");
            let retired_ids = owners
                .iter()
                .filter_map(|(id, entry)| (entry.lease.strong_count() == 0).then_some(*id))
                .collect::<Vec<_>>();
            retired_ids
                .into_iter()
                .map(|id| {
                    owners
                        .remove(&id)
                        .expect("a discovered retired owner must remain registered")
                })
                .collect::<Vec<_>>()
        };
        let count = retired.len();
        drop(retired);
        count
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.owners
            .lock()
            .expect("external owner registry was poisoned")
            .len()
    }
}

impl ExternalOwnerHandle {
    #[cfg(test)]
    fn lease_is_shared(&self) -> bool {
        Arc::strong_count(&self.lease) > 1
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct DropSignal(Arc<AtomicUsize>);

    impl Drop for DropSignal {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn dead_owner_is_detached_before_destructor_runs() {
        let registry = ExternalOwnerRegistry::new();
        let drops = Arc::new(AtomicUsize::new(0));
        let handle = registry.insert(Arc::new(DropSignal(Arc::clone(&drops))));
        let clone = handle.clone();
        assert!(handle.lease_is_shared());
        assert_eq!(registry.len(), 1);

        drop(handle);
        assert_eq!(registry.drain_retired(), 0);
        assert_eq!(drops.load(Ordering::Relaxed), 0);

        drop(clone);
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        assert_eq!(registry.drain_retired(), 1);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert_eq!(registry.len(), 0);
    }
}
