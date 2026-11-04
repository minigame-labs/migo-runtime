//! Timeline capacity is admitted before growth and follows its physical owner.
use super::AutomationEvent;
use crate::limits::AudioAggregatePermit;
use crate::node_retirement::{Host, Reservation, Value};
use shared::error::{EngineError, EngineResult, ErrorCode};
use std::ops::{Deref, DerefMut};

type Data = Vec<AutomationEvent>;

pub(super) enum Events {
    Native(Data),
    Inline {
        data: Data,
        // Free the actual allocation before returning its physical credit.
        _permit: AudioAggregatePermit,
    },
    Cpu(Value<Data>),
}
impl Default for Events {
    fn default() -> Self {
        Self::Native(Vec::new())
    }
}
enum Admission {
    Inline(AudioAggregatePermit),
    Cpu(Reservation<Data>),
}
impl Admission {
    fn reserve(host: &Host, bytes: usize) -> EngineResult<Self> {
        if bytes < 4096 {
            Ok(Self::Inline(host.reserve_inline(bytes)?))
        } else {
            Ok(Self::Cpu(host.reserve_value_with_bytes::<Data>(bytes)?))
        }
    }
    fn finish(self, data: Data) -> Events {
        match self {
            Self::Inline(permit) => Events::Inline {
                data,
                _permit: permit,
            },
            Self::Cpu(reservation) => Events::Cpu(reservation.finish(data)),
        }
    }
}
impl Events {
    pub(super) fn native(data: Data) -> Self {
        Self::Native(data)
    }
    pub(super) fn prepare(&mut self, host: &Host) -> EngineResult<()> {
        if matches!(self, Self::Native(_)) && self.capacity() != 0 {
            let reservation = Admission::reserve(host, capacity_bytes(self.capacity())?)?;
            let data = std::mem::take(&mut **self);
            *self = reservation.finish(data);
        }
        Ok(())
    }
    pub(super) fn reserve_for(&mut self, host: Option<&Host>, required: usize) -> EngineResult<()> {
        if required <= self.capacity() {
            return Ok(());
        }
        let Some(host) = host else {
            // Preserve the original native Vec growth strategy at insert().
            return Ok(());
        };
        let capacity = self
            .capacity()
            .checked_mul(2)
            .ok_or_else(crate::node_storage::size_overflow)?
            .max(required)
            .max(4);
        let reservation = Admission::reserve(host, capacity_bytes(capacity)?)?;
        let mut data = Vec::with_capacity(capacity);
        data.extend_from_slice(self);
        debug_assert_eq!(data.capacity(), capacity);
        let previous = std::mem::replace(self, reservation.finish(data));
        if let Self::Cpu(value) = previous {
            value.retire_replacement();
        }
        Ok(())
    }
    #[cfg(test)]
    pub(super) fn observe(&mut self, hook: std::sync::Arc<dyn Fn() + Send + Sync>) {
        if let Self::Cpu(value) = self {
            value.observe(hook);
        }
    }
}
fn capacity_bytes(capacity: usize) -> EngineResult<usize> {
    capacity
        .checked_mul(std::mem::size_of::<AutomationEvent>())
        .ok_or_else(crate::node_storage::size_overflow)
}
pub(super) fn foreign_ledger() -> EngineError {
    EngineError::from_detail(
        ErrorCode::InvalidArgument,
        "AudioParam storage belongs to another physical ledger",
    )
}
impl Deref for Events {
    type Target = Data;
    fn deref(&self) -> &Data {
        match self {
            Self::Native(data) | Self::Inline { data, .. } => data,
            Self::Cpu(value) => value,
        }
    }
}
impl DerefMut for Events {
    fn deref_mut(&mut self) -> &mut Data {
        match self {
            Self::Native(data) | Self::Inline { data, .. } => data,
            Self::Cpu(value) => value,
        }
    }
}
