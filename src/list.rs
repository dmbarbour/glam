//! Persistent lists with compact byte leaves, value leaves, and lazy holes.
//!
//! The list owns only its structural representation. `T` is an opaque lazy
//! hole; operations which may encounter one receive a forcing callback from
//! the caller. In particular, this module has no knowledge of core values or
//! evaluator environments.

use std::fmt;
use std::sync::Arc;

use bytes::Bytes;
use fingertrees::measure::Measured;
use fingertrees::monoid::Sum;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListItem<V> {
    Byte(u8),
    Value(V),
}

/// One non-forcing decomposition of the logical front of a persistent list.
///
/// A deferred chunk represents an unknown-length list segment, so its exact
/// strict tail cannot be known yet. `suffix` retains everything logically
/// following that chunk; callers may resume with `forced ++ suffix` after
/// evaluating the deferred value.
pub(crate) enum ListFrontStep<U, D, V, T> {
    Empty,
    Item { item: ListItem<U>, tail: List<V, T> },
    Deferred { deferred: D, suffix: List<V, T> },
}

/// One non-forcing decomposition of the logical back of a persistent list.
///
/// This is the right-to-left counterpart of [`ListFrontStep`]. A deferred
/// chunk retains the strict prefix which precedes it, allowing callers to
/// force only holes which must be crossed from the back.
pub(crate) enum ListBackStep<U, D, V, T> {
    Empty,
    Item { init: List<V, T>, item: ListItem<U> },
    Deferred { prefix: List<V, T>, deferred: D },
}

#[cfg(test)]
enum ListLookup<V> {
    Found(ListItem<V>),
    Exhausted(usize),
}

pub struct List<V, T>(Arc<ListNode<V, T>>);

#[derive(Debug)]
enum ListNode<V, T> {
    Empty,
    Bytes(Bytes),
    Values(SharedSlice<V>),
    Concat(List<V, T>, List<V, T>),
    Finger(FingerList<V>),
    Thunk(T),
}

type FingerList<V> = fingertrees::sync::FingerTree<ListChunk<V>>;

#[derive(Debug, PartialEq, Eq)]
enum ListChunk<V> {
    Bytes(Bytes),
    Values(SharedSlice<V>),
}

struct SharedSlice<T> {
    data: Arc<[T]>,
    start: usize,
    len: usize,
}

impl<V, T> Clone for List<V, T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<V: fmt::Debug, T: fmt::Debug> fmt::Debug for List<V, T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl<V> Clone for ListChunk<V> {
    fn clone(&self) -> Self {
        match self {
            Self::Bytes(bytes) => Self::Bytes(bytes.clone()),
            Self::Values(values) => Self::Values(values.clone()),
        }
    }
}

impl<T> Clone for SharedSlice<T> {
    fn clone(&self) -> Self {
        Self {
            data: Arc::clone(&self.data),
            start: self.start,
            len: self.len,
        }
    }
}

/// Logical work performed while enumerating a list without forcing thunks.
///
/// Counts describe visits, not unique physical `Arc` nodes. Reusing one shared
/// spine twice therefore contributes twice, matching the initial mark
/// collector's deliberately simple tracing policy.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct LogicalListVisitStats {
    pub(crate) node_visits: usize,
    pub(crate) chunk_visits: usize,
    pub(crate) byte_segments: usize,
    pub(crate) shared_value_slices: usize,
    pub(crate) value_items: usize,
    pub(crate) thunk_items: usize,
}

/// One non-structural part discovered by [`List::visit_logical_parts`].
pub(crate) enum LogicalListPart<'part, V, T> {
    Bytes(&'part [u8]),
    Values(&'part [V]),
    Thunk(&'part T),
}

/// One borrowed logical item used by representation-level list operations.
///
/// Unlike [`ListItem`], this view neither clones strict values nor forces a
/// deferred segment. A thunk remains one retained representation item rather
/// than pretending to know how many eventual list items it may produce.
pub(crate) enum LogicalListItemRef<'part, V, T> {
    Byte(u8),
    Value(&'part V),
    Thunk(&'part T),
}

enum LogicalListCursorPart<'part, V, T> {
    Bytes(Bytes),
    Values(SharedSlice<V>),
    Thunk(&'part T),
}

struct LogicalListItemCursor<'part, V, T> {
    parts: Vec<LogicalListCursorPart<'part, V, T>>,
    part_index: usize,
    offset: usize,
}

impl<'part, V, T> LogicalListItemCursor<'part, V, T> {
    fn new(parts: Vec<LogicalListCursorPart<'part, V, T>>) -> Self {
        Self {
            parts,
            part_index: 0,
            offset: 0,
        }
    }

    fn current(&self) -> Option<LogicalListItemRef<'_, V, T>> {
        match self.parts.get(self.part_index)? {
            LogicalListCursorPart::Bytes(bytes) => bytes
                .get(self.offset)
                .copied()
                .map(LogicalListItemRef::Byte),
            LogicalListCursorPart::Values(values) => values
                .as_slice()
                .get(self.offset)
                .map(LogicalListItemRef::Value),
            LogicalListCursorPart::Thunk(thunk) => {
                (self.offset == 0).then_some(LogicalListItemRef::Thunk(*thunk))
            }
        }
    }

    fn advance(&mut self) {
        self.offset += 1;
        let exhausted = match &self.parts[self.part_index] {
            LogicalListCursorPart::Bytes(bytes) => self.offset == bytes.len(),
            LogicalListCursorPart::Values(values) => self.offset == values.len(),
            LogicalListCursorPart::Thunk(_) => self.offset == 1,
        };
        if exhausted {
            self.part_index += 1;
            self.offset = 0;
        }
    }
}

impl<T> SharedSlice<T> {
    fn from_vec(values: Vec<T>) -> Self {
        let len = values.len();
        Self {
            data: Arc::from(values),
            start: 0,
            len,
        }
    }

    fn len(&self) -> usize {
        self.len
    }

    fn as_slice(&self) -> &[T] {
        &self.data[self.start..self.start + self.len]
    }

    fn slice(&self, start: usize, end: usize) -> Self {
        assert!(start <= end);
        assert!(end <= self.len);
        Self {
            data: self.data.clone(),
            start: self.start + start,
            len: end - start,
        }
    }
}

impl<T: fmt::Debug> fmt::Debug for SharedSlice<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_slice().fmt(f)
    }
}

impl<T: PartialEq> PartialEq for SharedSlice<T> {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl<T: Eq> Eq for SharedSlice<T> {}

impl<V> ListChunk<V> {
    fn len(&self) -> usize {
        match self {
            Self::Bytes(bytes) => bytes.len(),
            Self::Values(values) => values.len(),
        }
    }

    fn slice(&self, start: usize, end: usize) -> Option<Self> {
        assert!(start <= end);
        assert!(end <= self.len());
        if start == end {
            None
        } else {
            Some(match self {
                Self::Bytes(bytes) => Self::Bytes(bytes.slice(start..end)),
                Self::Values(values) => Self::Values(values.slice(start, end)),
            })
        }
    }

    fn item_at_by<U>(
        &self,
        index: usize,
        duplicate_value: &mut impl FnMut(&V) -> U,
    ) -> Option<ListItem<U>> {
        match self {
            Self::Bytes(bytes) => bytes.get(index).copied().map(ListItem::Byte),
            Self::Values(values) => values
                .as_slice()
                .get(index)
                .map(duplicate_value)
                .map(ListItem::Value),
        }
    }

