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

#[derive(Debug, Clone)]
pub struct List<V: Clone, T: Clone>(Arc<ListNode<V, T>>);

#[derive(Debug, Clone)]
enum ListNode<V: Clone, T: Clone> {
    Empty,
    Bytes(Bytes),
    Values(SharedSlice<V>),
    Concat(List<V, T>, List<V, T>),
    Finger(FingerList<V>),
    Thunk(T),
}

type FingerList<V> = fingertrees::sync::FingerTree<ListChunk<V>>;

#[derive(Debug, Clone, PartialEq, Eq)]
enum ListChunk<V: Clone> {
    Bytes(Bytes),
    Values(SharedSlice<V>),
}

#[derive(Clone)]
struct SharedSlice<T> {
    data: Arc<[T]>,
    start: usize,
    len: usize,
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

impl<V: Clone> ListChunk<V> {
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

impl<V: Clone> Measured for ListChunk<V> {
    type Measure = Sum<usize>;

    fn measure(&self) -> Self::Measure {
        Sum(self.len())
    }
}

impl<V: Clone + PartialEq, T: Clone> PartialEq for List<V, T> {
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

impl<V: Clone + Eq, T: Clone> Eq for List<V, T> {}

impl<V: Clone, T: Clone> List<V, T> {
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

    pub fn from_thunk(thunk: T) -> Self {
        Self(Arc::new(ListNode::Thunk(thunk)))
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

    pub fn try_slice<E>(
        &self,
        start: usize,
        end: usize,
        force_thunk: &mut impl FnMut(&T) -> Result<Self, E>,
    ) -> Result<Option<Self>, E> {
        assert!(start <= end);
        let Some((_, tail)) = self.try_split_at(start, force_thunk)? else {
            return Ok(None);
        };
        let Some((middle, _)) = tail.try_split_at(end - start, force_thunk)? else {
            return Ok(None);
        };
        Ok(Some(middle))
    }

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

    pub fn try_split_from_end<E>(
        &self,
        count: usize,
        force_thunk: &mut impl FnMut(&T) -> Result<Self, E>,
    ) -> Result<Option<(Self, Self)>, E> {
        self.split_from_end_checked_with(count, force_thunk)
    }

    pub fn try_pop_front<E>(
        &self,
        force_thunk: &mut impl FnMut(&T) -> Result<Self, E>,
    ) -> Result<Option<(ListItem<V>, Self)>, E> {
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
                    ListItem::Value(first.clone()),
                    Self::from_value_slice(values.slice(1, values.len())),
                )))
            }
            ListNode::Concat(left, right) => {
                if let Some((first, left_tail)) = left.try_pop_front(force_thunk)? {
                    Ok(Some((first, Self::concat(left_tail, right.clone()))))
                } else {
                    right.try_pop_front(force_thunk)
                }
            }
            ListNode::Finger(finger) => {
                if finger.measure().0 == 0 {
                    Ok(None)
                } else {
                    let (first, rest) = Self::split_finger_at(finger, 1);
                    let Some((value, _)) = Self::from_finger(first).try_pop_front(force_thunk)?
                    else {
                        unreachable!("non-empty one-item finger tree should have a front value");
                    };
                    Ok(Some((value, Self::from_finger(rest))))
                }
            }
            ListNode::Thunk(thunk) => force_thunk(thunk)?.try_pop_front(force_thunk),
        }
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

    fn split_from_end_checked_with<E>(
        &self,
        count: usize,
        force_thunk: &mut impl FnMut(&T) -> Result<Self, E>,
    ) -> Result<Option<(Self, Self)>, E> {
        match self.0.as_ref() {
            ListNode::Concat(left, right) => {
                let right_len = right.try_len(force_thunk)?;
                if count < right_len {
                    let Some((right_left, right_right)) =
                        right.split_from_end_checked_with(count, force_thunk)?
                    else {
                        unreachable!("right branch should split below its length");
                    };
                    Ok(Some((Self::concat(left.clone(), right_left), right_right)))
                } else if count == right_len {
                    Ok(Some((left.clone(), right.clone())))
                } else {
                    let Some((left_left, left_right)) =
                        left.split_from_end_checked_with(count - right_len, force_thunk)?
                    else {
                        return Ok(None);
                    };
                    Ok(Some((left_left, Self::concat(left_right, right.clone()))))
                }
            }
            ListNode::Thunk(thunk) => {
                force_thunk(thunk)?.split_from_end_checked_with(count, force_thunk)
            }
            _ => {
                let len = self.try_len(force_thunk)?;
                if count > len {
                    Ok(None)
                } else {
                    self.split_at_checked_with(len - count, force_thunk)
                }
            }
        }
    }

    #[cfg(test)]
    fn slice_finger(finger: &FingerList<V>, start: usize, end: usize) -> Self {
        let (_, tail) = Self::split_finger_at(finger, start);
        let (middle, _) = Self::split_finger_at(&tail, end - start);
        Self::from_finger(middle)
    }

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

    fn items_for_eq(&self) -> Vec<ListItem<V>> {
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
}
