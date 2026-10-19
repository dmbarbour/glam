//! Ports: the buffers of machines' receiving ports, and the wiring between
//! machines' ports, beside the runtime's other transactional events
//! (`machines-communicate-through-wired-ports`).
//!
//! Buffers and wiring are Rust structures with glam values as payloads, so
//! nothing here forces a value. A transaction claims from buffers through
//! FIFO cursors, appends to them, and edits the wiring. It reads committed
//! buffers only, never its own appends, as glam's log reads do, while its
//! routing follows its own wiring edits.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};

use rpds::RedBlackTreeMapSync;

use crate::api::Value;
use crate::core::Key;
use crate::runtime::RuntimeValueRoot;

/// A machine's port in one role: the machine's path from the root, and the
/// port's name.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::api) struct PortAddress {
    pub(in crate::api) machine: Vec<Key>,
    pub(in crate::api) port: Key,
}

/// One end of a wire, relative to the machine that owns the wire: a
/// descendant's port when the path is not empty, and the owner's own port
/// when it is.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::api) struct WireEnd {
    pub(in crate::api) path: Vec<Key>,
    pub(in crate::api) port: Key,
}

/// A wire a machine owns: from a descendant's output, or the owner's own
/// input, to a descendant's input, or the owner's own output.
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

type Wiring = RedBlackTreeMapSync<Vec<Key>, Arc<BTreeSet<Wire>>>;

/// The committed buffers and wiring. Cloning it is a snapshot.
#[derive(Clone)]
pub(in crate::api) struct PortState {
    buffers: RedBlackTreeMapSync<PortAddress, Arc<PortBuffer>>,
    /// Wires by the path of the machine that owns them.
    wiring: Wiring,
    /// Advanced by each commit that changes the wiring.
    wiring_revision: u64,
}

impl PortState {
    pub(in crate::api) fn new() -> Self {
        Self {
            buffers: RedBlackTreeMapSync::new_sync(),
            wiring: RedBlackTreeMapSync::new_sync(),
            wiring_revision: 0,
        }
    }

    /// Whether `journal`'s reads and routing still hold: no buffer it read
    /// was consumed, nor one it found short added to, and the wiring it
    /// followed is unchanged.
    pub(in crate::api) fn validate(&self, journal: &PortJournal) -> bool {
        let observations = journal
            .observations
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if observations.wiring && self.wiring_revision != journal.snapshot.wiring_revision {
            return false;
        }
        observations
            .reads
            .iter()
            .chain(&journal.cursors)
            .all(|(address, cursor)| self.holds(address, cursor))
    }

    fn holds(&self, address: &PortAddress, cursor: &PortCursor) -> bool {
        let (head, next) = match self.buffers.get(address) {
            Some(buffer) => (buffer.head, buffer.next()),
            None => (0, 0),
        };
        head == cursor.start
            && cursor.next <= next
            && (!cursor.observed_tail || next == cursor.next)
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
                apply_edit(&mut self.wiring, edit);
            }
            self.wiring_revision += 1;
        }
        changed
    }
}

/// A transaction's position in one buffer: its first sequence when the
/// transaction first read it, the next it would claim, and whether it saw
/// the buffer's end, so that an item added since invalidates it.
#[derive(Clone, Debug)]
struct PortCursor {
    start: u64,
    next: u64,
    observed_tail: bool,
}

#[derive(Clone)]
enum WiringEdit {
    Add(Vec<Key>, Wire),
    Remove(Vec<Key>, Wire),
}

fn apply_edit(wiring: &mut Wiring, edit: &WiringEdit) {
    let (owner, wire, add) = match edit {
        WiringEdit::Add(owner, wire) => (owner, wire, true),
        WiringEdit::Remove(owner, wire) => (owner, wire, false),
    };
    let mut wires = wiring.get(owner).cloned().unwrap_or_default();
    let set = Arc::make_mut(&mut wires);
    if add {
        set.insert(wire.clone());
    } else {
        set.remove(wire);
    }
    if set.is_empty() {
        wiring.remove_mut(owner);
    } else {
        wiring.insert_mut(owner.clone(), wires);
    }
}

