//! Accepted result cleanup used to outlive neither dequeue nor receiver exit.
//! The original result item/byte ticket bounds this drop-only CPU mailbox too.
use super::*;
use migo_io::audio_cpu::AudioCpu;
use tokio::sync::mpsc;
#[derive(Clone)]
pub(super) struct Retirement {
    send: mpsc::Sender<Box<dyn Send>>,
    pub(super) cpu: AudioCpu,
    #[cfg(test)]
    pub(super) hook: Option<Arc<dyn Fn() + Send + Sync>>,
    #[cfg(test)]
    pub(super) unadmitted_hook: Option<Arc<dyn Fn() + Send + Sync>>,
}
pub(super) struct Lease {
    _permit: DecodeQueuePermit,
    pub(super) physical: Option<AudioAggregatePermit>,
    pub(super) retired: Option<Retirement>,
    #[cfg(test)]
    pub(super) witness: Witness,
}
struct Retired<T> {
    _value: T,
    _lease: Lease,
}
pub(super) fn boxed<T: Send + 'static>(value: T, lease: Lease) -> Box<dyn Send> {
    Box::new(Retired {
        _value: value,
        _lease: lease,
    })
}
pub(super) struct Owned<T: Send + 'static> {
    value: Option<T>,
    lease: Option<Lease>,
    prepare: fn(T, Lease) -> Box<dyn Send>,
}
#[cfg(test)]
pub(super) fn entry<T: DecodeQueuePayload>(
    value: T,
    permit: DecodeQueuePermit,
    retired: Option<&Retirement>,
) -> Owned<T> {
    entry_charged(value, permit, retired, None)
}
pub(super) fn entry_charged<T: DecodeQueuePayload>(
    value: T,
    permit: DecodeQueuePermit,
    retired: Option<&Retirement>,
    physical: Option<AudioAggregatePermit>,
) -> Owned<T> {
    Owned {
        value: Some(value),
        lease: Some(Lease {
            _permit: permit,
            physical,
            retired: retired.cloned(),
            #[cfg(test)]
            witness: Witness(retired.and_then(|r| r.hook.clone())),
        }),
        prepare: T::retire_parts,
    }
}
impl<T: Send + 'static> Owned<T> {
    pub(super) fn value(&self) -> &T {
        self.value.as_ref().unwrap()
    }
    pub(super) fn into_parts(mut self) -> (T, Lease) {
        (self.value.take().unwrap(), self.lease.take().unwrap())
    }
    pub(super) fn try_transfer<U>(
        mut self,
        transfer: impl FnOnce(&mut T, &mut Option<AudioAggregatePermit>) -> EngineResult<U>,
    ) -> EngineResult<U> {
        match transfer(
            self.value.as_mut().unwrap(),
            &mut self.lease.as_mut().unwrap().physical,
        ) {
            Ok(value) => {
                drop(self.value.take());
                release(self.lease.take().unwrap());
                Ok(value)
            }
            Err(error) => Err(error),
        }
    }
    pub(super) fn into_value(mut self) -> T {
        let value = self.value.take().unwrap();
        release(self.lease.take().unwrap());
        value
    }
    pub(super) fn split<H, U: Send + 'static>(
        mut self,
        f: impl FnOnce(T) -> (H, U),
    ) -> (H, Owned<U>) {
        let (head, value) = f(self.value.take().unwrap());
        (
            head,
            Owned {
                value: Some(value),
                lease: self.lease.take(),
                prepare: boxed::<U>,
            },
        )
    }
    pub(super) fn map<U: Send + 'static>(self, f: impl FnOnce(T) -> U) -> Owned<U> {
        self.split(|value| ((), f(value))).1
    }
    pub(super) fn send(mut self, resp: tokio::sync::oneshot::Sender<T>) {
        let value = self.value.take().unwrap();
        match resp.send(value) {
            Ok(()) => release(self.lease.take().unwrap()),
            Err(value) => self.value = Some(value),
        }
    }
}
pub(super) fn release(lease: Lease) {
    #[cfg(test)]
    let lease = {
        let mut lease = lease;
        lease.witness.0 = None;
        lease
    };
    drop(lease);
}
impl<T: Send + 'static> Drop for Owned<T> {
    fn drop(&mut self) {
        let Some(value) = self.value.take() else {
            return;
        };
        let lease = self.lease.take().unwrap();
        let Some(retired) = lease.retired.as_ref() else {
            drop(value);
            drop(lease);
            return;
        };
        let send = retired.send.clone();
        if send.try_send((self.prepare)(value, lease)).is_err() {
            unreachable!("result retirement exceeded original result admission");
        }
    }
}
pub(super) fn channel<T: DecodeQueuePayload>(
    capacity: usize,
    bytes: usize,
    cpu: AudioCpu,
) -> (DecodeQueueSender<T>, DecodeQueueReceiver<T>) {
    let (mut send, receive) = decode_queue(capacity, bytes);
    let (retired, mut incoming) = mpsc::channel::<Box<dyn Send>>(capacity.max(1));
    let worker = cpu.clone();
    crate::streaming::get_stream_runtime().spawn(async move {
        while let Some(value) = incoming.recv().await {
            worker.retirement().retire(value).await;
        }
        worker.retirement().retire(incoming).await;
    });
    send.retirement = Some(Retirement {
        send: retired,
        cpu,
        #[cfg(test)]
        hook: None,
        #[cfg(test)]
        unadmitted_hook: None,
    });
    (send, receive)
}
pub(super) fn reject(entry: Owned<DecodeResult>, code: ErrorCode, detail: &'static str) {
    let error = EngineError::from_detail(code, detail);
    match entry.value() {
        DecodeResult::AudioBuffer { .. } => {
            let (resp, body) = entry.split(|r| match r {
                DecodeResult::AudioBuffer { resp, result, .. } => (resp, result),
                _ => unreachable!(),
            });
            drop(body);
            let _ = resp.send(Err(error));
        }
        DecodeResult::InnerAudio { .. } => {
            let (resp, body) = entry.split(|r| match r {
                DecodeResult::InnerAudio {
                    resp, result, load, ..
                } => (resp, (result, load)),
                _ => unreachable!(),
            });
            drop(body);
            let _ = resp.send(Err(error));
        }
    }
}
pub(super) fn integrate(
    entry: Owned<DecodeResult>,
    contexts: &impl ContextView,
    players: &mut impl PlayerAccess,
) {
    if matches!(entry.value(), DecodeResult::AudioBuffer { .. }) {
        let ((ctx_id, resp), body) = entry.split(|r| match r {
            DecodeResult::AudioBuffer {
                ctx_id,
                resp,
                result,
            } => ((ctx_id, resp), result),
            _ => unreachable!(),
        });
        integrate_buffer(contexts, ctx_id, body, resp);
    } else {
        super::inner_custody::integrate(entry, players);
    }
}
fn integrate_buffer(
    contexts: &impl ContextView,
    ctx_id: AudioContextId,
    body: Owned<EngineResult<DecodedAudio>>,
    resp: AudioResp<DecodedPcm>,
) {
    if resp.is_closed() {
        return;
    }
    let frames = if let Ok(decoded) = body.value() {
        let valid = if !contexts.contains_key(&ctx_id) {
            Err(EngineError::from_detail(
                ErrorCode::NotFound,
                format!("AudioContext {} closed during decode", ctx_id),
            ))
        } else {
            decoded_pcm_frames(decoded)
        };
        match valid {
            Ok(frames) => Some(frames),
            Err(error) => {
                drop(body);
                let _ = resp.send(Err(error));
                return;
            }
        }
    } else {
        None
    };
    if let Some(frames) = frames {
        let (result, lease) = body.into_parts();
        let pcm = super::response_custody::prepare(result.unwrap(), frames, lease);
        before_send();
        let _ = resp.send(Ok(pcm));
    } else {
        let answer = body.map(|result| {
            result.map(|_| unreachable!("successful PCM needs its checked frame count"))
        });
        before_send();
        answer.send(resp);
    }
}
pub(super) fn before_send() {
    #[cfg(test)]
    BEFORE_SEND.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}
#[cfg(test)]
#[derive(Default)]
pub(super) struct Witness(pub(super) Option<Arc<dyn Fn() + Send + Sync>>);
#[cfg(test)]
impl Drop for Witness {
    fn drop(&mut self) {
        if let Some(hook) = &self.0 {
            hook();
        }
    }
}
#[cfg(test)]
thread_local! {pub(super) static BEFORE_SEND:std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const {std::cell::RefCell::new(None)};}
