//! Ports: the buffers of machines' receiving ports, and the wiring between
//! machines' ports, beside the runtime's other transactional events
//! (`machines-communicate-through-wired-ports`).
//!
//! Buffers and wiring are Rust structures with glam values as payloads, so
//! nothing here forces a value. A transaction claims from buffers through
//! FIFO cursors, appends to them, and edits the wiring. It reads committed
//! buffers only, never its own appends, as glam's log reads do, while its
//! routing follows its own wiring edits.
//!
//! A machine wires its own ports and its direct children's. A value that
//! reaches a machine's output follows every wire from it: those its parent
//! owns, and those the machine owns, which lead inward. A value that reaches
//! an input passes through where the machine wired the input on, and
//! otherwise waits in the input's buffer.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use rpds::RedBlackTreeMapSync;

use crate::api::Value;
use crate::core::Key;
use crate::runtime::RuntimeValueRoot;

/// The diagnostics port, which the default configuration wires.
pub(in crate::api) static DIAGNOSTICS: LazyLock<Key> = LazyLock::new(|| Key::from("diagnostics"));

/// The port where the runtime asks for sources' binaries
/// (`imports-are-source-requests`): it writes requests into the root's
/// input, which the default configuration passes through to the root's
/// output, for the host to wire to its server.
pub(in crate::api) static IMPORTS: LazyLock<Key> = LazyLock::new(|| Key::from("imports"));

/// A machine's port in one role: the machine's path from the root, and the
/// port's name.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::api) struct PortAddress {
    pub(in crate::api) machine: Vec<Key>,
    pub(in crate::api) port: Key,
}

/// One end of a wire, as the machine that owns the wire names it: its own
/// input or output, or a direct child's port, which is the child's output
/// as a wire's source and its input as a wire's destination.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::api) enum WireEnd {
    In(Key),
    Out(Key),
    Child(Key, Key),
}

/// A wire a machine owns.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::api) struct Wire {
    pub(in crate::api) source: WireEnd,
    pub(in crate::api) destination: WireEnd,
}

/// A receiving port's buffer: the sequence of its first item, and the items.
#[derive(Clone, Default)]
struct PortBuffer {
    head: u64,
    items: VecDeque<RuntimeValueRoot>,
}

impl PortBuffer {
    fn next(&self) -> u64 {
        self.head + self.items.len() as u64
    }
}

/// The wiring a machine owns: its wires, the default wires it removed, and
/// whether it turned the default wiring off.
#[derive(Clone, Default)]
struct OwnedWiring {
    wires: BTreeSet<Wire>,
    suppressed: BTreeSet<Wire>,
    defaults_off: bool,
}

impl OwnedWiring {
    fn is_empty(&self) -> bool {
        self.wires.is_empty() && self.suppressed.is_empty() && !self.defaults_off
    }
}

/// What [`PortJournal::owned_wiring`] reports of the wiring a machine owns.
pub(in crate::api) struct OwnedWiringView {
    pub(in crate::api) wires: Vec<Wire>,
    pub(in crate::api) defaults: bool,
    pub(in crate::api) suppressed: Vec<Wire>,
}

/// Whether the default diagnostics wiring applies to the wiring that the
/// machine at `owner` owns.
fn defaults_apply(wiring: &Wiring, diagnostics: bool, owner: &[Key]) -> bool {
    diagnostics && !wiring.get(owner).is_some_and(|owned| owned.defaults_off)
}

/// Wiring by the path of the machine that owns it.
type Wiring = RedBlackTreeMapSync<Vec<Key>, Arc<OwnedWiring>>;

/// The committed buffers and wiring. Cloning it is a snapshot.
#[derive(Clone)]
pub(in crate::api) struct PortState {
    buffers: RedBlackTreeMapSync<PortAddress, Arc<PortBuffer>>,
    wiring: Wiring,
    /// Advanced by each commit that changes the wiring.
    wiring_revision: u64,
    /// Whether the default configuration's diagnostics wiring applies.
    diagnostics: bool,
}

