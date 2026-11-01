//! Constructor reservations become one zero-filled, physically owned backing.
//! Every materialization result keeps the physical permit with its allocation.
use super::*;

#[derive(Debug)]
pub struct PreparedInitialAudioBacking {
    registry: Arc<RegistryInner>,
    key: AudioBufferKey,
    planar: Vec<f32>,
    physical: Option<AudioAggregatePermit>,
}
impl PreparedInitialAudioBacking {
    fn publish(self) -> AudioPlanarBacking {
        {
            let mut state = self.registry.state.lock();
            let entry = state
                .entries
                .get_mut(&self.key)
                .expect("prepared initial AudioBuffer entry disappeared");
            let BackingState::Writable {
                initial_pending, ..
            } = &mut entry.state
            else {
                panic!("prepared initial AudioBuffer changed before commit")
            };
            assert!(
                *initial_pending,
                "initial AudioBuffer was already initialized"
            );
            *initial_pending = false;
        }
        AudioPlanarBacking {
            samples: self.planar,
            _physical: self.physical,
        }
    }
}

#[derive(Debug)]
pub enum PreparedWritableAudioBacking {
    Initial(PreparedInitialAudioBacking),
    Materialized(PreparedAudioBacking),
}
impl PreparedWritableAudioBacking {
    pub fn samples(&self) -> &[f32] {
        match self {
            Self::Initial(p) => &p.planar,
            Self::Materialized(p) => p.samples(),
        }
    }
    /// Encode the final reply while original PCM and output capacity are charged.
    /// Commit after encoding succeeds; refusal keeps the original entry.
    pub fn encode_le_reply(self, prefix: &[u8]) -> EngineResult<crate::sync_reply::SyncReplyBytes> {
        let len = prefix
            .len()
            .checked_add(self.samples().len().checked_mul(4).ok_or_else(|| {
                EngineError::from_detail(ErrorCode::InvalidArgument, "audio wire size overflow")
            })?)
            .ok_or_else(|| {
                EngineError::from_detail(ErrorCode::InvalidArgument, "audio wire size overflow")
            })?;
        let ledger = match &self {
            Self::Initial(p) => &p.registry.physical,
            Self::Materialized(p) => &p.registry.physical,
        };
        let physical = ledger.try_reserve(len, "AudioBuffer wire reply")?;
        let mut bytes = bytemuck::allocation::try_zeroed_vec::<u8>(len).map_err(|()| {
            EngineError::from_detail(
                ErrorCode::OutOfMemory,
                "failed to allocate audio wire reply",
            )
        })?;
        debug_assert_eq!(bytes.capacity(), len);
        bytes[..prefix.len()].copy_from_slice(prefix);
        for (out, sample) in bytes[prefix.len()..]
            .chunks_exact_mut(4)
            .zip(self.samples())
        {
            out.copy_from_slice(&sample.to_le_bytes());
        }
        drop(self.commit_owned());
        Ok(crate::sync_reply::SyncReplyBytes::charged(bytes, physical))
    }

    pub fn commit(self) -> AudioPlanarBacking {
        self.commit_owned()
    }
    pub fn commit_owned(self) -> AudioPlanarBacking {
        match self {
            Self::Initial(p) => p.publish(),
            Self::Materialized(p) => p.commit_owned(),
        }
    }
}
impl AudioResourceRegistry {
    /// Reserve the original process/runtime allowance before constructor allocation.
    pub fn reserve_initial_backing(
        &self,
        generation: i64,
        format: AudioBufferFormat,
    ) -> EngineResult<AudioBackingLease> {
        self.reserve_backing_for(generation, format, true)
    }
    /// Initialize a pending constructor once, otherwise use frozen materialization.
    pub fn prepare_writable_backing(
        &self,
        key: AudioBufferKey,
    ) -> EngineResult<PreparedWritableAudioBacking> {
        let (format, bytes, initial) = {
            let state = self.inner.state.lock();
            ensure_generation_active(&state, key.runtime_generation)?;
            let entry = state.entries.get(&key).ok_or_else(missing_backing_error)?;
            (
                entry.format,
                entry.byte_len,
                matches!(
                    &entry.state,
                    BackingState::Writable {
                        initial_pending: true,
                        ..
                    }
                ),
            )
        };
        if !initial {
            return self
                .prepare_materialize(key)
                .map(PreparedWritableAudioBacking::Materialized);
        }
        // The logical process/runtime allowance is already held by the entry.
        // This distinct physical ticket must precede the first native PCM allocation.
        let physical = self
            .inner
            .physical
            .try_reserve(bytes, "initial AudioBuffer")?;
        let planar = try_exact_pcm_allocation(checked_sample_count(format)?)?;
        Ok(PreparedWritableAudioBacking::Initial(
            PreparedInitialAudioBacking {
                registry: self.inner.clone(),
                key,
                planar,
                physical: Some(physical),
            },
        ))
    }
}