    fn for_each_segment<E>(
        &self,
        on_bytes: &mut impl FnMut(&[u8]) -> Result<(), E>,
        on_values: &mut impl FnMut(&[V]) -> Result<(), E>,
    ) -> Result<(), E> {
        match self {
            Self::Bytes(bytes) => on_bytes(bytes),
            Self::Values(values) => on_values(values.as_slice()),
        }
    }
}

impl<V> Measured for ListChunk<V> {
    type Measure = Sum<usize>;

    fn measure(&self) -> Self::Measure {
        Sum(self.len())
    }
}

impl<V: Clone + PartialEq, T> PartialEq for List<V, T> {
    fn eq(&self, other: &Self) -> bool {
        if Arc::ptr_eq(&self.0, &other.0) {
            return true;
        }
        let (Some(self_len), Some(other_len)) = (self.known_len(), other.known_len()) else {
            return false;
        };
        self_len == other_len && self.items_for_eq() == other.items_for_eq()
    }
}

impl<V: Clone + Eq, T> Eq for List<V, T> {}

impl<V, T> List<V, T> {
    pub fn empty() -> Self {
        Self(Arc::new(ListNode::Empty))
    }

    pub fn from_bytes(bytes: impl Into<Bytes>) -> Self {
        let bytes = bytes.into();
        if bytes.is_empty() {
            Self::empty()
        } else {
            Self(Arc::new(ListNode::Bytes(bytes)))
        }
    }

    pub fn from_values(values: Vec<V>) -> Self {
        if values.is_empty() {
            Self::empty()
        } else {
            Self(Arc::new(ListNode::Values(SharedSlice::from_vec(values))))
        }
    }

    /// Borrows the one strict value leaf represented by this list.
    ///
    /// The canonical empty list also qualifies. Byte leaves, concatenations,
    /// finger trees, and deferred tails deliberately do not.
    pub(crate) fn value_slice(&self) -> Option<&[V]> {
        match self.0.as_ref() {
            ListNode::Empty => Some(&[]),
            ListNode::Values(values) => Some(values.as_slice()),
            ListNode::Bytes(_)
            | ListNode::Concat(_, _)
            | ListNode::Finger(_)
            | ListNode::Thunk(_) => None,
        }
    }

    pub fn from_thunk(thunk: T) -> Self {
        Self(Arc::new(ListNode::Thunk(thunk)))
    }

    /// Maps exactly one representation node without forcing a deferred part.
    ///
    /// A concatenation retains its shape, but both children become deferred
    /// recursive transformations. Strict leaves are transformed as one unit;
    /// in particular, this operation does not invent finer concatenation
    /// boundaries for a byte slice, value slice, or finger tree.
    pub(crate) fn map_root_step<U, S>(
        &self,
        map_byte: &mut impl FnMut(u8) -> U,
        map_value: &mut impl FnMut(&V) -> U,
        defer_list: &mut impl FnMut(Self) -> S,
        defer_thunk: &mut impl FnMut(&T) -> S,
    ) -> List<U, S> {
        match self.0.as_ref() {
            ListNode::Empty => List::empty(),
            ListNode::Bytes(bytes) => {
                List::from_values(bytes.iter().copied().map(map_byte).collect())
            }
            ListNode::Values(values) => {
                List::from_values(values.as_slice().iter().map(map_value).collect())
            }
            ListNode::Concat(left, right) => List::concat(
                List::from_thunk(defer_list(left.clone())),
                List::from_thunk(defer_list(right.clone())),
            ),
            ListNode::Finger(finger) => {
                let mut mapped = FingerList::new();
                for chunk in finger.iter() {
                    let values = match chunk {
                        ListChunk::Bytes(bytes) => {
                            bytes.iter().copied().map(&mut *map_byte).collect()
                        }
                        ListChunk::Values(values) => {
                            values.as_slice().iter().map(&mut *map_value).collect()
                        }
                    };
                    mapped = mapped.push_right(ListChunk::Values(SharedSlice::from_vec(values)));
                }
                List::from_finger(mapped)
            }
            ListNode::Thunk(thunk) => List::from_thunk(defer_thunk(thunk)),
        }
    }

    /// Flat-maps exactly one representation node without forcing a deferred
    /// part.
    ///
    /// A reached strict leaf is allowed to produce one list per item. Those
    /// lists are joined with logarithmic concatenation depth without looking
    /// inside them. A source concatenation or thunk instead remains deferred,
    /// matching [`Self::map_root_step`]'s one-node structural boundary.
    pub(crate) fn flat_map_root_step<U, S, E>(
        &self,
        map_byte: &mut impl FnMut(u8) -> Result<List<U, S>, E>,
        map_value: &mut impl FnMut(&V) -> Result<List<U, S>, E>,
        defer_list: &mut impl FnMut(Self) -> S,
        defer_thunk: &mut impl FnMut(&T) -> S,
    ) -> Result<List<U, S>, E> {
        let mapped = match self.0.as_ref() {
            ListNode::Empty => List::empty(),
            ListNode::Bytes(bytes) => {
                let mut parts = Vec::with_capacity(bytes.len());
                for byte in bytes.iter().copied() {
                    parts.push(map_byte(byte)?);
                }
                List::concat_balanced(parts)
            }
            ListNode::Values(values) => {
                let mut parts = Vec::with_capacity(values.len());
                for value in values.as_slice() {
                    parts.push(map_value(value)?);
                }
                List::concat_balanced(parts)
            }
            ListNode::Concat(left, right) => List::concat(
                List::from_thunk(defer_list(left.clone())),
                List::from_thunk(defer_list(right.clone())),
            ),
            ListNode::Finger(finger) => {
                let mut parts = Vec::with_capacity(finger.measure().0);
                for chunk in finger.iter() {
                    match chunk {
                        ListChunk::Bytes(bytes) => {
                            for byte in bytes.iter().copied() {
                                parts.push(map_byte(byte)?);
                            }
                        }
                        ListChunk::Values(values) => {
                            for value in values.as_slice() {
                                parts.push(map_value(value)?);
                            }
                        }
                    }
                }
                List::concat_balanced(parts)
            }
            ListNode::Thunk(thunk) => List::from_thunk(defer_thunk(thunk)),
        };
        Ok(mapped)
    }

    fn concat_balanced(mut lists: Vec<List<V, T>>) -> Self {
        if lists.is_empty() {
            return Self::empty();
        }
        while lists.len() > 1 {
            let mut next = Vec::with_capacity(lists.len().div_ceil(2));
            let mut current = lists.into_iter();
            while let Some(left) = current.next() {
                next.push(match current.next() {
                    Some(right) => Self::concat(left, right),
                    None => left,
                });
            }
            lists = next;
        }
        lists.pop().expect("a non-empty concatenation has one root")
    }

    fn from_value_slice(values: SharedSlice<V>) -> Self {
        if values.len() == 0 {
            Self::empty()
        } else {
            Self(Arc::new(ListNode::Values(values)))
        }
    }

    pub fn concat(left: Self, right: Self) -> Self {
        if left.is_empty() {
            right
        } else if right.is_empty() {
            left
        } else {
            Self(Arc::new(ListNode::Concat(left, right)))
        }
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.known_len()
            .expect("list length requires all lazy list chunks to be forced")
    }