impl PortState {
    pub(in crate::api) fn new() -> Self {
        Self {
            buffers: RedBlackTreeMapSync::new_sync(),
            wiring: RedBlackTreeMapSync::new_sync(),
            wiring_revision: 0,
            diagnostics: true,
        }
    }

    /// Sets whether the default diagnostics wiring applies: each child's
    /// diagnostics output to its parent's diagnostics input, each machine's
    /// diagnostics input, the root's aside, to its own output, and the
    /// root's imports input to its output.
    pub(in crate::api) fn set_diagnostics_wiring(&mut self, enabled: bool) {
        if self.diagnostics != enabled {
            self.diagnostics = enabled;
            self.wiring_revision += 1;
        }
    }

    /// Delivers `value` to the input at `address` at once, outside any
    /// transaction, as runtime inputs are admitted: for diagnostics that the
    /// runtime routes to a port, and for its requests for sources. It
    /// arrives as a child's write would, passing
    /// through where the machine wired the input on; where that routing
    /// fails, by a loop or an unconnected output, it waits in the input
    /// itself, so that nothing is lost. The caller holds the transaction lock
    /// and publishes the change.
    pub(in crate::api) fn admit(&mut self, address: PortAddress, value: RuntimeValueRoot) {
        let buffers = PortJournal::new(self.clone())
            .route_input(&address.machine, &address.port)
            .unwrap_or_else(|_| vec![address]);
        for address in buffers {
            let mut buffer = self.buffers.get(&address).cloned().unwrap_or_default();
            Arc::make_mut(&mut buffer).items.push_back(value.clone());
            self.buffers.insert_mut(address, buffer);
        }
    }

    /// Whether `journal`'s reads and routing still hold: no buffer it read
    /// was consumed, nor one it found short added to, no machine's buffers
    /// it surveyed changed, and the wiring it followed is unchanged.
    pub(in crate::api) fn validate(&self, journal: &PortJournal) -> bool {
        let observations = lock(&journal.observations);
        if observations.wiring && self.wiring_revision != journal.snapshot.wiring_revision {
            return false;
        }
        observations
            .reads
            .iter()
            .chain(&journal.cursors)
            .all(|(address, cursor)| self.holds(address, cursor))
            && observations.surveys.iter().all(|machine| {
                machine_buffers(&self.buffers, machine)
                    == machine_buffers(&journal.snapshot.buffers, machine)
            })
    }

    fn holds(&self, address: &PortAddress, cursor: &PortCursor) -> bool {
        let (head, next) = match self.buffers.get(address) {
            Some(buffer) => (buffer.head, buffer.next()),
            None => (0, 0),
        };
        head == cursor.start && cursor.next <= next && cursor.tail.is_none_or(|tail| next == tail)
    }

    /// The first item the buffer at `address` holds, as committed, with its
    /// sequence.
    pub(in crate::api) fn head(&self, address: &PortAddress) -> Option<(u64, RuntimeValueRoot)> {
        self.buffers
            .get(address)
            .and_then(|buffer| buffer.items.front().map(|item| (buffer.head, item.clone())))
    }

    /// Applies a validated journal, returning whether anything changed.
    pub(in crate::api) fn commit(&mut self, journal: &PortJournal) -> bool {
        let mut changed = false;
        for (address, cursor) in &journal.cursors {
            let count = cursor.next - cursor.start;
            if count == 0 {
                continue;
            }
            changed = true;
            let buffer = Arc::make_mut(
                self.buffers
                    .get_mut(address)
                    .expect("validation found every buffer a claim consumes"),
            );
            buffer.items.drain(..count as usize);
            buffer.head += count;
        }
        for (address, value) in &journal.appends {
            changed = true;
            let mut buffer = self.buffers.get(address).cloned().unwrap_or_default();
            Arc::make_mut(&mut buffer).items.push_back(value.clone());
            self.buffers.insert_mut(address.clone(), buffer);
        }
        if !journal.edits.is_empty() {
            changed = true;
            for edit in &journal.edits {
                apply_edit(&mut self.wiring, self.diagnostics, edit);
            }
            self.wiring_revision += 1;
        }
        changed
    }
}

