//! Where machines' mixins of their environment lie, and the environment
//! that reflection's hosts read from them (`machines-inherit-an-environment`).
//! A machine's mixin is its share of the environment: the store holds it,
//! and the runtime's share record says which machines have one.

use std::sync::LazyLock;

use crate::api::{RuntimeEventJournal, Value};
use crate::core::{CoreValueFactory, Key};
use crate::reflection::StoreJournal;
use crate::runtime::RuntimeValueRoot;

use super::super::runtime::{RuntimeSharedResources, ShareJournal};
use super::CutTransaction;

/// The machine, a direct child of the root, whose environment reflection's
/// tasks read, and through whose diagnostics port the runtime's own
/// diagnostics pass, until reflection tasks are machines.
pub(in crate::api) static REFL: LazyLock<Key> = LazyLock::new(|| Key::atom_from_text("refl"));

/// The keys of the environment volume: a machine's share lies under
/// `children` at each key of its path, then at `share`, so a subtree's
/// shares go with one write.
pub(in crate::api) static CHILDREN: LazyLock<Key> =
    LazyLock::new(|| Key::atom_from_text("children"));
pub(in crate::api) static SHARE: LazyLock<Key> = LazyLock::new(|| Key::atom_from_text("share"));

/// The node of the machine at `machine` in the environment volume.
pub(in crate::api) fn environment_node(machine: &[Key]) -> Vec<Key> {
    machine
        .iter()
        .flat_map(|key| [CHILDREN.clone(), key.clone()])
        .collect()
}

/// Where the share of the machine at `machine` lies in the environment
/// volume.
pub(in crate::api) fn environment_share(machine: &[Key]) -> Vec<Key> {
    let mut path = environment_node(machine);
    path.push(SHARE.clone());
    path
}

/// The machines whose mixins compose the environment of the machine at
/// `machine`, the most specific first: it, then each ancestor, to the root.
pub(in crate::api) fn environment_chain(machine: &[Key]) -> Vec<Vec<Key>> {
    (0..=machine.len())
        .rev()
        .map(|depth| machine[..depth].to_vec())
        .collect()
}

/// The mixin of the machine at `machine` within `cut`, where it has one,
/// observing its place in the store, set or not, so that a cut that read it
/// runs again when it changes.
pub(super) fn observed_mixin(cut: &mut CutTransaction, machine: &[Key]) -> Option<Value> {
    let path = environment_share(machine);
    cut.observed |= cut.journal.observe_environment_read(&path);
    cut.events
        .shares
        .is_set(machine)
        .then(|| cut.journal.environment_value(&path))
}

/// The environment of the machine at `machine`, composed from the shares
/// committed as it is read, over `base`: for reflection's hosts, which read
/// it outside any transaction until reflection tasks are machines, so a
/// change to a share wakes no task that read it.
pub(in crate::api) fn committed_environment(
    resources: &RuntimeSharedResources,
    machine: &[Key],
    base: &RuntimeValueRoot,
) -> RuntimeValueRoot {
    let (_, store, events) = resources.transaction_snapshot();
    let shares = ShareJournal::new(&events.shares);
    let mixins = environment_chain(machine)
        .iter()
        .filter(|machine| shares.is_set(machine))
        .map(|machine| {
            store
                .environment_value(&environment_share(machine))
                .into_runtime_root()
        })
        .collect();
    crate::g_syntax::composed_environment_root(resources.values.core(), mixins, Some(base))
}

/// The environment of the machine at `machine` as `store` and `events` show
/// it, over `base`: the overlays on it and its ancestors, the most specific
/// first, or `base` itself where none applies
/// (`machines-inherit-an-environment`). It reads each share's place in the
/// store, observed, so a cut that read it runs again when an overlay above
/// it changes, and answers whether it observed anything.
pub(crate) fn journal_environment(
    store: &mut StoreJournal,
    events: &RuntimeEventJournal,
    values: &CoreValueFactory,
    machine: &[Key],
    base: &RuntimeValueRoot,
) -> (RuntimeValueRoot, bool) {
    let mut observed = false;
    let mut overlays = Vec::new();
    for machine in environment_chain(machine) {
        let path = environment_share(&machine);
        observed |= store.observe_environment_read(&path);
        if events.shares.is_set(&machine) {
            overlays.push(store.environment_value(&path).into_runtime_root());
        }
    }
    let environment = if overlays.is_empty() {
        base.clone()
    } else {
        crate::g_syntax::composed_environment_root(values, overlays, Some(base))
    };
    (environment, observed)
}

/// Overlays `overlay` on the environment of the machine at `machine`,
/// stacked on what its share holds: the share becomes the overlay over what
/// it was (`machines-inherit-an-environment`). The share's place is read,
/// observed, so that overlays made at once conflict.
pub(crate) fn overlay_environment(
    store: &mut StoreJournal,
    events: &mut RuntimeEventJournal,
    values: &CoreValueFactory,
    machine: &[Key],
    overlay: Value,
) {
    let path = environment_share(machine);
    store.observe_environment_read(&path);
    let share = if events.shares.is_set(machine) {
        let below = store.environment_value(&path).into_runtime_root();
        Value::from_runtime_root(crate::g_syntax::composed_environment_root(
            values,
            vec![overlay.into_runtime_root(), below],
            None,
        ))
    } else {
        overlay
    };
    store.write_environment(path, share);
    events.shares.set(machine.to_vec());
}

/// Clears the overlays on the environment of the machine at `machine`, so
/// that its environment is its ancestors' alone.
pub(crate) fn clear_environment(
    store: &mut StoreJournal,
    events: &mut RuntimeEventJournal,
    machine: &[Key],
) {
    store.remove_environment(environment_share(machine));
    events.shares.clear(machine.to_vec());
}