/// What a transaction observed of the ports, shared by every alternative of
/// it, as its store reads are: reads made by a failed alternative still
/// count for the one that commits.
#[derive(Default)]
struct PortObservations {
    reads: BTreeMap<PortAddress, PortCursor>,
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
        let (head, items) = match self.snapshot.buffers.get(address) {
            Some(buffer) => (buffer.head, Some(buffer.clone())),
            None => (0, None),
        };
        let cursor = self
            .cursors
            .entry(address.clone())
            .or_insert_with(|| PortCursor {
                start: head,
                next: head,
                observed_tail: false,
            });
        let end = items.as_ref().map_or(0, |buffer| buffer.next());
        let claimed = if end - cursor.next < n as u64 {
            cursor.observed_tail = true;
            None
        } else {
            let buffer = items.expect("a buffer with items exists");
            let offset = (cursor.next - head) as usize;
            let values = buffer
                .items
                .range(offset..offset + n)
                .map(|root| Value::from_runtime_root(root.clone()))
                .collect();
            cursor.next += n as u64;
            Some(values)
        };
        let observation = cursor.clone();
        self.observe(address, observation);
        claimed
    }

    /// How many items the buffer at `address` holds beyond this journal's
    /// claims, observing its end.
    pub(in crate::api) fn available(&mut self, address: &PortAddress) -> usize {
        let (head, end) = self
            .snapshot
            .buffers
            .get(address)
            .map_or((0, 0), |buffer| (buffer.head, buffer.next()));
        let cursor = self
            .cursors
            .entry(address.clone())
            .or_insert_with(|| PortCursor {
                start: head,
                next: head,
                observed_tail: false,
            });
        cursor.observed_tail = true;
        let count = (end - cursor.next) as usize;
        let observation = cursor.clone();
        self.observe(address, observation);
        count
    }

    fn observe(&mut self, address: &PortAddress, cursor: PortCursor) {
        let mut observations = self
            .observations
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        observations
            .reads
            .entry(address.clone())
            .and_modify(|prior| {
                debug_assert_eq!(prior.start, cursor.start);
                prior.next = prior.next.max(cursor.next);
                prior.observed_tail |= cursor.observed_tail;
            })
            .or_insert(cursor);
        if let Some(branch) = &self.branch {
            lock(&branch.reads).buffers.insert(address.clone());
        }
    }

    fn observe_wiring(&mut self) {
        self.observations
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .wiring = true;
        if let Some(branch) = &self.branch {
            lock(&branch.reads).wiring = true;
        }
    }

    /// Appends `value` to the buffer at `address`, when this journal commits.
    pub(in crate::api) fn append(&mut self, address: PortAddress, value: Value) {
        self.appends.push((address, value.into_runtime_root()));
    }

    /// Adds `wire`, owned by the machine at `owner`.
    pub(in crate::api) fn wire(&mut self, owner: Vec<Key>, wire: Wire) {
        let edit = WiringEdit::Add(owner, wire);
        apply_edit(&mut self.wiring, &edit);
        self.edits.push(edit);
    }

    /// Removes `wire`, owned by the machine at `owner`, if it is there.
    pub(in crate::api) fn unwire(&mut self, owner: Vec<Key>, wire: Wire) {
        let edit = WiringEdit::Remove(owner, wire);
        apply_edit(&mut self.wiring, &edit);
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

    fn follow(
        &self,
        end: RouteEnd,
        route: &mut Vec<RouteEnd>,
        buffers: &mut Vec<PortAddress>,
    ) -> Result<(), String> {
        if route.contains(&end) {
            return Err(format!("the wiring loops through {end:?}"));
        }
        let next: Vec<RouteEnd> = match &end {
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
                let next = self.through_input(machine, port);
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
    /// lead: the wires its ancestors own, nearest last.
    fn leading_from_output(&self, machine: &[Key], port: &Key) -> Vec<RouteEnd> {
        (0..machine.len())
            .flat_map(|depth| {
                let owner = &machine[..depth];
                let source = WireEnd {
                    path: machine[depth..].to_vec(),
                    port: port.clone(),
                };
                self.destinations(owner, &source)
            })
            .collect()
    }

    /// Where the machine at `machine` passes its input `port` through to.
    fn through_input(&self, machine: &[Key], port: &Key) -> Vec<RouteEnd> {
        let source = WireEnd {
            path: Vec::new(),
            port: port.clone(),
        };
        self.destinations(machine, &source)
    }

    fn destinations(&self, owner: &[Key], source: &WireEnd) -> Vec<RouteEnd> {
        let Some(wires) = self.wiring.get(owner) else {
            return Vec::new();
        };
        wires
            .iter()
            .filter(|wire| wire.source == *source)
            .map(|wire| {
                let destination = &wire.destination;
                if destination.path.is_empty() {
                    RouteEnd::Output(owner.to_vec(), destination.port.clone())
                } else {
                    let machine = owner.iter().chain(&destination.path).cloned().collect();
                    RouteEnd::Input(machine, destination.port.clone())
                }
            })
            .collect()
    }

    /// Whether a wire leads into the input `port` of the machine at
    /// `machine`, observing the wiring.
    pub(in crate::api) fn input_connected(&mut self, machine: &[Key], port: &Key) -> bool {
        self.observe_wiring();
        (0..machine.len()).any(|depth| {
            let destination = WireEnd {
                path: machine[depth..].to_vec(),
                port: port.clone(),
            };
            self.wiring
                .get(&machine[..depth])
                .is_some_and(|wires| wires.iter().any(|wire| wire.destination == destination))
        })
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
            apply_edit(&mut self.wiring, edit);
            self.edits.push(edit.clone());
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
