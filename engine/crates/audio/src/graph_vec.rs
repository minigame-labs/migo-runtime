//! Command-grown graph vectors keep physical credit through actual destruction.
use crate::limits::AudioAggregatePermit;
use crate::node_retirement::{Host, Reservation, Value};
use shared::error::{EngineError, EngineResult, ErrorCode};
use std::ops::{Deref, DerefMut, RangeBounds};

pub(crate) struct GraphVec<T: Send + 'static> {
    // Inline owners drop after data; CPU owners take this Vec without allocating.
    data: Vec<T>,
    owner: Owner<T>,
}
enum Owner<T: Send + 'static> {
    Native,
    Inline(AudioAggregatePermit),
    Cpu(Value<Option<Vec<T>>>),
}
pub(crate) enum Admission<T: Send + 'static> {
    Inline(AudioAggregatePermit),
    Cpu(Reservation<Option<Vec<T>>>),
}
impl<T: Send + 'static> Admission<T> {
    fn reserve(host: &Host, capacity: usize) -> EngineResult<Self> {
        let bytes = capacity
            .checked_mul(std::mem::size_of::<T>())
            .ok_or_else(crate::node_storage::size_overflow)?;
        if bytes < 4096 {
            Ok(Self::Inline(host.reserve_inline(bytes)?))
        } else {
            Ok(Self::Cpu(
                host.reserve_value_with_bytes::<Option<Vec<T>>>(bytes)?,
            ))
        }
    }
    fn finish(self) -> Owner<T> {
        match self {
            Self::Inline(permit) => Owner::Inline(permit),
            Self::Cpu(reservation) => Owner::Cpu(reservation.finish(None)),
        }
    }
}
/// An initial allocation is admitted before a constructor allocates any data.
pub(crate) struct Initial<T: Send + 'static> {
    capacity: usize,
    admission: Admission<T>,
}
impl<T: Send + 'static> Initial<T> {
    pub(crate) fn reserve(host: &Host, capacity: usize) -> EngineResult<Self> {
        Ok(Self {
            capacity,
            admission: Admission::reserve(host, capacity)?,
        })
    }
    pub(crate) fn finish(self) -> GraphVec<T> {
        let data = Vec::with_capacity(self.capacity);
        debug_assert_eq!(data.capacity(), self.capacity);
        GraphVec {
            data,
            owner: self.admission.finish(),
        }
    }
}
/// Tickets only: reserving a batch never allocates or changes its data vectors.
pub(crate) enum Plan<T: Send + 'static> {
    Native {
        required: usize,
    },
    Managed {
        capacity: usize,
        admission: Admission<T>,
    },
}
impl<T: Send + 'static> Plan<T> {
    pub(crate) fn reserve(
        data: &GraphVec<T>,
        host: Option<&Host>,
        required: usize,
    ) -> EngineResult<Option<Self>> {
        if required <= data.capacity() {
            return Ok(None);
        }
        let Some(host) = host else {
            return Ok(Some(Self::Native { required }));
        };
        let capacity = data
            .capacity()
            .checked_mul(2)
            .ok_or_else(crate::node_storage::size_overflow)?
            .max(required)
            .max(4);
        let admission = Admission::reserve(host, capacity)?;
        Ok(Some(Self::Managed {
            capacity,
            admission,
        }))
    }
    pub(crate) fn install(self, target: &mut GraphVec<T>)
    where
        T: Clone,
    {
        match self {
            Self::Native { required } => target.data.reserve(required.saturating_sub(target.len())),
            Self::Managed {
                capacity,
                admission,
            } => {
                let mut data = Vec::with_capacity(capacity);
                data.extend_from_slice(&target.data);
                debug_assert_eq!(data.capacity(), capacity);
                let replacement = GraphVec {
                    data,
                    owner: admission.finish(),
                };
                *target = replacement;
            }
        }
    }
}
impl<T: Send + 'static> GraphVec<T> {
    pub(crate) fn from_vec(data: Vec<T>) -> Self {
        Self {
            data,
            owner: Owner::Native,
        }
    }
    pub(crate) fn new() -> Self {
        Self::default()
    }
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self::from_vec(Vec::with_capacity(capacity))
    }
    pub(crate) fn prepare(&mut self, host: &Host) -> EngineResult<()> {
        let accepts = match &self.owner {
            Owner::Native => true,
            Owner::Inline(permit) => host.accepts_inline(permit),
            Owner::Cpu(value) => value.accepts_physical_ledger(host),
        };
        if !accepts {
            return Err(EngineError::from_detail(
                ErrorCode::InvalidArgument,
                "graph vector belongs to another physical ledger",
            ));
        }
        if matches!(self.owner, Owner::Native) && self.capacity() != 0 {
            let admission = Admission::reserve(host, self.capacity())?;
            self.owner = admission.finish();
        }
        Ok(())
    }
    pub(crate) fn capacity(&self) -> usize {
        self.data.capacity()
    }
    pub(crate) fn as_slice(&self) -> &[T] {
        &self.data
    }
    pub(crate) fn as_mut_slice(&mut self) -> &mut [T] {
        &mut self.data
    }
    fn check_capacity(&self, required: usize) {
        assert!(
            matches!(self.owner, Owner::Native) || required <= self.capacity(),
            "managed graph vector exceeded admitted capacity"
        );
    }
    pub(crate) fn clear(&mut self) {
        self.data.clear();
    }
    pub(crate) fn push(&mut self, value: T) {
        self.check_capacity(
            self.len()
                .checked_add(1)
                .expect("graph vector length overflow"),
        );
        self.data.push(value);
    }
    pub(crate) fn pop(&mut self) -> Option<T> {
        self.data.pop()
    }
    pub(crate) fn resize(&mut self, len: usize, value: T)
    where
        T: Clone,
    {
        self.check_capacity(len);
        self.data.resize(len, value);
    }
    pub(crate) fn extend_from_slice(&mut self, values: &[T])
    where
        T: Clone,
    {
        self.check_capacity(
            self.len()
                .checked_add(values.len())
                .expect("graph vector length overflow"),
        );
        self.data.extend_from_slice(values);
    }
    pub(crate) fn retain(&mut self, f: impl FnMut(&T) -> bool) {
        self.data.retain(f);
    }
    pub(crate) fn drain<R: RangeBounds<usize>>(&mut self, range: R) -> std::vec::Drain<'_, T> {
        self.data.drain(range)
    }
    #[cfg(test)]
    pub(crate) fn set_native_capacity(&mut self, capacity: usize) {
        assert!(matches!(self.owner, Owner::Native));
        let mut data = Vec::with_capacity(capacity.max(self.len()));
        data.append(&mut self.data);
        self.data = data;
    }
    #[cfg(test)]
    pub(crate) fn observe(&mut self, hook: std::sync::Arc<dyn Fn() + Send + Sync>) {
        if let Owner::Cpu(value) = &mut self.owner {
            value.observe(hook);
        }
    }
}
impl<T: Send + 'static> Default for GraphVec<T> {
    fn default() -> Self {
        Self::from_vec(Vec::new())
    }
}
impl<T: Send + 'static> Drop for GraphVec<T> {
    fn drop(&mut self) {
        if let Owner::Cpu(value) = &mut self.owner {
            **value = Some(std::mem::take(&mut self.data));
        }
    }
}
impl<T: Send + 'static> Deref for GraphVec<T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        self.as_slice()
    }
}
impl<T: Send + 'static> DerefMut for GraphVec<T> {
    fn deref_mut(&mut self) -> &mut [T] {
        self.as_mut_slice()
    }
}
impl<'a, T: Send + 'static> IntoIterator for &'a GraphVec<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.data.iter()
    }
}
impl<T: Send + std::fmt::Debug + 'static> std::fmt::Debug for GraphVec<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.data.fmt(f)
    }
}
