//! Dequeue is not installation, and a full result queue owns no PCM ticket.
//! Keep the original queue or completion credit until refusal disposal on CPU.
use super::result_custody::{Owned, Retirement};
use super::*;

pub(super) fn integrate(
    entry: Owned<DecodeResult>,
    players: &mut HashMap<InnerAudioId, InnerAudioPlayer>,
) {
    let ((id, load, resp), body) = entry.split(|value| match value {
        DecodeResult::InnerAudio {
            id,
            load,
            result,
            resp,
        } => ((id, load, resp), result),
        _ => unreachable!(),
    });
    let response = match players.get_mut(&id) {
        Some(player) if player.shared.owns_load(&load) && !resp.is_closed() => {
            if body.value().is_err() {
                let Err(error) = body.into_value() else {
                    unreachable!()
                };
                player.discard_loading_controls();
                let _ = resp.send(Err(error));
                return;
            }
            let response = body.try_transfer(|result, physical| {
                let audio = result.as_mut().unwrap();
                let info = InnerAudioInfo {
                    duration: audio.duration(),
                    sample_rate: audio.sample_rate,
                    channels: audio.channels,
                };
                player
                    .try_complete_load_charged(&load, audio, physical)
                    .map(|()| info)
            });
            if response.is_err() {
                player.discard_loading_controls();
            }
            response
        }
        _ => {
            drop(body);
            Err(crate::inner_audio::InnerAudioLoadToken::cancelled_error())
        }
    };
    super::result_custody::before_send();
    let _ = resp.send(response);
}

struct Release<F: FnOnce()>(Option<F>);
impl<F: FnOnce()> Drop for Release<F> {
    fn drop(&mut self) {
        if let Some(release) = self.0.take() {
            release();
        }
    }
}
struct Unadmitted<T, F: FnOnce()> {
    _value: T,
    _release: Release<F>,
    #[cfg(test)]
    _witness: super::result_custody::Witness,
}
fn dispose<T: Send + 'static, F: FnOnce() + Send + 'static>(
    value: T,
    release: F,
    retired: &Retirement,
) {
    let value = Unadmitted {
        _value: value,
        _release: Release(Some(release)),
        #[cfg(test)]
        _witness: super::result_custody::Witness(retired.unadmitted_hook.clone()),
    };
    let cpu = retired.cpu.clone();
    crate::streaming::get_stream_runtime().spawn(async move {
        cpu.retirement().retire(value).await;
    });
}
pub(super) fn reject_unadmitted<F: FnOnce() + Send + 'static>(
    result: DecodeResult,
    code: ErrorCode,
    detail: &'static str,
    release: F,
    retired: Option<&Retirement>,
) {
    let Some(retired) = retired.filter(|_| result.queued_bytes() >= 4096) else {
        reject_decode_result_after(result, code, detail, release);
        return;
    };
    // Production successes still carry Complete's original in-flight and local-slot
    // permits here. Pending credit was already returned when execution began. No uncharged replacement queue is created.
    let error = EngineError::from_detail(code, detail);
    match result {
        DecodeResult::AudioBuffer { result, resp, .. } => {
            dispose(result, release, retired);
            let _ = resp.send(Err(error));
        }
        DecodeResult::InnerAudio {
            result, load, resp, ..
        } => {
            dispose((result, load), release, retired);
            let _ = resp.send(Err(error));
        }
    }
}