/// The ports of the machine at `machine` that hold buffers, with each
/// buffer's head and next sequences, in port order.
fn machine_buffers(
    buffers: &RedBlackTreeMapSync<PortAddress, Arc<PortBuffer>>,
    machine: &[Key],
) -> Vec<(Key, u64, u64)> {
    buffers
        .iter()
        .filter(|(address, _)| address.machine == machine)
        .map(|(address, buffer)| (address.port.clone(), buffer.head, buffer.next()))
        .collect()
}

/// Whether `wire`, owned by the machine at `owner`, is one of the default
/// wiring's: the diagnostics wiring, and the root's imports pass-through.
fn is_default(diagnostics: bool, owner: &[Key], wire: &Wire) -> bool {
    let port = &*DIAGNOSTICS;
    diagnostics
        && match (&wire.source, &wire.destination) {
            (WireEnd::Child(_, source), WireEnd::In(destination)) => {
                source == port && destination == port
            }
            (WireEnd::In(source), WireEnd::Out(destination)) => {
                let passed = if owner.is_empty() { &*IMPORTS } else { port };
                source == passed && destination == passed
            }
            _ => false,
        }
}

/// A transaction's position in one buffer: its first sequence when the
/// transaction first read it, the next it would claim, and the buffer's end
/// where it saw that, so that an item added since invalidates it. The end
/// is where the buffer stood, not the claim: a count of items left
/// unclaimed sees them, and stands while nothing joins them.
#[derive(Clone, Debug)]
struct PortCursor {
    start: u64,
    next: u64,
    tail: Option<u64>,
}

#[derive(Clone)]
enum WiringEdit {
    Add(Vec<Key>, Wire),
    Remove(Vec<Key>, Wire),
    /// Returns the wiring a machine owns to the defaults.
    Reset(Vec<Key>),
    /// Turns the default wiring a machine owns on or off.
    Defaults(Vec<Key>, bool),
}

/// Applies a wiring edit. Removing a default wire suppresses it, and adding
/// it back lifts the suppression; where the machine turned its defaults off,
/// such a wire is one of its own.
fn apply_edit(wiring: &mut Wiring, diagnostics: bool, edit: &WiringEdit) {
    let (owner, wire, add) = match edit {
        WiringEdit::Add(owner, wire) => (owner, wire, true),
        WiringEdit::Remove(owner, wire) => (owner, wire, false),
        WiringEdit::Reset(owner) => {
            wiring.remove_mut(owner);
            return;
        }
        WiringEdit::Defaults(owner, enabled) => {
            let mut owned = wiring.get(owner).cloned().unwrap_or_default();
            Arc::make_mut(&mut owned).defaults_off = !enabled;
            store_owned(wiring, owner, owned);
            return;
        }
    };
    let default =
        defaults_apply(wiring, diagnostics, owner) && is_default(diagnostics, owner, wire);
    let mut owned = wiring.get(owner).cloned().unwrap_or_default();
    let set = Arc::make_mut(&mut owned);
    match (add, default) {
        (true, true) => {
            set.suppressed.remove(wire);
        }
        (true, false) => {
            set.wires.insert(wire.clone());
        }
        (false, true) => {
            set.suppressed.insert(wire.clone());
        }
        (false, false) => {
            set.wires.remove(wire);
        }
    }
    store_owned(wiring, owner, owned);
}