    pub fn known_len(&self) -> Option<usize> {
        match self.0.as_ref() {
            ListNode::Empty => Some(0),
            ListNode::Bytes(bytes) => Some(bytes.len()),
            ListNode::Values(values) => Some(values.len()),
            ListNode::Concat(left, right) => Some(left.known_len()? + right.known_len()?),
            ListNode::Finger(finger) => Some(finger.measure().0),
            ListNode::Thunk(_) => None,
        }
    }

    #[cfg(test)]
    pub fn try_len<E>(
        &self,
        force_thunk: &mut impl FnMut(&T) -> Result<Self, E>,
    ) -> Result<usize, E> {
        match self.0.as_ref() {
            ListNode::Empty => Ok(0),
            ListNode::Bytes(bytes) => Ok(bytes.len()),
            ListNode::Values(values) => Ok(values.len()),
            ListNode::Concat(left, right) => {
                Ok(left.try_len(force_thunk)? + right.try_len(force_thunk)?)
            }
            ListNode::Finger(finger) => Ok(finger.measure().0),
            ListNode::Thunk(thunk) => force_thunk(thunk)?.try_len(force_thunk),
        }
    }

    #[cfg(test)]
    pub fn balanced(&self) -> Self {
        Self::from_finger(self.to_finger())
    }

    pub fn try_balanced<E>(
        &self,
        force_thunk: &mut impl FnMut(&T) -> Result<Self, E>,
    ) -> Result<Self, E> {
        Ok(Self::from_finger(self.to_finger_with(force_thunk)?))
    }

    #[cfg(test)]
    pub fn slice(&self, start: usize, end: usize) -> Self {
        assert!(start <= end);
        assert!(end <= self.len());
        self.slice_checked(start, end)
    }

    #[cfg(test)]
    pub fn try_split_at<E>(
        &self,
        index: usize,
        force_thunk: &mut impl FnMut(&T) -> Result<Self, E>,
    ) -> Result<Option<(Self, Self)>, E> {
        self.split_at_checked_with(index, force_thunk)
    }

    #[cfg(test)]
    pub fn split_from_end(&self, count: usize) -> Option<(Self, Self)> {
        self.split_from_end_checked(count)
    }

    /// Returns the zero-based item at `index`, forcing only lazy chunks which
    /// must be crossed to reach it.
    #[cfg(test)]
    pub fn try_at<E>(
        &self,
        index: usize,
        force_thunk: &mut impl FnMut(&T) -> Result<Self, E>,
    ) -> Result<Option<ListItem<V>>, E>
    where
        V: Clone,
    {
        self.try_at_by(index, &mut Clone::clone, force_thunk)
    }

    /// Returns the zero-based item at `index`, using the caller's operation to
    /// duplicate a strict value leaf.
    ///
    /// Structural list shells and byte items require no element duplication.
    /// The callback is invoked only after any required thunk has been forced,
    /// so a caller may keep forcing and managed-value access in disjoint
    /// scopes.
    #[cfg(test)]
    pub fn try_at_by<E, U>(
        &self,
        index: usize,
        duplicate_value: &mut impl FnMut(&V) -> U,
        force_thunk: &mut impl FnMut(&T) -> Result<Self, E>,
    ) -> Result<Option<ListItem<U>>, E> {
        Ok(
            match self.lookup_at_with_by(index, duplicate_value, force_thunk)? {
                ListLookup::Found(item) => Some(item),
                ListLookup::Exhausted(_) => None,
            },
        )
    }

    #[cfg(test)]
    pub fn try_pop_front<E>(
        &self,
        force_thunk: &mut impl FnMut(&T) -> Result<Self, E>,
    ) -> Result<Option<(ListItem<V>, Self)>, E>
    where
        V: Clone,
    {
        self.try_pop_front_by(&mut Clone::clone, force_thunk)
    }

    /// Removes the first item, using the caller's operation to duplicate a
    /// strict value leaf while all returned list shells continue sharing their
    /// original persistent structure.
    pub fn try_pop_front_by<E, U>(
        &self,
        duplicate_value: &mut impl FnMut(&V) -> U,
        force_thunk: &mut impl FnMut(&T) -> Result<Self, E>,
    ) -> Result<Option<(ListItem<U>, Self)>, E> {
        match self.0.as_ref() {
            ListNode::Empty => Ok(None),
            ListNode::Bytes(bytes) => Ok(bytes.first().map(|byte| {
                (
                    ListItem::Byte(*byte),
                    Self::from_bytes(bytes.slice(1..bytes.len())),
                )
            })),
            ListNode::Values(values) => {
                let Some(first) = values.as_slice().first() else {
                    return Ok(None);
                };
                Ok(Some((
                    ListItem::Value(duplicate_value(first)),
                    Self::from_value_slice(values.slice(1, values.len())),
                )))
            }
            ListNode::Concat(left, right) => {
                if let Some((first, left_tail)) =
                    left.try_pop_front_by(duplicate_value, force_thunk)?
                {
                    Ok(Some((first, Self::concat(left_tail, right.clone()))))
                } else {
                    right.try_pop_front_by(duplicate_value, force_thunk)
                }
            }
            ListNode::Finger(finger) => {
                let Some((chunk, mut rest)) = finger.view_left() else {
                    return Ok(None);
                };
                let Some(value) = chunk.item_at_by(0, duplicate_value) else {
                    unreachable!("finger trees do not store empty chunks");
                };
                if let Some(chunk_tail) = chunk.slice(1, chunk.len()) {
                    rest = rest.push_left(chunk_tail);
                }
                Ok(Some((value, Self::from_finger(rest))))
            }
            ListNode::Thunk(thunk) => {
                force_thunk(thunk)?.try_pop_front_by(duplicate_value, force_thunk)
            }
        }
    }

    /// Decomposes one logical front item without forcing a deferred chunk.
    ///
    /// The explicit local worklist bounds Rust-stack use for arbitrarily deep
    /// `Concat` spines. Only the selected strict value or deferred chunk is
    /// duplicated; the returned tail continues to share all list structure.
    pub(crate) fn pop_front_step_by<U, D>(
        &self,
        duplicate_value: &mut impl FnMut(&V) -> U,
        duplicate_deferred: &mut impl FnMut(&T) -> D,
    ) -> ListFrontStep<U, D, V, T> {
        let mut worklist = vec![self.clone()];

        while let Some(list) = worklist.pop() {
            match list.0.as_ref() {
                ListNode::Empty => {}
                ListNode::Bytes(bytes) => {
                    let item = ListItem::Byte(
                        *bytes.first().expect("a canonical byte leaf is never empty"),
                    );
                    let local_tail = Self::from_bytes(bytes.slice(1..bytes.len()));
                    return ListFrontStep::Item {
                        item,
                        tail: Self::join_logical_suffix(local_tail, &worklist),
                    };
                }
                ListNode::Values(values) => {
                    let first = values
                        .as_slice()
                        .first()
                        .expect("a canonical value leaf is never empty");
                    let local_tail = Self::from_value_slice(values.slice(1, values.len()));
                    return ListFrontStep::Item {
                        item: ListItem::Value(duplicate_value(first)),
                        tail: Self::join_logical_suffix(local_tail, &worklist),
                    };
                }
                ListNode::Concat(left, right) => {
                    worklist.push(right.clone());
                    worklist.push(left.clone());
                }
                ListNode::Finger(finger) => {
                    let (chunk, mut rest) = finger
                        .view_left()
                        .expect("a canonical finger-tree node is never empty");
                    let item = chunk
                        .item_at_by(0, duplicate_value)
                        .expect("finger trees do not store empty chunks");
                    if let Some(chunk_tail) = chunk.slice(1, chunk.len()) {
                        rest = rest.push_left(chunk_tail);
                    }
                    return ListFrontStep::Item {
                        item,
                        tail: Self::join_logical_suffix(Self::from_finger(rest), &worklist),
                    };
                }
                ListNode::Thunk(thunk) => {
                    return ListFrontStep::Deferred {
                        deferred: duplicate_deferred(thunk),
                        suffix: Self::join_logical_suffix(Self::empty(), &worklist),
                    };
                }
            }
        }

        ListFrontStep::Empty
    }

