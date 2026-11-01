//! Encoded replies keep physical credit until their actual byte allocation drops.
use crate::audio_physical::{AudioAggregateLedger, AudioAggregatePermit};
use crate::error::{EngineError, EngineResult, ErrorCode};
use std::sync::Arc;
use std::{fmt, ops::Deref};
#[derive(Default)]
pub struct SyncReplyBytes {
    bytes: Vec<u8>,
    _physical: Option<AudioAggregatePermit>,
    start: usize,
}
impl SyncReplyBytes {
    pub(crate) fn charged(bytes: Vec<u8>, physical: AudioAggregatePermit) -> Self {
        Self {
            bytes,
            _physical: Some(physical),
            start: 0,
        }
    }
    pub fn capacity(&self) -> usize {
        self.bytes.capacity()
    }
    pub fn is_charged(&self) -> bool {
        self._physical.is_some()
    }
    /// Reveal only already-reserved headroom, preserving the allocation.
    pub fn prepend(&mut self, prefix: &[u8]) -> EngineResult<()> {
        if prefix.len() > self.start {
            return Err(EngineError::from_detail(
                ErrorCode::InvalidArgument,
                "wire reply has insufficient reserved headroom",
            ));
        }
        let start = self.start - prefix.len();
        self.bytes[start..self.start].copy_from_slice(prefix);
        self.start = start;
        Ok(())
    }
    /// Encode directly into one exact allocation after admitting its full capacity.
    pub fn encode_audio(
        ledger: &Arc<AudioAggregateLedger>,
        headroom: usize,
        parts: &[AudioWirePart<'_>],
    ) -> EngineResult<Self> {
        let overflow =
            || EngineError::from_detail(ErrorCode::InvalidArgument, "audio wire size overflow");
        let mut len = headroom;
        for part in parts {
            let bytes = match part {
                AudioWirePart::Bytes(v) => v.len(),
                AudioWirePart::Floats(v) => v.len().checked_mul(4).ok_or_else(overflow)?,
            };
            len = len.checked_add(bytes).ok_or_else(overflow)?;
        }
        let physical = ledger.try_reserve(len, "audio service wire reply")?;
        let mut bytes = bytemuck::allocation::try_zeroed_vec::<u8>(len).map_err(|()| {
            EngineError::from_detail(
                ErrorCode::OutOfMemory,
                "failed to allocate audio service wire reply",
            )
        })?;
        debug_assert_eq!(bytes.capacity(), len);
        let mut at = headroom;
        for part in parts {
            match part {
                AudioWirePart::Bytes(v) => {
                    bytes[at..at + v.len()].copy_from_slice(v);
                    at += v.len();
                }
                AudioWirePart::Floats(v) => {
                    for sample in *v {
                        bytes[at..at + 4].copy_from_slice(&sample.to_le_bytes());
                        at += 4;
                    }
                }
            }
        }
        Ok(Self {
            bytes,
            _physical: Some(physical),
            start: headroom,
        })
    }
}
pub enum AudioWirePart<'a> {
    Bytes(&'a [u8]),
    Floats(&'a [f32]),
}
impl AsRef<[u8]> for SyncReplyBytes {
    fn as_ref(&self) -> &[u8] {
        &self.bytes[self.start..]
    }
}
impl From<Vec<u8>> for SyncReplyBytes {
    fn from(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            _physical: None,
            start: 0,
        }
    }
}
impl Deref for SyncReplyBytes {
    type Target = [u8];
    fn deref(&self) -> &Self::Target {
        self.as_ref()
    }
}
impl fmt::Debug for SyncReplyBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_ref().fmt(f)
    }
}
impl PartialEq for SyncReplyBytes {
    fn eq(&self, other: &Self) -> bool {
        self.as_ref() == other.as_ref()
    }
}
impl Eq for SyncReplyBytes {}
impl PartialEq<Vec<u8>> for SyncReplyBytes {
    fn eq(&self, other: &Vec<u8>) -> bool {
        self.as_ref() == other.as_slice()
    }
}

#[cfg(test)]
#[path = "audio_service_wire_bytes_tests.rs"]
mod service_wire_tests;
#[cfg(test)]
pub(crate) fn service_wire_custody_test() {
    service_wire_tests::lifetime();
}
