//! Admit analyser output before the original getter allocates or advances FFT history.
use super::*;
use crate::node_retirement::{HANDOFF_BYTES, Host, Reservation, Value};
use shared::audio_analyser_samples::{AnalyserSamples, AnalyserStorage};

pub(super) fn bytes(
    contexts: &mut HashMap<AudioContextId, AudioContext>,
    index: &NodeContextIndex,
    node_id: AudioNodeId,
    frequency: bool,
) -> EngineResult<AnalyserSamples<u8>> {
    read(contexts, index, node_id, frequency, |node| {
        if frequency {
            node.get_byte_frequency_data()
        } else {
            node.get_byte_time_domain_data()
        }
    })
}
pub(super) fn floats(
    contexts: &mut HashMap<AudioContextId, AudioContext>,
    index: &NodeContextIndex,
    node_id: AudioNodeId,
    frequency: bool,
) -> EngineResult<AnalyserSamples<f32>> {
    read(contexts, index, node_id, frequency, |node| {
        if frequency {
            node.get_float_frequency_data()
        } else {
            node.get_float_time_domain_data()
        }
    })
}
fn read<T: Send + 'static>(
    contexts: &mut HashMap<AudioContextId, AudioContext>,
    index: &NodeContextIndex,
    node_id: AudioNodeId,
    frequency: bool,
    getter: impl FnOnce(&mut AnalyserNode) -> Vec<T>,
) -> EngineResult<AnalyserSamples<T>> {
    let missing = || {
        EngineError::from_detail(
            ErrorCode::NotFound,
            format!("AnalyserNode {} not found", node_id),
        )
    };
    let ctx = index
        .get_context(node_id)
        .and_then(|id| contexts.get_mut(&id))
        .ok_or_else(missing)?;
    let count = ctx
        .with_node_typed::<AnalyserNode, _>(node_id, |node| {
            node.fft_size() / if frequency { 2 } else { 1 }
        })
        .ok_or_else(missing)?;
    let bytes = count * std::mem::size_of::<T>();
    let admission = ctx
        .response_retirement_host()
        .map(|host| Admission::new(host, bytes))
        .transpose()?;
    let data = ctx
        .with_node_typed::<AnalyserNode, _>(node_id, getter)
        .expect("checked analyser disappeared");
    debug_assert_eq!(data.capacity() * std::mem::size_of::<T>(), bytes);
    Ok(match admission {
        Some(Admission::Cpu(reservation)) => AnalyserSamples::new(reservation.finish(data)),
        Some(Admission::Inline(permit)) => AnalyserSamples::new(Small {
            data,
            _permit: permit,
        }),
        None => data.into(),
    })
}
enum Admission<T: Send + 'static> {
    Cpu(Reservation<Vec<T>>),
    Inline(AudioAggregatePermit),
}
impl<T: Send + 'static> Admission<T> {
    fn new(host: &Host, bytes: usize) -> EngineResult<Self> {
        if bytes >= 4096 {
            Ok(Self::Cpu(host.reserve_value_with_bytes::<Vec<T>>(bytes)?))
        } else {
            Ok(Self::Inline(host.reserve_inline(
                bytes + std::mem::size_of::<Small<T>>() + HANDOFF_BYTES,
            )?))
        }
    }
}
impl<T: Send + 'static> AnalyserStorage<T> for Value<Vec<T>> {
    fn samples(&self) -> &[T] {
        self
    }
    fn samples_mut(&mut self) -> &mut [T] {
        self
    }
}
struct Small<T> {
    data: Vec<T>,
    _permit: AudioAggregatePermit,
}
impl<T: Send> AnalyserStorage<T> for Small<T> {
    fn samples(&self) -> &[T] {
        &self.data
    }
    fn samples_mut(&mut self) -> &mut [T] {
        &mut self.data
    }
}

#[cfg(test)]
#[path = "audio_thread_analyser_output_tests.rs"]
mod tests;