    fn join_logical_suffix(mut prefix: Self, pending: &[Self]) -> Self {
        for suffix in pending.iter().rev() {
            prefix = Self::concat(prefix, suffix.clone());
        }
        prefix
    }

    /// Removes the final item while forcing only lazy chunks which must be
    /// crossed from the right edge to find it.
    #[cfg(test)]
    pub fn try_pop_back<E>(
        &self,
        force_thunk: &mut impl FnMut(&T) -> Result<Self, E>,
    ) -> Result<Option<(Self, ListItem<V>)>, E>
    where
        V: Clone,
    {
        self.try_pop_back_by(&mut Clone::clone, force_thunk)
    }

    /// Removes the final item, using the caller's operation to duplicate a
    /// strict value leaf while preserving persistent sharing in the prefix.
    pub fn try_pop_back_by<E, U>(
        &self,
        duplicate_value: &mut impl FnMut(&V) -> U,
        force_thunk: &mut impl FnMut(&T) -> Result<Self, E>,
    ) -> Result<Option<(Self, ListItem<U>)>, E> {
        match self.0.as_ref() {
            ListNode::Empty => Ok(None),
            ListNode::Bytes(bytes) => Ok(bytes.last().map(|byte| {
                (
                    Self::from_bytes(bytes.slice(0..bytes.len() - 1)),
                    ListItem::Byte(*byte),
                )
            })),
            ListNode::Values(values) => {
                let Some(last) = values.as_slice().last() else {
                    return Ok(None);
                };
                Ok(Some((
                    Self::from_value_slice(values.slice(0, values.len() - 1)),
                    ListItem::Value(duplicate_value(last)),
                )))
            }
            ListNode::Concat(left, right) => {
                if let Some((right_init, last)) =
                    right.try_pop_back_by(duplicate_value, force_thunk)?
                {
                    Ok(Some((Self::concat(left.clone(), right_init), last)))
                } else {
                    left.try_pop_back_by(duplicate_value, force_thunk)
                }
            }
            ListNode::Finger(finger) => {
                let Some((chunk, mut rest)) = finger.view_right() else {
                    return Ok(None);
                };
                let Some(value) = chunk.item_at_by(chunk.len() - 1, duplicate_value) else {
                    unreachable!("finger trees do not store empty chunks");
                };
                if let Some(chunk_init) = chunk.slice(0, chunk.len() - 1) {
                    rest = rest.push_right(chunk_init);
                }
                Ok(Some((Self::from_finger(rest), value)))
            }
            ListNode::Thunk(thunk) => {
                force_thunk(thunk)?.try_pop_back_by(duplicate_value, force_thunk)
            }
        }
    }

    /// Performs one non-forcing decomposition from the logical back.
    ///
    /// The returned prefix shares the original persistent structure. A
    /// deferred chunk is reported with the exact strict prefix which must be
    /// reattached after that chunk is evaluated.
    pub(crate) fn pop_back_step_by<U, D>(
        &self,
        duplicate_value: &mut impl FnMut(&V) -> U,
        duplicate_deferred: &mut impl FnMut(&T) -> D,
    ) -> ListBackStep<U, D, V, T> {
        let mut worklist = vec![self.clone()];
        while let Some(current) = worklist.pop() {
            match current.0.as_ref() {
                ListNode::Empty => {}
                ListNode::Bytes(bytes) => {
                    let index = bytes.len() - 1;
                    return ListBackStep::Item {
                        init: Self::join_logical_prefix(
                            Self::from_bytes(bytes.slice(0..index)),
                            &worklist,
                        ),
                        item: ListItem::Byte(bytes[index]),
                    };
                }
                ListNode::Values(values) => {
                    let index = values.len() - 1;
                    let last = values
                        .as_slice()
                        .last()
                        .expect("a canonical value leaf is never empty");
                    return ListBackStep::Item {
                        init: Self::join_logical_prefix(
                            Self::from_value_slice(values.slice(0, index)),
                            &worklist,
                        ),
                        item: ListItem::Value(duplicate_value(last)),
                    };
                }
                ListNode::Concat(left, right) => {
                    worklist.push(left.clone());
                    worklist.push(right.clone());
                }
                ListNode::Finger(finger) => {
                    let (chunk, mut rest) = finger
                        .view_right()
                        .expect("a canonical finger-tree node is never empty");
                    let index = chunk.len() - 1;
                    let item = chunk
                        .item_at_by(index, duplicate_value)
                        .expect("finger trees do not store empty chunks");
                    if let Some(chunk_init) = chunk.slice(0, index) {
                        rest = rest.push_right(chunk_init);
                    }
                    return ListBackStep::Item {
                        init: Self::join_logical_prefix(Self::from_finger(rest), &worklist),
                        item,
                    };
                }
                ListNode::Thunk(thunk) => {
                    return ListBackStep::Deferred {
                        prefix: Self::join_logical_prefix(Self::empty(), &worklist),
                        deferred: duplicate_deferred(thunk),
                    };
                }
            }
        }

        ListBackStep::Empty
    }

    fn join_logical_prefix(mut suffix: Self, pending: &[Self]) -> Self {
        for prefix in pending.iter().rev() {
            suffix = Self::concat(prefix.clone(), suffix);
        }
        suffix
    }

    #[cfg(test)]
    fn lookup_at_with_by<E, U>(
        &self,
        index: usize,
        duplicate_value: &mut impl FnMut(&V) -> U,
        force_thunk: &mut impl FnMut(&T) -> Result<Self, E>,
    ) -> Result<ListLookup<U>, E> {
        Ok(match self.0.as_ref() {
            ListNode::Empty => ListLookup::Exhausted(0),
            ListNode::Bytes(bytes) => bytes
                .get(index)
                .copied()
                .map(|byte| ListLookup::Found(ListItem::Byte(byte)))
                .unwrap_or_else(|| ListLookup::Exhausted(bytes.len())),
            ListNode::Values(values) => values
                .as_slice()
                .get(index)
                .map(duplicate_value)
                .map(|value| ListLookup::Found(ListItem::Value(value)))
                .unwrap_or_else(|| ListLookup::Exhausted(values.len())),
            ListNode::Concat(left, right) => {
                match left.lookup_at_with_by(index, duplicate_value, force_thunk)? {
                    found @ ListLookup::Found(_) => found,
                    ListLookup::Exhausted(left_len) => {
                        match right.lookup_at_with_by(
                            index - left_len,
                            duplicate_value,
                            force_thunk,
                        )? {
                            found @ ListLookup::Found(_) => found,
                            ListLookup::Exhausted(right_len) => {
                                ListLookup::Exhausted(left_len + right_len)
                            }
                        }
                    }
                }
            }
            ListNode::Finger(finger) => {
                let len = finger.measure().0;
                if index >= len {
                    ListLookup::Exhausted(len)
                } else {
                    let (_, right) = Self::split_finger_at(finger, index);
                    let Some((chunk, _)) = right.view_left() else {
                        unreachable!("an in-bounds finger-tree index leaves a right chunk");
                    };
                    let Some(item) = chunk.item_at_by(0, duplicate_value) else {
                        unreachable!("finger trees do not store empty chunks");
                    };
                    ListLookup::Found(item)
                }
            }
            ListNode::Thunk(thunk) => {
                force_thunk(thunk)?.lookup_at_with_by(index, duplicate_value, force_thunk)?
            }
        })
    }