/// Stores the wiring the machine at `owner` owns, or drops it where it says
/// nothing beyond the defaults.
fn store_owned(wiring: &mut Wiring, owner: &[Key], owned: Arc<OwnedWiring>) {
    if owned.is_empty() {
        wiring.remove_mut(owner);
    } else {
        wiring.insert_mut(owner.to_vec(), owned);
    }
}

/// What a transaction observed of the ports, shared by every alternative of
/// it, as its store reads are: reads made by a failed alternative still
/// count for the one that commits.
#[derive(Default)]
struct PortObservations {
    reads: BTreeMap<PortAddress, PortCursor>,
    /// Machines whose buffers the transaction surveyed together.
    surveys: BTreeSet<Vec<Key>>,
    wiring: bool,
}

/// A branch of a journal, for a fork within a cut: where the journal stood
/// at the fork, and what the branch has read since, for its merge.
#[derive(Clone)]
struct PortBranch {
    cursors: BTreeMap<PortAddress, u64>,
    edits: usize,
    appends: usize,
    reads: Arc<Mutex<BranchReads>>,
}

#[derive(Default)]
struct BranchReads {
    buffers: BTreeSet<PortAddress>,
    wiring: bool,
}

/// One transaction's port actions: claims, appends and wiring edits.
#[derive(Clone)]
pub(in crate::api) struct PortJournal {
    snapshot: PortState,
    /// The snapshot's wiring with this journal's edits applied.
    wiring: Wiring,
    edits: Vec<WiringEdit>,
    cursors: BTreeMap<PortAddress, PortCursor>,
    appends: Vec<(PortAddress, RuntimeValueRoot)>,
    observations: Arc<Mutex<PortObservations>>,
    branch: Option<PortBranch>,
}

/// An endpoint while routing: a machine's port in a role, by the machine's
/// path from the root.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum RouteEnd {
    Output(Vec<Key>, Key),
    Input(Vec<Key>, Key),
}

impl PortJournal {
    pub(in crate::api) fn new(snapshot: PortState) -> Self {
        Self {
            wiring: snapshot.wiring.clone(),
            snapshot,
            edits: Vec::new(),
            cursors: BTreeMap::new(),
            appends: Vec::new(),
            observations: Arc::new(Mutex::new(PortObservations::default())),
            branch: None,
        }
    }

    /// Claims the next `n` items of the buffer at `address`, or returns
    /// `None`, claiming nothing, when it holds fewer. Either way the read is
    /// observed: a short read waits for an item to arrive.
    pub(in crate::api) fn claim(&mut self, address: &PortAddress, n: usize) -> Option<Vec<Value>> {
        let buffer = self.snapshot.buffers.get(address).cloned();
        let head = buffer.as_ref().map_or(0, |buffer| buffer.head);
        let end = buffer.as_ref().map_or(0, |buffer| buffer.next());
        let cursor = self.cursor(address, head);
        let claimed = if end - cursor.next < n as u64 {
            cursor.tail = Some(end);
            None
        } else {
            // A claim of none needs no buffer.
            let offset = (cursor.next - head) as usize;
            let values = buffer
                .iter()
                .flat_map(|buffer| buffer.items.range(offset..offset + n))
                .map(|root| Value::from_runtime_root(root.clone()))
                .collect();
            cursor.next += n as u64;
            Some(values)
        };
        let observation = cursor.clone();
        self.observe(address, observation);
        claimed
    }

    /// The sequence of the next item this journal would claim from the
    /// buffer at `address`. It observes nothing: a claim observes what it
    /// takes.
    pub(in crate::api) fn next_sequence(&self, address: &PortAddress) -> u64 {
        self.cursors.get(address).map_or_else(
            || {
                self.snapshot
                    .buffers
                    .get(address)
                    .map_or(0, |buffer| buffer.head)
            },
            |cursor| cursor.next,
        )
    }

