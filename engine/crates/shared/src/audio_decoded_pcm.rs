//! Decoded PCM crosses a response before a registry can adopt it.
//! Its custody travels with the samples; admitted registry credit replaces
//! transport credit before the old ticket can be returned.
use crate::audio_physical::AudioAggregateLedger;
use std::fmt;
use std::sync::Arc;

pub type DecodedPcmCredit = Box<dyn Send + Sync>;

pub trait DecodedPcmCustody: Send + Sync {
    fn has_physical_credit(&self, _ledger: &Arc<AudioAggregateLedger>) -> bool {
        false
    }
    fn adopted(&mut self);
    fn dispose(self: Box<Self>, samples: Vec<f32>, credit: Option<DecodedPcmCredit>);
}

pub struct DecodedPcm {
    pub sample_rate: u32,
    pub channels: u32,
    pub frames: u32,
    pub samples: Vec<f32>,
    credit: Option<DecodedPcmCredit>,
    custody: Option<Box<dyn DecodedPcmCustody>>,
}
impl DecodedPcm {
    pub fn new(sample_rate: u32, channels: u32, frames: u32, samples: Vec<f32>) -> Self {
        Self {
            sample_rate,
            channels,
            frames,
            samples,
            credit: None,
            custody: None,
        }
    }
    pub fn with_custody(
        sample_rate: u32,
        channels: u32,
        frames: u32,
        samples: Vec<f32>,
        custody: Box<dyn DecodedPcmCustody>,
    ) -> Self {
        let mut pcm = Self::new(sample_rate, channels, frames, samples);
        pcm.custody = Some(custody);
        pcm
    }
    pub(crate) fn has_physical_credit(&self, ledger: &Arc<AudioAggregateLedger>) -> bool {
        self.custody
            .as_ref()
            .is_some_and(|owner| owner.has_physical_credit(ledger))
    }
    pub(crate) fn adopt_credit(&mut self, credit: DecodedPcmCredit) {
        assert!(
            self.credit.is_none(),
            "decoded PCM already has persistent credit"
        );
        self.credit = Some(credit);
        if let Some(custody) = &mut self.custody {
            custody.adopted();
        }
    }
}
impl Drop for DecodedPcm {
    fn drop(&mut self) {
        if let Some(custody) = self.custody.take() {
            custody.dispose(std::mem::take(&mut self.samples), self.credit.take());
        }
    }
}
impl fmt::Debug for DecodedPcm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DecodedPcm")
            .field("sample_rate", &self.sample_rate)
            .field("channels", &self.channels)
            .field("frames", &self.frames)
            .field("samples", &self.samples)
            .finish_non_exhaustive()
    }
}
