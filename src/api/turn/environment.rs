//! Where machines' mixins of their environment lie, and the environment
//! that reflection's hosts read from them (`machines-inherit-an-environment`).
//! A machine's mixin is its share of the environment: the store holds it,
//! and the runtime's share record says which machines have one.

use std::sync::LazyLock;

use crate::api::Value;
use crate::core::Key;
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