    /// Claims everything the buffer at `address` holds beyond this journal's
    /// claims, observing its end, so that an item arriving meanwhile
    /// conflicts.
    pub(in crate::api) fn claim_all(&mut self, address: &PortAddress) -> Vec<Value> {
        let held = self.available(address);
        self.claim(address, held)
            .expect("a claim of what a buffer holds succeeds")
    }

    /// The buffers of `port` at the machine at `machine` and its
    /// descendants, in address order. It observes nothing: it is discovery.
    pub(in crate::api) fn buffers_below(&self, machine: &[Key], port: &Key) -> Vec<PortAddress> {
        self.snapshot
            .buffers
            .keys()
            .filter(|address| address.port == *port && address.machine.starts_with(machine))
            .cloned()
            .collect()
    }

    /// How many items the buffer at `address` holds beyond this journal's
    /// claims, observing its end.
    pub(in crate::api) fn available(&mut self, address: &PortAddress) -> usize {
        let (head, end) = self
            .snapshot
            .buffers
            .get(address)
            .map_or((0, 0), |buffer| (buffer.head, buffer.next()));
        let cursor = self.cursor(address, head);
        cursor.tail = Some(end);
        let count = (end - cursor.next) as usize;
        let observation = cursor.clone();
        self.observe(address, observation);
        count
    }

    /// The input ports of the machine at `machine` that hold items beyond
    /// this journal's claims, with how many, in port order. It surveys all
    /// of the machine's buffers, so an item arriving at any of them, even a
    /// port that held none, invalidates it.
    pub(in crate::api) fn pending(&mut self, machine: &[Key]) -> Vec<(Key, usize)> {
        lock(&self.observations).surveys.insert(machine.to_vec());
        machine_buffers(&self.snapshot.buffers, machine)
            .into_iter()
            .filter_map(|(port, head, end)| {
                let address = PortAddress {
                    machine: machine.to_vec(),
                    port: port.clone(),
                };
                let claimed = self
                    .cursors
                    .get(&address)
                    .map_or(head, |cursor| cursor.next);
                (end > claimed).then(|| (port, (end - claimed) as usize))
            })
            .collect()
    }

    fn cursor(&mut self, address: &PortAddress, head: u64) -> &mut PortCursor {
        self.cursors
            .entry(address.clone())
            .or_insert_with(|| PortCursor {
                start: head,
                next: head,
                tail: None,
            })
    }

    fn observe(&mut self, address: &PortAddress, cursor: PortCursor) {
        lock(&self.observations)
            .reads
            .entry(address.clone())
            .and_modify(|prior| {
                debug_assert_eq!(prior.start, cursor.start);
                prior.next = prior.next.max(cursor.next);
                prior.tail = prior.tail.max(cursor.tail);
            })
            .or_insert(cursor);
        if let Some(branch) = &self.branch {
            lock(&branch.reads).buffers.insert(address.clone());
        }
    }

    fn observe_wiring(&mut self) {
        lock(&self.observations).wiring = true;
        if let Some(branch) = &self.branch {
            lock(&branch.reads).wiring = true;
        }
    }

    /// Returns the machine at `machine` and its descendants to their initial
    /// port state: claims everything their input buffers hold, so that what
    /// arrives after this journal's snapshot stays, drops what this journal
    /// was to append to them, and returns the wiring they own to the
    /// defaults. It observes the wiring, which a concurrent change then
    /// conflicts with.
    pub(in crate::api) fn clear_below(&mut self, machine: &[Key]) {
        self.observe_wiring();
        let addresses: Vec<PortAddress> = self
            .snapshot
            .buffers
            .keys()
            .filter(|address| address.machine.starts_with(machine))
            .cloned()
            .collect();
        for address in addresses {
            // Counted without observing the end: later arrivals stay.
            let (head, end) = self
                .snapshot
                .buffers
                .get(&address)
                .map_or((0, 0), |buffer| (buffer.head, buffer.next()));
            let held = end - self.cursor(&address, head).next;
            self.claim(&address, held as usize);
        }
        self.appends
            .retain(|(address, _)| !address.machine.starts_with(machine));
        let owners: Vec<Vec<Key>> = self
            .wiring
            .keys()
            .filter(|owner| owner.starts_with(machine))
            .cloned()
            .collect();
        for owner in owners {
            self.edit(WiringEdit::Reset(owner));
        }
    }

