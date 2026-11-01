//! A successful oneshot send must not detach PCM from its transport ticket.
//! One pre-registered drop slot follows it through registry adoption and final
//! source-node Arc release, including after the originating Host closes.
#[cfg(test)]
use super::result_custody::Witness;
use super::result_custody::{Lease, release};
use super::*;
use shared::audio_decoded_pcm::{DecodedPcmCredit, DecodedPcmCustody};
use tokio::sync::oneshot;

struct Owner {
    send: Option<oneshot::Sender<Retired>>,
    lease: Option<Lease>,
    physical: Option<AudioAggregatePermit>,
    #[cfg(test)]
    witness: Witness,
}
struct Retired {
    _samples: Vec<f32>,
    _credit: Option<DecodedPcmCredit>,
    _lease: Option<Lease>,
    _physical: Option<AudioAggregatePermit>,
    #[cfg(test)]
    _witness: Witness,
}
pub(super) fn prepare(audio: DecodedAudio, frames: u32, mut lease: Lease) -> DecodedPcm {
    let Some(retired) = lease.retired.as_ref() else {
        release(lease);
        return DecodedPcm::new(audio.sample_rate, audio.channels, frames, audio.samples);
    };
    let cpu = retired.cpu.clone();
    let (send, receive) = oneshot::channel::<Retired>();
    crate::streaming::get_stream_runtime().spawn(async move {
        if let Ok(value) = receive.await {
            cpu.retirement().retire(value).await;
        }
    });
    let physical = lease.physical.take();
    let owner = Owner {
        physical,
        send: Some(send),
        #[cfg(test)]
        witness: std::mem::take(&mut lease.witness),
        lease: Some(lease),
    };
    DecodedPcm::with_custody(
        audio.sample_rate,
        audio.channels,
        frames,
        audio.samples,
        Box::new(owner),
    )
}
impl DecodedPcmCustody for Owner {
    fn has_physical_credit(&self, ledger: &Arc<AudioAggregateLedger>) -> bool {
        self.physical
            .as_ref()
            .is_some_and(|credit| credit.belongs_to(ledger))
    }
    fn adopted(&mut self) {
        if let Some(lease) = self.lease.take() {
            release(lease);
        }
    }
    fn dispose(mut self: Box<Self>, samples: Vec<f32>, credit: Option<DecodedPcmCredit>) {
        let value = Retired {
            _samples: samples,
            _credit: credit,
            _lease: self.lease.take(),
            _physical: self.physical.take(),
            #[cfg(test)]
            _witness: std::mem::take(&mut self.witness),
        };
        if self.send.take().unwrap().send(value).is_err() {
            unreachable!("process-lifetime decoded response disposal receiver disappeared");
        }
    }
}
