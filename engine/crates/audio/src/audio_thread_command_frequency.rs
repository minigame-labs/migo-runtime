//! A filter query borrows its frequency Vec. Keep the original queue ticket
//! until large admitted inputs are destroyed on the existing AudioCpu lane.
use super::*;

pub(super) fn forward(
    delivery: AudioCommandDelivery,
    contexts: &mut impl ContextAccess,
    index: &NodeContextIndex,
    inbox: &mut crate::command_inbox::Inbox,
) -> Option<AudioCommandDelivery> {
    let AudioCmd::GetFrequencyResponse {
        node_id,
        frequencies,
        ..
    } = delivery.command()
    else {
        return Some(delivery);
    };
    if !delivery.has_queue_permit()
        || frequencies
            .capacity()
            .saturating_mul(std::mem::size_of::<f32>())
            < 4096
    {
        return Some(delivery);
    }
    let result = query(contexts, index, *node_id, frequencies);
    inbox.finish_frequency(delivery, result);
    None
}

type Data = (Vec<f32>, Vec<f32>);
use crate::limits::AudioAggregatePermit;
use crate::node_retirement::{HANDOFF_BYTES, Host, Reservation, Value};
use shared::audio_frequency_response::{FrequencyResponse, FrequencyResponseStorage};

pub(super) fn query(
    contexts: &mut impl ContextAccess,
    index: &NodeContextIndex,
    node_id: AudioNodeId,
    frequencies: &[f32],
) -> EngineResult<FrequencyResponse> {
    let missing = || {
        EngineError::from_detail(
            ErrorCode::NotFound,
            format!("Filter node {} not found", node_id),
        )
    };
    let ctx = index
        .get_context(node_id)
        .and_then(|id| contexts.get_mut(&id))
        .ok_or_else(missing)?;
    if !ctx.node_is::<BiquadFilterNode>(node_id) && !ctx.node_is::<IIRFilterNode>(node_id) {
        return Err(missing());
    }
    let limit = migo_services::audio::MAX_FREQUENCY_RESPONSE_POINTS;
    if frequencies.len() > limit {
        return Err(EngineError::from_detail(
            ErrorCode::InputSaturated,
            format!(
                "frequency response has {} points; limit is {}",
                frequencies.len(),
                limit
            ),
        ));
    }
    // Each mature getter allocates exactly two Vec::with_capacity(len) arrays.
    // The existing public point limit bounds this multiplication on every ABI.
    let bytes = frequencies.len() * 2 * std::mem::size_of::<f32>();
    let admission = ctx
        .response_retirement_host()
        .map(|host| Admission::new(host, bytes))
        .transpose()?;
    let data = compute(ctx, node_id, frequencies).expect("checked filter node disappeared");
    Ok(match admission {
        Some(admission) => admission.finish(data),
        // Standalone graphs retain their original synchronous ownership.
        None => FrequencyResponse::new(data),
    })
}

fn compute(ctx: &mut AudioContext, node_id: AudioNodeId, frequencies: &[f32]) -> Option<Data> {
    let sr = ctx.sample_rate() as f64;
    // Try BiquadFilterNode first, preserving its coefficient update.
    if let Some(r) = ctx.with_node_typed::<BiquadFilterNode, _>(node_id, |n| {
        n.get_frequency_response(sr, frequencies)
    }) {
        return Some(r);
    }
    ctx.with_node_typed::<IIRFilterNode, _>(node_id, |n| n.get_frequency_response(sr, frequencies))
}

enum Admission {
    Cpu(Reservation<Data>),
    Inline(AudioAggregatePermit),
}
impl Admission {
    fn new(host: &Host, bytes: usize) -> EngineResult<Self> {
        if bytes >= 4096 {
            Ok(Self::Cpu(host.reserve_value_with_bytes::<Data>(bytes)?))
        } else {
            Ok(Self::Inline(host.reserve_inline(
                bytes + std::mem::size_of::<Small>() + HANDOFF_BYTES,
            )?))
        }
    }
    fn finish(self, data: Data) -> FrequencyResponse {
        match self {
            Self::Cpu(reservation) => {
                let value = reservation.finish(data);
                #[cfg(test)]
                let value = {
                    let mut value = value;
                    OUTPUT_WITNESS.with(|hook| {
                        if let Some(hook) = hook.borrow().as_ref() {
                            value.observe(hook.clone());
                        }
                    });
                    value
                };
                FrequencyResponse::new(value)
            }
            Self::Inline(permit) => FrequencyResponse::new(Small {
                data,
                _permit: permit,
                #[cfg(test)]
                _witness: Witness(OUTPUT_WITNESS.with(|hook| hook.borrow().clone())),
            }),
        }
    }
}
impl FrequencyResponseStorage for Value<Data> {
    fn values(&self) -> (&[f32], &[f32]) {
        let data: &Data = self;
        (&data.0, &data.1)
    }
}
struct Small {
    data: Data,
    _permit: AudioAggregatePermit,
    #[cfg(test)]
    _witness: Witness,
}
impl FrequencyResponseStorage for Small {
    fn values(&self) -> (&[f32], &[f32]) {
        (&self.data.0, &self.data.1)
    }
}
#[cfg(test)]
struct Witness(Option<Arc<dyn Fn() + Send + Sync>>);
#[cfg(test)]
impl Drop for Witness {
    fn drop(&mut self) {
        if let Some(hook) = &self.0 {
            hook();
        }
    }
}
#[cfg(test)]
thread_local! {
    static OUTPUT_WITNESS: std::cell::RefCell<Option<Arc<dyn Fn() + Send + Sync>>> = const { std::cell::RefCell::new(None) };
}
#[cfg(test)]
fn observe_output<R>(hook: Arc<dyn Fn() + Send + Sync>, f: impl FnOnce() -> R) -> R {
    struct Reset(Option<Arc<dyn Fn() + Send + Sync>>);
    impl Drop for Reset {
        fn drop(&mut self) {
            OUTPUT_WITNESS.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }
    let _reset = Reset(OUTPUT_WITNESS.with(|slot| slot.replace(Some(hook))));
    f()
}

#[cfg(test)]
#[path = "audio_thread_command_frequency_tests.rs"]
pub(super) mod tests;

#[cfg(test)]
#[path = "audio_thread_frequency_output_tests.rs"]
mod output_tests;