    /// The machines at or below `machine` that hold port state: values
    /// waiting in an input beyond this journal's claims, or wiring they own.
    /// It observes nothing: it is discovery, not a read a cut validates.
    pub(in crate::api) fn present_below(&self, machine: &[Key]) -> BTreeSet<Vec<Key>> {
        let waiting = self
            .snapshot
            .buffers
            .iter()
            .filter_map(|(address, buffer)| {
                let claimed = self
                    .cursors
                    .get(address)
                    .map_or(buffer.head, |cursor| cursor.next);
                (address.machine.starts_with(machine) && buffer.next() > claimed)
                    .then(|| address.machine.clone())
            });
        let wiring = self
            .wiring
            .keys()
            .filter(|owner| owner.starts_with(machine))
            .cloned();
        waiting.chain(wiring).collect()
    }

    /// Appends `value` to the buffer at `address`, when this journal commits.
    pub(in crate::api) fn append(&mut self, address: PortAddress, value: Value) {
        self.appends.push((address, value.into_runtime_root()));
    }

    /// Adds `wire`, owned by the machine at `owner`.
    pub(in crate::api) fn wire(&mut self, owner: Vec<Key>, wire: Wire) {
        self.edit(WiringEdit::Add(owner, wire));
    }

    /// Removes `wire`, owned by the machine at `owner`, if it is there; a
    /// default wire is suppressed.
    pub(in crate::api) fn unwire(&mut self, owner: Vec<Key>, wire: Wire) {
        self.edit(WiringEdit::Remove(owner, wire));
    }

    /// Turns the default wiring that the machine at `owner` owns on or off.
    pub(in crate::api) fn set_defaults(&mut self, owner: Vec<Key>, enabled: bool) {
        self.edit(WiringEdit::Defaults(owner, enabled));
    }

    /// The wiring the machine at `owner` owns, observing the wiring: its
    /// wires, whether the default wiring applies to it, and the default
    /// wires it removed.
    pub(in crate::api) fn owned_wiring(&mut self, owner: &[Key]) -> OwnedWiringView {
        self.observe_wiring();
        let owned = self.wiring.get(owner);
        OwnedWiringView {
            wires: owned
                .iter()
                .flat_map(|owned| owned.wires.iter().cloned())
                .collect(),
            defaults: defaults_apply(&self.wiring, self.snapshot.diagnostics, owner),
            suppressed: owned
                .iter()
                .flat_map(|owned| owned.suppressed.iter().cloned())
                .collect(),
        }
    }

    fn edit(&mut self, edit: WiringEdit) {
        apply_edit(&mut self.wiring, self.snapshot.diagnostics, &edit);
        self.edits.push(edit);
    }

    /// The buffers that a write to the output `port` of the machine at
    /// `machine` reaches, following wires through pass-throughs, in a fixed
    /// order. Several wires from one output each deliver. It fails when an
    /// output on the way has no wire, and when the wiring loops.
    pub(in crate::api) fn route(
        &mut self,
        machine: &[Key],
        port: &Key,
    ) -> Result<Vec<PortAddress>, String> {
        self.observe_wiring();
        let mut buffers = Vec::new();
        let mut route = Vec::new();
        self.follow(
            RouteEnd::Output(machine.to_vec(), port.clone()),
            &mut route,
            &mut buffers,
        )?;
        Ok(buffers)
    }

