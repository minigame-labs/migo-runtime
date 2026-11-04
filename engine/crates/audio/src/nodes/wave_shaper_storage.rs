//! WaveShaper data keeps its actual capacity charge until physical destruction.
use super::FirState;
use crate::limits::AudioAggregatePermit;
use crate::node_retirement::{Host, Reservation, Value};
use shared::error::{EngineError, EngineResult, ErrorCode};
use std::ops::{Deref, DerefMut};

#[derive(Default)]
pub(super) struct FirHistory {
    pub(super) up_state: Vec<FirState>,
    pub(super) down_state: Vec<FirState>,
}
impl FirHistory {
    fn bytes(&self) -> usize {
        (self.up_state.capacity() + self.down_state.capacity()) * std::mem::size_of::<FirState>()
    }
    fn grown(&self, up: usize, down: usize) -> Self {
        let mut up_state = Vec::with_capacity(up);
        up_state.extend_from_slice(&self.up_state);
        let mut down_state = Vec::with_capacity(down);
        down_state.extend_from_slice(&self.down_state);
        Self {
            up_state,
            down_state,
        }
    }
}

pub(super) enum History {
    Native(FirHistory),
    Owned(Value<FirHistory>),
}
impl Default for History {
    fn default() -> Self {
        Self::Native(FirHistory::default())
    }
}
impl History {
    /// Reserve both adoption and growth before moving or allocating any data.
    pub(super) fn prepare(&mut self, host: &Host, channels: usize) -> EngineResult<()> {
        if let Self::Owned(value) = self
            && !value.accepts_physical_ledger(host)
        {
            return Err(foreign_ledger());
        }
        let up = self.up_state.capacity().max(channels);
        let down = self.down_state.capacity().max(channels);
        let grows = up != self.up_state.capacity() || down != self.down_state.capacity();
        let bytes = up
            .checked_add(down)
            .and_then(|count| count.checked_mul(std::mem::size_of::<FirState>()))
            .ok_or_else(crate::node_storage::size_overflow)?;
        let adoption = if matches!(self, Self::Native(_)) && self.bytes() != 0 {
            Some(host.reserve_value_with_bytes::<FirHistory>(self.bytes())?)
        } else {
            None
        };
        let growth = if grows {
            Some(host.reserve_value_with_bytes::<FirHistory>(bytes)?)
        } else {
            None
        };
        if let Some(reservation) = adoption {
            let data = std::mem::take(&mut **self);
            *self = Self::Owned(reservation.finish(data));
        }
        if let Some(reservation) = growth {
            let data = self.grown(up, down);
            debug_assert_eq!(data.bytes(), bytes);
            *self = Self::Owned(reservation.finish(data));
        }
        Ok(())
    }
    pub(super) fn is_owned(&self) -> bool {
        matches!(self, Self::Owned(_))
    }
    pub(super) fn bytes(&self) -> usize {
        (**self).bytes()
    }
    #[cfg(test)]
    pub(super) fn observe(&mut self, hook: std::sync::Arc<dyn Fn() + Send + Sync>) {
        if let Self::Owned(value) = self {
            value.observe(hook);
        }
    }
}
impl Deref for History {
    type Target = FirHistory;
    fn deref(&self) -> &FirHistory {
        match self {
            Self::Native(data) => data,
            Self::Owned(data) => data,
        }
    }
}
impl DerefMut for History {
    fn deref_mut(&mut self) -> &mut FirHistory {
        match self {
            Self::Native(data) => data,
            Self::Owned(data) => data,
        }
    }
}

pub(super) fn foreign_ledger() -> EngineError {
    EngineError::from_detail(
        ErrorCode::InvalidArgument,
        "waveshaper storage belongs to another physical ledger",
    )
}

pub(super) enum Curve {
    Native(Vec<f32>),
    Inline {
        data: Vec<f32>,
        // Fields drop in order: samples die before their physical credit.
        _permit: AudioAggregatePermit,
    },
    Cpu(Value<Vec<f32>>),
}
pub(super) enum CurveAdmission {
    Native,
    Inline(AudioAggregatePermit),
    Cpu(Reservation<Vec<f32>>),
}
impl CurveAdmission {
    pub(super) fn reserve(host: Option<&Host>, bytes: usize) -> EngineResult<Self> {
        match host {
            None => Ok(Self::Native),
            Some(host) if bytes < 4096 => Ok(Self::Inline(host.reserve_inline(bytes)?)),
            Some(host) => Ok(Self::Cpu(host.reserve_value_with_bytes::<Vec<f32>>(bytes)?)),
        }
    }
    pub(super) fn finish(self, data: Vec<f32>) -> Curve {
        match self {
            Self::Native => Curve::Native(data),
            Self::Inline(permit) => Curve::Inline {
                data,
                _permit: permit,
            },
            Self::Cpu(reservation) => Curve::Cpu(reservation.finish(data)),
        }
    }
}
impl Curve {
    pub(super) fn is_native(&self) -> bool {
        matches!(self, Self::Native(_))
    }
    pub(super) fn into_native(self) -> Vec<f32> {
        match self {
            Self::Native(data) => data,
            _ => unreachable!("only unowned curves need adoption"),
        }
    }
    pub(super) fn retire_replacement(self) {
        if let Self::Cpu(value) = self {
            value.retire_replacement();
        }
    }
    #[cfg(test)]
    pub(super) fn observe(&mut self, hook: std::sync::Arc<dyn Fn() + Send + Sync>) {
        if let Self::Cpu(value) = self {
            value.observe(hook);
        }
    }
}
impl Deref for Curve {
    type Target = Vec<f32>;
    fn deref(&self) -> &Vec<f32> {
        match self {
            Self::Native(data) | Self::Inline { data, .. } => data,
            Self::Cpu(data) => data,
        }
    }
}