    pub fn for_each_segment<E>(
        &self,
        on_bytes: &mut impl FnMut(&[u8]) -> Result<(), E>,
        on_values: &mut impl FnMut(&[V]) -> Result<(), E>,
    ) -> Result<(), E> {
        match self.0.as_ref() {
            ListNode::Empty => Ok(()),
            ListNode::Bytes(bytes) => on_bytes(bytes),
            ListNode::Values(values) => on_values(values.as_slice()),
            ListNode::Concat(left, right) => {
                left.for_each_segment(on_bytes, on_values)?;
                right.for_each_segment(on_bytes, on_values)
            }
            ListNode::Finger(finger) => finger
                .iter()
                .try_for_each(|chunk| chunk.for_each_segment(on_bytes, on_values)),
            ListNode::Thunk(_) => {
                panic!("list segment traversal requires all lazy list chunks to be forced")
            }
        }
    }

    /// Visits byte slices, strict value slices, and deferred thunks without
    /// forcing any deferred part.
    ///
    /// `Concat` traversal uses an explicit local worklist so an unbalanced
    /// syntax-built list cannot consume the Rust call stack. Finger trees keep
    /// their library iterator; byte segments are edge-free but remain visible
    /// in the returned work counters.
    pub(crate) fn visit_logical_parts(
        &self,
        visit: &mut impl FnMut(LogicalListPart<'_, V, T>),
    ) -> LogicalListVisitStats {
        let mut stats = LogicalListVisitStats::default();
        let mut worklist = vec![self];

        while let Some(list) = worklist.pop() {
            stats.node_visits += 1;
            match list.0.as_ref() {
                ListNode::Empty => {}
                ListNode::Bytes(bytes) => {
                    stats.byte_segments += 1;
                    visit(LogicalListPart::Bytes(bytes.as_ref()));
                }
                ListNode::Values(values) => {
                    stats.shared_value_slices += 1;
                    stats.value_items += values.len();
                    visit(LogicalListPart::Values(values.as_slice()));
                }
                ListNode::Concat(left, right) => {
                    // LIFO insertion preserves ordinary left-to-right logical
                    // order while bounding traversal by heap storage.
                    worklist.push(right);
                    worklist.push(left);
                }
                ListNode::Finger(finger) => {
                    for chunk in finger.iter() {
                        stats.chunk_visits += 1;
                        match chunk {
                            ListChunk::Bytes(bytes) => {
                                stats.byte_segments += 1;
                                visit(LogicalListPart::Bytes(bytes.as_ref()));
                            }
                            ListChunk::Values(values) => {
                                stats.shared_value_slices += 1;
                                stats.value_items += values.len();
                                visit(LogicalListPart::Values(values.as_slice()));
                            }
                        }
                    }
                }
                ListNode::Thunk(thunk) => {
                    stats.thunk_items += 1;
                    visit(LogicalListPart::Thunk(thunk));
                }
            }
        }

        stats
    }

    /// Compares retained logical items without cloning values or forcing
    /// deferred segments.
    ///
    /// Segment boundaries are deliberately ignored: two adjacent byte or
    /// strict-value leaves compare like the equivalent single leaf. The
    /// caller owns comparison of borrowed values and opaque thunk identities.
    pub(crate) fn same_logical_items_by(
        &self,
        other: &Self,
        mut same_item: impl FnMut(LogicalListItemRef<'_, V, T>, LogicalListItemRef<'_, V, T>) -> bool,
    ) -> bool {
        if Arc::ptr_eq(&self.0, &other.0) {
            return true;
        }

        let left_parts = self.logical_cursor_parts();
        let right_parts = other.logical_cursor_parts();
        let mut left = LogicalListItemCursor::new(left_parts);
        let mut right = LogicalListItemCursor::new(right_parts);

        loop {
            match (left.current(), right.current()) {
                (None, None) => return true,
                (Some(left_item), Some(right_item)) => {
                    if !same_item(left_item, right_item) {
                        return false;
                    }
                    left.advance();
                    right.advance();
                }
                _ => return false,
            }
        }
    }

    fn logical_cursor_parts(&self) -> Vec<LogicalListCursorPart<'_, V, T>> {
        let mut parts = Vec::new();
        let mut worklist = vec![self];

        while let Some(list) = worklist.pop() {
            match list.0.as_ref() {
                ListNode::Empty => {}
                ListNode::Bytes(bytes) => {
                    parts.push(LogicalListCursorPart::Bytes(bytes.clone()));
                }
                ListNode::Values(values) => {
                    parts.push(LogicalListCursorPart::Values(values.clone()));
                }
                ListNode::Concat(left, right) => {
                    worklist.push(right);
                    worklist.push(left);
                }
                ListNode::Finger(finger) => {
                    parts.extend(finger.iter().map(|chunk| match chunk {
                        ListChunk::Bytes(bytes) => LogicalListCursorPart::Bytes(bytes),
                        ListChunk::Values(values) => LogicalListCursorPart::Values(values),
                    }));
                }
                ListNode::Thunk(thunk) => {
                    parts.push(LogicalListCursorPart::Thunk(thunk));
                }
            }
        }

        parts
    }