    /// The buffers that a value arriving at the input `port` of the machine
    /// at `machine` reaches, as [`Self::route`] follows a write, without
    /// observing the wiring.
    fn route_input(&self, machine: &[Key], port: &Key) -> Result<Vec<PortAddress>, String> {
        let mut buffers = Vec::new();
        self.follow(
            RouteEnd::Input(machine.to_vec(), port.clone()),
            &mut Vec::new(),
            &mut buffers,
        )?;
        Ok(buffers)
    }

    fn follow(
        &self,
        end: RouteEnd,
        route: &mut Vec<RouteEnd>,
        buffers: &mut Vec<PortAddress>,
    ) -> Result<(), String> {
        if route.contains(&end) {
            return Err(format!("the wiring loops through {end:?}"));
        }
        let next = match &end {
            RouteEnd::Output(machine, port) => {
                let next = self.leading_from_output(machine, port);
                if next.is_empty() {
                    return Err(format!(
                        "output {port:?} of machine {machine:?} is not connected"
                    ));
                }
                next
            }
            RouteEnd::Input(machine, port) => {
                let next = self.destinations(machine, &WireEnd::In(port.clone()));
                if next.is_empty() {
                    buffers.push(PortAddress {
                        machine: machine.clone(),
                        port: port.clone(),
                    });
                    return Ok(());
                }
                next
            }
        };
        route.push(end);
        for end in next {
            self.follow(end, route, buffers)?;
        }
        route.pop();
        Ok(())
    }

    /// Where the wires from the output `port` of the machine at `machine`
    /// lead: outward, by its parent's wires, then inward, by its own.
    fn leading_from_output(&self, machine: &[Key], port: &Key) -> Vec<RouteEnd> {
        let mut next = match machine.split_last() {
            Some((child, parent)) => {
                self.destinations(parent, &WireEnd::Child(child.clone(), port.clone()))
            }
            None => Vec::new(),
        };
        next.extend(self.destinations(machine, &WireEnd::Out(port.clone())));
        next
    }

    /// Where the wires that the machine at `owner` owns from `source` lead:
    /// its own, and the default ones it has not suppressed.
    fn destinations(&self, owner: &[Key], source: &WireEnd) -> Vec<RouteEnd> {
        let owned = self.wiring.get(owner);
        let mut destinations: Vec<&WireEnd> = owned
            .iter()
            .flat_map(|owned| owned.wires.iter())
            .filter(|wire| wire.source == *source)
            .map(|wire| &wire.destination)
            .collect();
        let defaults = self.default_destinations(owner, source);
        let suppressed = |wire: &Wire| owned.is_some_and(|owned| owned.suppressed.contains(wire));
        // A default wire the machine also owns as its own delivers once.
        let defaults = defaults
            .iter()
            .filter(|destination| {
                !suppressed(&Wire {
                    source: source.clone(),
                    destination: (*destination).clone(),
                })
            })
            .filter(|destination| !destinations.contains(destination))
            .collect::<Vec<_>>();
        destinations.extend(defaults);
        destinations
            .into_iter()
            .map(|destination| match destination {
                WireEnd::Child(child, port) => {
                    RouteEnd::Input(owner.iter().chain([child]).cloned().collect(), port.clone())
                }
                WireEnd::Out(port) => RouteEnd::Output(owner.to_vec(), port.clone()),
                WireEnd::In(port) => RouteEnd::Input(owner.to_vec(), port.clone()),
            })
            .collect()
    }

    /// The destinations of the default wires from `source` that the machine
    /// at `owner` would own.
    fn default_destinations(&self, owner: &[Key], source: &WireEnd) -> Vec<WireEnd> {
        if !defaults_apply(&self.wiring, self.snapshot.diagnostics, owner) {
            return Vec::new();
        }
        let port = &*DIAGNOSTICS;
        let candidates = match source {
            WireEnd::Child(_, source) if source == port => vec![WireEnd::In(port.clone())],
            WireEnd::In(source) => vec![WireEnd::Out(source.clone())],
            _ => Vec::new(),
        };
        candidates
            .into_iter()
            .filter(|destination| {
                is_default(
                    self.snapshot.diagnostics,
                    owner,
                    &Wire {
                        source: source.clone(),
                        destination: destination.clone(),
                    },
                )
            })
            .collect()
    }

