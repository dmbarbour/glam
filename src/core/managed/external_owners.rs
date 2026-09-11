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

use crate::runtime::EvaluationRuntimeId;

#[derive(Clone)]
pub(crate) struct ExternalOwnerHandle {
    runtime: EvaluationRuntimeId,
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
    runtime: EvaluationRuntimeId,
    next_id: AtomicU64,
    owners: Mutex<HashMap<NonZeroU64, ExternalOwnerEntry>>,
}

impl ExternalOwnerRegistry {
    pub(crate) fn new(runtime: EvaluationRuntimeId) -> Self {
        Self {
            runtime,
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
        ExternalOwnerHandle {
            runtime: self.runtime,
            id,
            lease,
        }
    }

    pub(crate) fn get<T>(&self, handle: &ExternalOwnerHandle) -> Arc<T>
    where
        T: Any + Send + Sync,
    {
        assert_eq!(
            handle.runtime, self.runtime,
            "an external owner handle must be opened by its matching runtime"
        );
        let owners = self
            .owners
            .lock()
            .expect("external owner registry was poisoned");
        let entry = owners
            .get(&handle.id)
            .expect("a live external owner lease must retain its registry entry");
        assert!(
            std::ptr::eq(entry.lease.as_ptr(), Arc::as_ptr(&handle.lease)),
            "an external owner handle must be opened by its matching registry"
        );
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

    pub(crate) fn try_get<T>(&self, handle: &ExternalOwnerHandle) -> Option<Arc<T>>
    where
        T: Any + Send + Sync,
    {
        if handle.runtime != self.runtime {
            return None;
        }
        let owners = self
            .owners
            .lock()
            .expect("external owner registry was poisoned");
        let entry = owners.get(&handle.id)?;
        if !std::ptr::eq(entry.lease.as_ptr(), Arc::as_ptr(&handle.lease))
            || entry.family != TypeId::of::<T>()
        {
            return None;
        }
        entry.owner.downcast_ref::<Arc<T>>().cloned()
    }

    /// Detaches dead entries under the registry lock and destroys their active
    /// owners one at a time only after releasing it.
    ///
    /// IDs establish a deterministic retirement order. If one destructor
    /// unwinds, that attempted owner remains detached while every untouched
    /// later owner remains registered for the next drain. Concurrent drains
    /// may divide the work, but removal under the registry lock still gives
    /// exactly one caller ownership of each destructor.
    pub(crate) fn drain_retired(&self) -> usize {
        let retired_ids = {
            let owners = self
                .owners
                .lock()
                .expect("external owner registry was poisoned");
            let mut retired_ids = owners
                .iter()
                .filter_map(|(id, entry)| (entry.lease.strong_count() == 0).then_some(*id))
                .collect::<Vec<_>>();
            retired_ids.sort_unstable();
            retired_ids
        };

        let mut count = 0;
        for id in retired_ids {
            let retired = self
                .owners
                .lock()
                .expect("external owner registry was poisoned")
                .remove(&id);
            let Some(retired) = retired else {
                continue;
            };
            drop(retired);
            count += 1;
        }
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
    pub(crate) fn same_owner(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.lease, &other.lease)
    }

    #[cfg(test)]
    fn lease_is_shared(&self) -> bool {
        Arc::strong_count(&self.lease) > 1
    }
}

#[cfg(test)]
mod tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct DropSignal(Arc<AtomicUsize>);

    struct OrderedDrop {
        id: usize,
        panic: bool,
        events: Arc<Mutex<Vec<usize>>>,
    }

    struct LockObservationDrop {
        registry: Weak<ExternalOwnerRegistry>,
        observed_unlocked: Arc<std::sync::atomic::AtomicBool>,
    }

    impl Drop for DropSignal {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    impl Drop for OrderedDrop {
        fn drop(&mut self) {
            self.events
                .lock()
                .expect("ordered-drop events were poisoned")
                .push(self.id);
            assert!(!self.panic, "injected opaque owner drop panic");
        }
    }

    impl Drop for LockObservationDrop {
        fn drop(&mut self) {
            let registry = self
                .registry
                .upgrade()
                .expect("registry should outlive its detached owner");
            self.observed_unlocked.store(
                registry.owners.try_lock().is_ok(),
                std::sync::atomic::Ordering::Relaxed,
            );
        }
    }

    #[test]
    fn dead_owner_is_detached_before_destructor_runs() {
        let registry = ExternalOwnerRegistry::new(crate::runtime::allocate_evaluation_runtime_id());
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

    #[test]
    fn retired_owner_is_destroyed_after_registry_unlock() {
        let registry = Arc::new(ExternalOwnerRegistry::new(
            crate::runtime::allocate_evaluation_runtime_id(),
        ));
        let observed_unlocked = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let handle = registry.insert(Arc::new(LockObservationDrop {
            registry: Arc::downgrade(&registry),
            observed_unlocked: Arc::clone(&observed_unlocked),
        }));
        drop(handle);

        assert_eq!(registry.drain_retired(), 1);
        assert!(observed_unlocked.load(std::sync::atomic::Ordering::Relaxed));
    }

    #[test]
    fn opaque_drop_panic_retries_untouched_suffix() {
        let registry = ExternalOwnerRegistry::new(crate::runtime::allocate_evaluation_runtime_id());
        let events = Arc::new(Mutex::new(Vec::new()));
        let first = registry.insert(Arc::new(OrderedDrop {
            id: 0,
            panic: true,
            events: Arc::clone(&events),
        }));
        let second = registry.insert(Arc::new(OrderedDrop {
            id: 1,
            panic: false,
            events: Arc::clone(&events),
        }));
        drop((first, second));

        let panic = catch_unwind(AssertUnwindSafe(|| registry.drain_retired()));
        assert!(panic.is_err());
        assert_eq!(*events.lock().unwrap(), vec![0]);
        assert_eq!(registry.len(), 1);

        assert_eq!(registry.drain_retired(), 1);
        assert_eq!(*events.lock().unwrap(), vec![0, 1]);
        assert_eq!(registry.len(), 0);
    }
}
