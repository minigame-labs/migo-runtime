//! A render allocation can outlive its node in the context's recycle pool.
use crate::limits::AudioAggregatePermit;
use crate::node_retirement::{Host, Reservation, Value};
use shared::error::{EngineError, EngineResult, ErrorCode};

pub(crate) enum RenderBuffer {
    Native(Vec<f32>),
    Inline {
        data: Vec<f32>,
        // Field order returns credit only after the actual allocation dies.
        _permit: AudioAggregatePermit,
    },
    Cpu(Value<Vec<f32>>),
}
enum Admission {
    Native,
    Inline(AudioAggregatePermit),
    Cpu(Reservation<Vec<f32>>),
}
impl Admission {
    fn reserve(host: Option<&Host>, bytes: usize) -> EngineResult<Self> {
        match host {
            None => Ok(Self::Native),
            Some(host) if bytes < 4096 => Ok(Self::Inline(host.reserve_inline(bytes)?)),
            Some(host) => Ok(Self::Cpu(host.reserve_value_with_bytes::<Vec<f32>>(bytes)?)),
        }
    }
    fn finish(self, data: Vec<f32>) -> RenderBuffer {
        match self {
            Self::Native => RenderBuffer::Native(data),
            Self::Inline(permit) => RenderBuffer::Inline {
                data,
                _permit: permit,
            },
            Self::Cpu(reservation) => RenderBuffer::Cpu(reservation.finish(data)),
        }
    }
}
pub(crate) struct Initial {
    samples: usize,
    admission: Admission,
}
impl Initial {
    pub(crate) fn reserve(host: &Host, samples: usize) -> EngineResult<Self> {
        Ok(Self {
            samples,
            admission: Admission::reserve(Some(host), capacity_bytes(samples)?)?,
        })
    }
    pub(crate) fn finish(self) -> RenderBuffer {
        let data = vec![0.0; self.samples];
        debug_assert_eq!(data.capacity(), self.samples);
        self.admission.finish(data)
    }
}
impl RenderBuffer {
    pub(crate) fn new(host: Option<&Host>, samples: usize) -> EngineResult<Self> {
        let bytes = capacity_bytes(samples)?;
        let reservation = Admission::reserve(host, bytes)?;
        let data = vec![0.0; samples];
        debug_assert_eq!(data.capacity(), samples);
        Ok(reservation.finish(data))
    }
    pub(crate) fn prepare(&mut self, host: &Host) -> EngineResult<()> {
        let accepts = match self {
            Self::Native(_) => true,
            Self::Inline {
                _permit: permit, ..
            } => host.accepts_inline(permit),
            Self::Cpu(value) => value.accepts_physical_ledger(host),
        };
        if !accepts {
            return Err(EngineError::from_detail(
                ErrorCode::InvalidArgument,
                "render buffer belongs to another physical ledger",
            ));
        }
        if let Self::Native(data) = self {
            let reservation = Admission::reserve(Some(host), capacity_bytes(data.capacity())?)?;
            *self = reservation.finish(std::mem::take(data));
        }
        Ok(())
    }
    fn data(&self) -> &Vec<f32> {
        match self {
            Self::Native(data) | Self::Inline { data, .. } => data,
            Self::Cpu(value) => value,
        }
    }
    fn data_mut(&mut self) -> &mut Vec<f32> {
        match self {
            Self::Native(data) | Self::Inline { data, .. } => data,
            Self::Cpu(value) => value,
        }
    }
    pub(crate) fn as_slice(&self) -> &[f32] {
        self.data().as_slice()
    }
    pub(crate) fn as_mut_slice(&mut self) -> &mut [f32] {
        self.data_mut().as_mut_slice()
    }
    pub(crate) fn len(&self) -> usize {
        self.data().len()
    }
    pub(crate) fn capacity(&self) -> usize {
        self.data().capacity()
    }
    pub(crate) fn clear(&mut self) {
        self.data_mut().clear();
    }
    pub(crate) fn resize(&mut self, samples: usize, value: f32) {
        // Production managed contexts submit existing bounded quanta. Native
        // standalone contexts retain their arbitrary-length Vec compatibility.
        assert!(
            matches!(self, Self::Native(_)) || samples <= self.capacity(),
            "managed render buffer exceeded admitted capacity"
        );
        self.data_mut().resize(samples, value);
    }
    #[cfg(test)]
    pub(crate) fn reserve_native(&mut self, samples: usize) {
        let Self::Native(data) = self else {
            panic!("only native buffers reserve outside admission")
        };
        data.reserve_exact(samples.saturating_sub(data.len()));
    }
    #[cfg(test)]
    pub(crate) fn observe(&mut self, hook: std::sync::Arc<dyn Fn() + Send + Sync>) {
        if let Self::Cpu(value) = self {
            value.observe(hook);
        }
    }
}
fn capacity_bytes(samples: usize) -> EngineResult<usize> {
    samples
        .checked_mul(std::mem::size_of::<f32>())
        .ok_or_else(crate::node_storage::size_overflow)
}