    /// Whether a wire leads into the input `port` of the machine at
    /// `machine`, observing the wiring: one its parent owns from elsewhere,
    /// one it owns itself, from a child or its own port, or, for the
    /// diagnostics input, the default wiring that collects from every child.
    pub(in crate::api) fn input_connected(&mut self, machine: &[Key], port: &Key) -> bool {
        self.observe_wiring();
        let leads_into = |owner: &[Key], destination: &WireEnd| {
            self.wiring.get(owner).is_some_and(|owned| {
                owned
                    .wires
                    .iter()
                    .any(|wire| wire.destination == *destination)
            })
        };
        let from_parent = machine.split_last().is_some_and(|(child, parent)| {
            leads_into(parent, &WireEnd::Child(child.clone(), port.clone()))
        });
        let own = leads_into(machine, &WireEnd::In(port.clone()));
        let by_default = *port == *DIAGNOSTICS
            && defaults_apply(&self.wiring, self.snapshot.diagnostics, machine);
        from_parent || own || by_default
    }

    /// Whether a wire leads from the output `port` of the machine at
    /// `machine`, observing the wiring.
    pub(in crate::api) fn output_connected(&mut self, machine: &[Key], port: &Key) -> bool {
        self.observe_wiring();
        !self.leading_from_output(machine, port).is_empty()
    }

    /// A branch of this journal, for a fork within its transaction. It
    /// shares the transaction's observations, and records its own reads too,
    /// for its merge.
    pub(in crate::api) fn branch(&self) -> Self {
        let mut branch = self.clone();
        branch.branch = Some(PortBranch {
            cursors: self
                .cursors
                .iter()
                .map(|(address, cursor)| (address.clone(), cursor.next))
                .collect(),
            edits: self.edits.len(),
            appends: self.appends.len(),
            reads: Arc::new(Mutex::new(BranchReads::default())),
        });
        branch
    }

    /// Whether `branch`, a branch of this journal, cannot merge in a serial
    /// order: since the fork, this journal claimed from a buffer the branch
    /// read, or changed wiring the branch followed.
    pub(in crate::api) fn conflicts_with(&self, branch: &PortJournal) -> bool {
        let base = branch
            .branch
            .as_ref()
            .expect("only a branch merges into its journal");
        let reads = lock(&base.reads);
        let claimed_since = |address: &PortAddress| match self.cursors.get(address) {
            None => false,
            Some(cursor) => {
                cursor.next > base.cursors.get(address).copied().unwrap_or(cursor.start)
            }
        };
        reads.buffers.iter().any(claimed_since) || (reads.wiring && self.edits.len() > base.edits)
    }

    /// Merges `branch`, which [`Self::conflicts_with`] admitted, as if its
    /// actions ran now: its claims, appends and wiring edits.
    pub(in crate::api) fn merge(&mut self, branch: &PortJournal) {
        let base = branch
            .branch
            .as_ref()
            .expect("only a branch merges into its journal");
        for (address, cursor) in &branch.cursors {
            if base.cursors.get(address) != Some(&cursor.next) {
                self.cursors.insert(address.clone(), cursor.clone());
            }
        }
        self.appends
            .extend(branch.appends[base.appends..].iter().cloned());
        for edit in &branch.edits[base.edits..] {
            self.edit(edit.clone());
        }
        // What the branch read, this journal's own branch read too.
        if let Some(own) = &self.branch {
            let reads = lock(&base.reads);
            let mut own = lock(&own.reads);
            own.buffers.extend(reads.buffers.iter().cloned());
            own.wiring |= reads.wiring;
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