    pub fn try_for_each_segment<E>(
        &self,
        on_bytes: &mut impl FnMut(&[u8]) -> Result<(), E>,
        on_values: &mut impl FnMut(&[V]) -> Result<(), E>,
        force_thunk: &mut impl FnMut(&T) -> Result<Self, E>,
    ) -> Result<(), E> {
        match self.0.as_ref() {
            ListNode::Empty => Ok(()),
            ListNode::Bytes(bytes) => on_bytes(bytes),
            ListNode::Values(values) => on_values(values.as_slice()),
            ListNode::Concat(left, right) => {
                left.try_for_each_segment(on_bytes, on_values, force_thunk)?;
                right.try_for_each_segment(on_bytes, on_values, force_thunk)
            }
            ListNode::Finger(finger) => finger
                .iter()
                .try_for_each(|chunk| chunk.for_each_segment(on_bytes, on_values)),
            ListNode::Thunk(thunk) => {
                force_thunk(thunk)?.try_for_each_segment(on_bytes, on_values, force_thunk)
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        matches!(self.0.as_ref(), ListNode::Empty)
    }

    #[cfg(test)]
    pub(crate) fn shares_spine_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    fn from_finger(finger: FingerList<V>) -> Self {
        if finger.is_empty() {
            Self::empty()
        } else {
            Self(Arc::new(ListNode::Finger(finger)))
        }
    }

    #[cfg(test)]
    fn to_finger(&self) -> FingerList<V> {
        let mut finger = FingerList::new();
        self.push_chunks_into(&mut finger);
        finger
    }

    fn to_finger_with<E>(
        &self,
        force_thunk: &mut impl FnMut(&T) -> Result<Self, E>,
    ) -> Result<FingerList<V>, E> {
        let mut finger = FingerList::new();
        self.push_chunks_into_with(&mut finger, force_thunk)?;
        Ok(finger)
    }

    #[cfg(test)]
    fn push_chunks_into(&self, finger: &mut FingerList<V>) {
        match self.0.as_ref() {
            ListNode::Empty => {}
            ListNode::Bytes(bytes) => {
                *finger = finger.push_right(ListChunk::Bytes(bytes.clone()));
            }
            ListNode::Values(values) => {
                *finger = finger.push_right(ListChunk::Values(values.clone()));
            }
            ListNode::Concat(left, right) => {
                left.push_chunks_into(finger);
                right.push_chunks_into(finger);
            }
            ListNode::Finger(right) => *finger = finger.concat(right),
            ListNode::Thunk(_) => {
                panic!("finger-tree conversion requires all lazy list chunks to be forced")
            }
        }
    }

    fn push_chunks_into_with<E>(
        &self,
        finger: &mut FingerList<V>,
        force_thunk: &mut impl FnMut(&T) -> Result<Self, E>,
    ) -> Result<(), E> {
        match self.0.as_ref() {
            ListNode::Empty => {}
            ListNode::Bytes(bytes) => {
                *finger = finger.push_right(ListChunk::Bytes(bytes.clone()));
            }
            ListNode::Values(values) => {
                *finger = finger.push_right(ListChunk::Values(values.clone()));
            }
            ListNode::Concat(left, right) => {
                left.push_chunks_into_with(finger, force_thunk)?;
                right.push_chunks_into_with(finger, force_thunk)?;
            }
            ListNode::Finger(right) => *finger = finger.concat(right),
            ListNode::Thunk(thunk) => {
                force_thunk(thunk)?.push_chunks_into_with(finger, force_thunk)?;
            }
        }
        Ok(())
    }

    #[cfg(test)]
    fn slice_checked(&self, start: usize, end: usize) -> Self {
        if start == end {
            return Self::empty();
        }
        match self.0.as_ref() {
            ListNode::Empty => Self::empty(),
            ListNode::Bytes(bytes) => Self::from_bytes(bytes.slice(start..end)),
            ListNode::Values(values) => Self::from_value_slice(values.slice(start, end)),
            ListNode::Concat(left, right) => {
                Self::slice_concat(left, left.len(), right, start, end)
            }
            ListNode::Finger(finger) => Self::slice_finger(finger, start, end),
            ListNode::Thunk(_) => {
                panic!("list slice requires all lazy list chunks to be forced")
            }
        }
    }

    #[cfg(test)]
    fn split_at_checked(&self, index: usize) -> (Self, Self) {
        match self.0.as_ref() {
            ListNode::Empty => {
                assert_eq!(index, 0);
                (Self::empty(), Self::empty())
            }
            ListNode::Bytes(bytes) => (
                Self::from_bytes(bytes.slice(0..index)),
                Self::from_bytes(bytes.slice(index..bytes.len())),
            ),
            ListNode::Values(values) => (
                Self::from_value_slice(values.slice(0, index)),
                Self::from_value_slice(values.slice(index, values.len())),
            ),
            ListNode::Concat(left, right) => {
                let left_len = left.len();
                if index < left_len {
                    let (left_left, left_right) = left.split_at_checked(index);
                    (left_left, Self::concat(left_right, right.clone()))
                } else if index == left_len {
                    (left.clone(), right.clone())
                } else {
                    let (right_left, right_right) = right.split_at_checked(index - left_len);
                    (Self::concat(left.clone(), right_left), right_right)
                }
            }
            ListNode::Finger(finger) => {
                let (left, right) = Self::split_finger_at(finger, index);
                (Self::from_finger(left), Self::from_finger(right))
            }
            ListNode::Thunk(_) => {
                panic!("list split requires all lazy list chunks to be forced")
            }
        }
    }

    #[cfg(test)]
    fn split_at_checked_with<E>(
        &self,
        index: usize,
        force_thunk: &mut impl FnMut(&T) -> Result<Self, E>,
    ) -> Result<Option<(Self, Self)>, E> {
        match self.0.as_ref() {
            ListNode::Empty => Ok((index == 0).then(|| (Self::empty(), Self::empty()))),
            ListNode::Bytes(bytes) => {
                if index > bytes.len() {
                    Ok(None)
                } else {
                    Ok(Some((
                        Self::from_bytes(bytes.slice(0..index)),
                        Self::from_bytes(bytes.slice(index..bytes.len())),
                    )))
                }
            }
            ListNode::Values(values) => {
                if index > values.len() {
                    Ok(None)
                } else {
                    Ok(Some((
                        Self::from_value_slice(values.slice(0, index)),
                        Self::from_value_slice(values.slice(index, values.len())),
                    )))
                }
            }
            ListNode::Concat(left, right) => {
                let left_len = left.try_len(force_thunk)?;
                if index < left_len {
                    let Some((left_left, left_right)) =
                        left.split_at_checked_with(index, force_thunk)?
                    else {
                        unreachable!("left branch should split below its length");
                    };
                    Ok(Some((left_left, Self::concat(left_right, right.clone()))))
                } else if index == left_len {
                    Ok(Some((left.clone(), right.clone())))
                } else {
                    let Some((right_left, right_right)) =
                        right.split_at_checked_with(index - left_len, force_thunk)?
                    else {
                        return Ok(None);
                    };
                    Ok(Some((Self::concat(left.clone(), right_left), right_right)))
                }
            }
            ListNode::Finger(finger) => {
                if index > finger.measure().0 {
                    Ok(None)
                } else {
                    let (left, right) = Self::split_finger_at(finger, index);
                    Ok(Some((Self::from_finger(left), Self::from_finger(right))))
                }
            }
            ListNode::Thunk(thunk) => force_thunk(thunk)?.split_at_checked_with(index, force_thunk),
        }
    }

    #[cfg(test)]
    fn split_from_end_checked(&self, count: usize) -> Option<(Self, Self)> {
        match self.0.as_ref() {
            ListNode::Concat(left, right) => {
                let right_len = right.len();
                if count < right_len {
                    let (right_left, right_right) = right.split_from_end_checked(count)?;
                    Some((Self::concat(left.clone(), right_left), right_right))
                } else if count == right_len {
                    Some((left.clone(), right.clone()))
                } else {
                    let (left_left, left_right) = left.split_from_end_checked(count - right_len)?;
                    Some((left_left, Self::concat(left_right, right.clone())))
                }
            }
            _ => {
                let len = self.len();
                (count <= len).then(|| self.split_at_checked(len - count))
            }
        }
    }

    #[cfg(test)]
    fn slice_finger(finger: &FingerList<V>, start: usize, end: usize) -> Self {
        let (_, tail) = Self::split_finger_at(finger, start);
        let (middle, _) = Self::split_finger_at(&tail, end - start);
        Self::from_finger(middle)
    }

    #[cfg(test)]
    fn split_finger_at(finger: &FingerList<V>, index: usize) -> (FingerList<V>, FingerList<V>) {
        let len = finger.measure().0;
        assert!(index <= len);
        if index == 0 {
            return (FingerList::new(), finger.clone());
        }
        if index == len {
            return (finger.clone(), FingerList::new());
        }
        let (mut left, right) = finger.split(|measure| measure.0 > index);
        let left_len = left.measure().0;
        if left_len == index {
            return (left, right);
        }
        let Some((chunk, tail)) = right.view_left() else {
            unreachable!("finger split inside a non-empty tree should leave a boundary chunk");
        };
        let chunk_offset = index - left_len;
        if let Some(chunk_left) = chunk.slice(0, chunk_offset) {
            left = left.push_right(chunk_left);
        }
        let mut right = tail;
        if let Some(chunk_right) = chunk.slice(chunk_offset, chunk.len()) {
            right = right.push_left(chunk_right);
        }
        (left, right)
    }

    #[cfg(test)]
    fn slice_concat(left: &Self, left_len: usize, right: &Self, start: usize, end: usize) -> Self {
        if end <= left_len {
            left.slice_checked(start, end)
        } else if start >= left_len {
            right.slice_checked(start - left_len, end - left_len)
        } else {
            Self::concat(
                left.slice_checked(start, left_len),
                right.slice_checked(0, end - left_len),
            )
        }
    }

    fn items_for_eq(&self) -> Vec<ListItem<V>>
    where
        V: Clone,
    {
        let items = std::cell::RefCell::new(Vec::new());
        self.for_each_segment(
            &mut |bytes| {
                items
                    .borrow_mut()
                    .extend(bytes.iter().copied().map(ListItem::Byte));
                Ok::<_, ()>(())
            },
            &mut |values| {
                items
                    .borrow_mut()
                    .extend(values.iter().cloned().map(ListItem::Value));
                Ok(())
            },
        )
        .expect("collecting known list items should not fail");
        items.into_inner()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestList = List<u32, &'static str>;

    struct NonClone(u32);

    struct NonCloneThunk;

    #[test]
    fn cloning_a_list_shell_does_not_require_cloneable_contents() {
        let list: List<NonClone, NonClone> =
            List(Arc::new(ListNode::Values(SharedSlice::from_vec(vec![
                NonClone(1),
            ]))));

        let duplicate = list.clone();

        assert!(Arc::ptr_eq(&list.0, &duplicate.0));
    }

    #[test]
    fn element_producing_operations_accept_explicit_nonclone_duplication() {
        let list = List::<NonClone, NonCloneThunk>::concat(
            List::from_values(vec![NonClone(1), NonClone(2)]),
            List::from_thunk(NonCloneThunk),
        );
        let mut duplicate = |value: &NonClone| value.0;
        let mut force = |_: &NonCloneThunk| {
            Ok::<_, ()>(List::<NonClone, NonCloneThunk>::from_values(vec![
                NonClone(3),
            ]))
        };

        assert_eq!(
            list.try_at_by(2, &mut duplicate, &mut force).unwrap(),
            Some(ListItem::Value(3))
        );
        let (head, tail) = list
            .try_pop_front_by(&mut duplicate, &mut force)
            .unwrap()
            .unwrap();
        assert_eq!(head, ListItem::Value(1));
        let (init, last) = tail
            .try_pop_back_by(&mut duplicate, &mut force)
            .unwrap()
            .unwrap();
        assert_eq!(last, ListItem::Value(3));
        assert_eq!(init.known_len(), Some(1));

        let balanced =
            List::<NonClone, NonCloneThunk>::from_values(vec![NonClone(4), NonClone(5)]).balanced();
        let (left, right) = balanced.try_split_at(1, &mut force).unwrap().unwrap();
        assert_eq!(left.known_len(), Some(1));
        assert_eq!(right.known_len(), Some(1));
    }

    #[test]
    fn root_map_defers_both_concat_children_without_visiting_them() {
        let source = TestList::concat(
            TestList::from_values(vec![1, 2]),
            TestList::from_bytes(Bytes::from_static(b"ab")),
        );
        let strict_visits = std::cell::Cell::new(0);
        let deferred_children = std::cell::Cell::new(0);
        let mapped = source.map_root_step(
            &mut |_| {
                strict_visits.set(strict_visits.get() + 1);
                0
            },
            &mut |_| {
                strict_visits.set(strict_visits.get() + 1);
                0
            },
            &mut |_| {
                deferred_children.set(deferred_children.get() + 1);
                "mapped child"
            },
            &mut |_| unreachable!("the source root is not a thunk"),
        );

        assert_eq!(strict_visits.get(), 0);
        assert_eq!(deferred_children.get(), 2);
        let stats = mapped.visit_logical_parts(&mut |_| {});
        assert_eq!(stats.node_visits, 3);
        assert_eq!(stats.thunk_items, 2);
        assert_eq!(stats.value_items, 0);
        assert_eq!(stats.byte_segments, 0);
    }

    #[test]
    fn root_flat_map_defers_both_concat_children_without_visiting_them() {
        let source = TestList::concat(
            TestList::from_values(vec![1, 2]),
            TestList::from_bytes(Bytes::from_static(b"ab")),
        );
        let strict_visits = std::cell::Cell::new(0);
        let deferred_children = std::cell::Cell::new(0);
        let flattened = source
            .flat_map_root_step(
                &mut |_| {
                    strict_visits.set(strict_visits.get() + 1);
                    Ok::<_, ()>(TestList::empty())
                },
                &mut |_| {
                    strict_visits.set(strict_visits.get() + 1);
                    Ok::<_, ()>(TestList::empty())
                },
                &mut |_| {
                    deferred_children.set(deferred_children.get() + 1);
                    "flattened child"
                },
                &mut |_| unreachable!("the source root is not a thunk"),
            )
            .unwrap();

        assert_eq!(strict_visits.get(), 0);
        assert_eq!(deferred_children.get(), 2);
        let stats = flattened.visit_logical_parts(&mut |_| {});
        assert_eq!(stats.node_visits, 3);
        assert_eq!(stats.thunk_items, 2);
        assert_eq!(stats.value_items, 0);
        assert_eq!(stats.byte_segments, 0);
    }

    #[test]
    fn root_flat_map_balances_a_strict_leaf_without_reordering_parts() {
        fn concat_depth<V, T>(list: &List<V, T>) -> usize {
            match list.0.as_ref() {
                ListNode::Concat(left, right) => 1 + concat_depth(left).max(concat_depth(right)),
                _ => 0,
            }
        }

        let item_count = 257_u32;
        let source = TestList::from_values((0..item_count).collect());
        let flattened = source
            .flat_map_root_step(
                &mut |_| unreachable!("the source leaf stores values"),
                &mut |value| Ok::<_, ()>(TestList::from_values(vec![*value])),
                &mut |_| unreachable!("the source root is not a concat"),
                &mut |_| unreachable!("the source root is not a thunk"),
            )
            .unwrap();

        assert_eq!(flattened.known_len(), Some(item_count as usize));
        assert!(
            concat_depth(&flattened) <= 9,
            "257 singleton parts need no more than ceil(log2(257)) concat levels"
        );
        let mut observed = Vec::new();
        flattened
            .for_each_segment(
                &mut |_| unreachable!("the result stores values"),
                &mut |values| {
                    observed.extend_from_slice(values);
                    Ok::<_, ()>(())
                },
            )
            .unwrap();
        assert_eq!(observed, (0..item_count).collect::<Vec<_>>());
    }

    #[test]
    fn byte_storage_stays_segmented_after_balancing() {
        let list = TestList::concat(
            TestList::from_bytes(Bytes::from_static(b"Hello")),
            TestList::from_values(vec![33]),
        )
        .balanced();
        let mut byte_lengths = Vec::new();
        let mut values = Vec::new();
        list.for_each_segment(
            &mut |bytes| {
                byte_lengths.push(bytes.len());
                Ok::<_, ()>(())
            },
            &mut |segment| {
                values.extend_from_slice(segment);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(byte_lengths, vec![5]);
        assert_eq!(values, vec![33]);
    }

    #[test]
    fn lazy_holes_are_forced_only_when_crossed() {
        let list = TestList::concat(TestList::from_values(vec![1]), TestList::from_thunk("tail"));
        let forces = std::cell::Cell::new(0);
        let mut force = |name: &&str| {
            forces.set(forces.get() + 1);
            assert_eq!(*name, "tail");
            Ok::<_, ()>(TestList::from_values(vec![2]))
        };
        let (first, tail) = list.try_pop_front(&mut force).unwrap().unwrap();
        assert_eq!(first, ListItem::Value(1));
        assert_eq!(forces.get(), 0);
        assert_eq!(
            tail.try_pop_front(&mut force).unwrap().unwrap().0,
            ListItem::Value(2)
        );
        assert_eq!(forces.get(), 1);
    }

    #[test]
    fn nonforcing_front_decomposition_retains_the_exact_deferred_suffix() {
        let list = TestList::concat(
            TestList::from_values(vec![1]),
            TestList::concat(
                TestList::from_thunk("middle"),
                TestList::from_bytes(Bytes::from_static(b"Z")),
            ),
        );
        let mut duplicate_value = |value: &u32| *value;
        let mut duplicate_deferred = |deferred: &&'static str| *deferred;

        let ListFrontStep::Item { item, tail } =
            list.pop_front_step_by(&mut duplicate_value, &mut duplicate_deferred)
        else {
            panic!("the strict prefix must be returned first")
        };
        assert_eq!(item, ListItem::Value(1));

        let ListFrontStep::Deferred { deferred, suffix } =
            tail.pop_front_step_by(&mut duplicate_value, &mut duplicate_deferred)
        else {
            panic!("the deferred segment must be reported without forcing")
        };
        assert_eq!(deferred, "middle");
        let ListFrontStep::Item { item, tail } =
            suffix.pop_front_step_by(&mut duplicate_value, &mut duplicate_deferred)
        else {
            panic!("the logical suffix must survive the deferred boundary")
        };
        assert_eq!(item, ListItem::Byte(b'Z'));
        assert!(matches!(
            tail.pop_front_step_by(&mut duplicate_value, &mut duplicate_deferred),
            ListFrontStep::Empty
        ));
    }

    #[test]
    fn nonforcing_front_decomposition_bounds_deep_concat_spines() {
        let mut list = TestList::from_values(vec![0]);
        for value in 1..20_000 {
            list = TestList::concat(list, TestList::from_values(vec![value]));
        }
        let mut duplicate_value = |value: &u32| *value;
        let mut duplicate_deferred = |deferred: &&'static str| *deferred;

        let ListFrontStep::Item { item, tail } =
            list.pop_front_step_by(&mut duplicate_value, &mut duplicate_deferred)
        else {
            panic!("a deep strict list must expose its first item")
        };
        assert_eq!(item, ListItem::Value(0));

        // This fixture deliberately leaks its pathological compatibility
        // spine: recursive `Arc` destruction is outside the front-walk
        // contract and will disappear with managed list spines.
        std::mem::forget(list);
        std::mem::forget(tail);
    }

    #[test]
    fn indexed_lookup_forces_only_lazy_holes_before_the_item() {
        let list = TestList::concat(
            TestList::from_values(vec![1]),
            TestList::concat(
                TestList::from_thunk("middle"),
                TestList::from_thunk("unused tail"),
            ),
        );
        let forced = std::cell::RefCell::new(Vec::new());
        let mut force = |name: &&str| {
            forced.borrow_mut().push((*name).to_owned());
            Ok::<_, ()>(match *name {
                "middle" => TestList::from_values(vec![2, 3]),
                "unused tail" => TestList::from_values(vec![4]),
                _ => unreachable!(),
            })
        };

        assert_eq!(
            list.try_at(2, &mut force).unwrap(),
            Some(ListItem::Value(3))
        );
        assert_eq!(*forced.borrow(), ["middle"]);
    }

    #[test]
    fn indexed_lookup_preserves_compact_byte_items_and_reports_bounds() {
        let list = TestList::concat(
            TestList::from_bytes(Bytes::from_static(b"AB")),
            TestList::from_values(vec![3]),
        )
        .balanced();
        let mut force =
            |_: &&str| -> Result<TestList, ()> { unreachable!("balanced list has no lazy holes") };

        assert_eq!(
            list.try_at(1, &mut force).unwrap(),
            Some(ListItem::Byte(b'B'))
        );
        assert_eq!(
            list.try_at(2, &mut force).unwrap(),
            Some(ListItem::Value(3))
        );
        assert_eq!(list.try_at(3, &mut force).unwrap(), None);
    }

    #[test]
    fn back_lookup_preserves_compact_and_finger_tree_segments() {
        let list = TestList::concat(
            TestList::from_bytes(Bytes::from_static(b"AB")),
            TestList::from_values(vec![3]),
        )
        .balanced();
        let mut force =
            |_: &&str| -> Result<TestList, ()> { unreachable!("balanced list has no lazy holes") };

        let (init, last) = list.try_pop_back(&mut force).unwrap().unwrap();
        assert_eq!(last, ListItem::Value(3));
        let (init, last) = init.try_pop_back(&mut force).unwrap().unwrap();
        assert_eq!(last, ListItem::Byte(b'B'));
        let (_, last) = init.try_pop_back(&mut force).unwrap().unwrap();
        assert_eq!(last, ListItem::Byte(b'A'));
    }

    #[test]
    fn back_lookup_does_not_force_an_unrelated_lazy_prefix() {
        let list = TestList::concat(
            TestList::from_thunk("unused prefix"),
            TestList::from_values(vec![1, 2]),
        );
        let mut force = |_: &&str| -> Result<TestList, ()> {
            panic!("finding a known suffix must not force its lazy prefix")
        };

        let (init, last) = list.try_pop_back(&mut force).unwrap().unwrap();
        assert_eq!(last, ListItem::Value(2));
        let (_, last) = init.try_pop_back(&mut force).unwrap().unwrap();
        assert_eq!(last, ListItem::Value(1));
    }

    #[test]
    fn back_lookup_forces_only_trailing_holes_needed_to_find_an_item() {
        let list = TestList::concat(
            TestList::from_thunk("unused prefix"),
            TestList::from_thunk("tail"),
        );
        let forced = std::cell::RefCell::new(Vec::new());
        let mut force = |name: &&str| {
            forced.borrow_mut().push((*name).to_owned());
            Ok::<_, ()>(match *name {
                "tail" => TestList::from_values(vec![2]),
                "unused prefix" => TestList::from_values(vec![1]),
                _ => unreachable!(),
            })
        };

        let (_, last) = list.try_pop_back(&mut force).unwrap().unwrap();
        assert_eq!(last, ListItem::Value(2));
        assert_eq!(*forced.borrow(), ["tail"]);
    }
}
