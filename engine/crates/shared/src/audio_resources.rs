//! Host-side accounting for JavaScript-owned Web Audio PCM backing.
//!
//! This registry is deliberately a control-thread leaf. It is used while V8
//! validates and allocates `AudioBuffer` backing stores and at isolate lifecycle
//! boundaries; neither the audio callback nor any other real-time processing
//! path may acquire its metadata lock.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;

use crate::error::{EngineError, EngineResult, ErrorCode};

const MAX_SINGLE_BACKING_BYTES: usize = 64 * 1024 * 1024;
const MAX_RUNTIME_BACKING_BYTES: usize = 128 * 1024 * 1024;
const MAX_RUNTIME_BACKING_BUFFERS: usize = 512;
const MAX_PROCESS_BACKING_BYTES: usize = 256 * 1024 * 1024;
const MAX_PROCESS_BACKING_BUFFERS: usize = 2048;
const MAX_CHANNELS: u32 = 32;
const MIN_SAMPLE_RATE: u32 = 3_000;
const MAX_SAMPLE_RATE: u32 = 768_000;
const MAX_PUBLIC_SERIAL: u32 = i32::MAX as u32;

/// Stable identity for one JS-owned `AudioBuffer` backing store.
///
/// Keys are scoped to one [`AudioResourceRegistry`]. The generation prevents a
/// retired isolate's finalizer from naming a replacement isolate's resource;
/// serials are monotonic within the registry and are never reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AudioBufferKey {
    pub runtime_generation: i64,
    pub serial: u32,
}

/// Shape of a planar f32 `AudioBuffer` backing store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioBufferFormat {
    pub channels: u32,
    pub frames: u32,
    pub sample_rate: u32,
}

/// Proof that the registry admitted an `AudioBuffer` allocation.
///
/// The registry entry, rather than this lightweight value, owns the accounting
/// permits. This lets the key cross the JS/native boundary while
/// [`AudioResourceRegistry::release_buffer`] remains idempotent. If allocation
/// fails after admission, callers must release the returned key immediately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioBackingLease {
    key: AudioBufferKey,
    format: AudioBufferFormat,
    byte_len: usize,
}

impl AudioBackingLease {
    #[inline]
    pub fn key(self) -> AudioBufferKey {
        self.key
    }

    #[inline]
    pub fn format(self) -> AudioBufferFormat {
        self.format
    }

    #[inline]
    pub fn byte_len(self) -> usize {
        self.byte_len
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct AudioResourceLimits {
    pub(crate) max_single_bytes: usize,
    pub(crate) max_runtime_bytes: usize,
    pub(crate) max_runtime_buffers: usize,
    pub(crate) max_process_bytes: usize,
    pub(crate) max_process_buffers: usize,
}

impl AudioResourceLimits {
    const PRODUCTION: Self = Self {
        max_single_bytes: MAX_SINGLE_BACKING_BYTES,
        max_runtime_bytes: MAX_RUNTIME_BACKING_BYTES,
        max_runtime_buffers: MAX_RUNTIME_BACKING_BUFFERS,
        max_process_bytes: MAX_PROCESS_BACKING_BYTES,
        max_process_buffers: MAX_PROCESS_BACKING_BUFFERS,
    };
}

#[derive(Debug)]
struct ProcessUsage {
    max_bytes: usize,
    max_buffers: usize,
    bytes: AtomicUsize,
    buffers: AtomicUsize,
}

impl ProcessUsage {
    fn new(max_bytes: usize, max_buffers: usize) -> Self {
        Self {
            max_bytes,
            max_buffers,
            bytes: AtomicUsize::new(0),
            buffers: AtomicUsize::new(0),
        }
    }

    fn try_reserve(self: &Arc<Self>, bytes: usize) -> EngineResult<ProcessPermit> {
        self.buffers
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(1).filter(|next| *next <= self.max_buffers)
            })
            .map_err(|_| {
                EngineError::from_detail(
                    ErrorCode::InputSaturated,
                    "process AudioBuffer backing count limit exceeded",
                )
            })?;

        if self
            .bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes)
                    .filter(|next| *next <= self.max_bytes)
            })
            .is_err()
        {
            self.buffers.fetch_sub(1, Ordering::AcqRel);
            return Err(EngineError::from_detail(
                ErrorCode::InputSaturated,
                "process AudioBuffer backing byte limit exceeded",
            ));
        }

        Ok(ProcessPermit {
            usage: Arc::clone(self),
            bytes,
        })
    }

    #[cfg(test)]
    fn snapshot(&self) -> (usize, usize) {
        (
            self.bytes.load(Ordering::Acquire),
            self.buffers.load(Ordering::Acquire),
        )
    }
}

#[derive(Debug)]
struct ProcessPermit {
    usage: Arc<ProcessUsage>,
    bytes: usize,
}

/// Immutable, interleaved PCM acquired by one or more native source nodes.
///
/// The process permit lives in the snapshot itself, not in the registry entry.
/// Consequently a source-node `Arc` keeps both the samples and their accounting
/// alive after the JS `AudioBuffer` is released or materialized into a newer
/// writable version.
#[derive(Debug)]
pub struct AudioSnapshot {
    format: AudioBufferFormat,
    samples: Vec<f32>,
    _process_permit: ProcessPermit,
}

impl AudioSnapshot {
    #[inline]
    pub fn format(&self) -> AudioBufferFormat {
        self.format
    }

    #[inline]
    pub fn samples(&self) -> &[f32] {
        &self.samples
    }
}

impl Drop for ProcessPermit {
    fn drop(&mut self) {
        self.usage.bytes.fetch_sub(self.bytes, Ordering::AcqRel);
        self.usage.buffers.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Debug, Default)]
struct RuntimeUsage {
    bytes: usize,
    buffers: usize,
    retiring: bool,
}

#[derive(Debug)]
struct BackingEntry {
    format: AudioBufferFormat,
    byte_len: usize,
    state: BackingState,
}

#[derive(Debug)]
enum BackingState {
    /// The registry permit accounts for the planar allocation currently owned
    /// by V8. The allocation itself stays on the JS side.
    Writable { _process_permit: ProcessPermit },
    /// The snapshot owns its own permit and may outlive this entry through
    /// source-node `Arc` clones.
    Frozen(Arc<AudioSnapshot>),
}

#[derive(Debug)]
struct RegistryState {
    next_serial: u32,
    serial_exhausted: bool,
    runtimes: HashMap<i64, RuntimeUsage>,
    /// Runtime generations are positive and advance monotonically by one for
    /// this Host. One high-watermark therefore tombstones every retired key in
    /// O(1) space, unlike a per-restart set that would leak metadata forever.
    retired_through: i64,
    entries: HashMap<AudioBufferKey, BackingEntry>,
}

impl RegistryState {
    fn new(next_serial: u32) -> Self {
        Self {
            next_serial,
            serial_exhausted: next_serial == 0 || next_serial > MAX_PUBLIC_SERIAL,
            runtimes: HashMap::new(),
            retired_through: 0,
            entries: HashMap::new(),
        }
    }
}

#[derive(Debug)]
struct RegistryInner {
    limits: AudioResourceLimits,
    process: Arc<ProcessUsage>,
    state: Mutex<RegistryState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SnapshotPreparation {
    FreezeWritable,
    ReuseFrozen,
}

/// A fully allocated snapshot awaiting atomic publication.
///
/// Callers first clone [`Self::snapshot`] into the audio command. If command
/// publication succeeds, they detach/transfer the old writable backing and then
/// invoke [`Self::commit`] while still holding the runtime's audio-publication
/// lock. Dropping this value instead rolls back its snapshot permit and leaves a
/// writable entry untouched.
#[derive(Debug)]
pub struct PreparedAudioSnapshot {
    registry: Arc<RegistryInner>,
    key: AudioBufferKey,
    snapshot: Arc<AudioSnapshot>,
    preparation: SnapshotPreparation,
}

impl PreparedAudioSnapshot {
    #[inline]
    pub fn snapshot(&self) -> Arc<AudioSnapshot> {
        Arc::clone(&self.snapshot)
    }

    /// Publish the prepared state transition. This has no recoverable failure:
    /// the caller's publication lock guarantees that the entry cannot change
    /// between prepare and commit. Violating that internal precondition is a
    /// programming error and fails closed with a panic.
    pub fn commit(self) -> Arc<AudioSnapshot> {
        let Self {
            registry,
            key,
            snapshot,
            preparation,
        } = self;
        if preparation == SnapshotPreparation::FreezeWritable {
            let old_state = {
                let mut state = registry.state.lock();
                let entry = state
                    .entries
                    .get_mut(&key)
                    .expect("prepared AudioBuffer entry disappeared before snapshot commit");
                assert!(
                    matches!(&entry.state, BackingState::Writable { .. }),
                    "prepared AudioBuffer changed before snapshot commit"
                );
                std::mem::replace(
                    &mut entry.state,
                    BackingState::Frozen(Arc::clone(&snapshot)),
                )
            };
            drop(old_state);
        }
        snapshot
    }
}

/// Exact planar PCM awaiting publication as a new V8 backing store.
///
/// Dropping this value returns the newly reserved process permit and leaves the
/// entry frozen. [`Self::commit`] transfers the permit into the registry and
/// returns the exact-capacity allocation for zero-copy adoption by V8.
#[derive(Debug)]
pub struct PreparedAudioBacking {
    registry: Arc<RegistryInner>,
    key: AudioBufferKey,
    source_snapshot: Arc<AudioSnapshot>,
    planar: Vec<f32>,
    writable_permit: ProcessPermit,
}

impl PreparedAudioBacking {
    #[inline]
    pub fn samples(&self) -> &[f32] {
        &self.planar
    }

    /// Publish Frozen → Writable under the runtime publication lock.
    pub fn commit(self) -> Vec<f32> {
        let Self {
            registry,
            key,
            source_snapshot,
            planar,
            writable_permit,
        } = self;
        let old_state = {
            let mut state = registry.state.lock();
            let entry = state
                .entries
                .get_mut(&key)
                .expect("prepared AudioBuffer entry disappeared before materialize commit");
            match &entry.state {
                BackingState::Frozen(current) if Arc::ptr_eq(current, &source_snapshot) => {}
                _ => panic!("prepared AudioBuffer changed before materialize commit"),
            }
            std::mem::replace(
                &mut entry.state,
                BackingState::Writable {
                    _process_permit: writable_permit,
                },
            )
        };
        drop(old_state);
        planar
    }
}

/// A writable `AudioBuffer`'s channel-major samples, as the caller holds them.
#[derive(Clone, Copy)]
enum PlanarPcm<'a> {
    /// Viewed in place: a V8 backing store in this process.
    Samples(&'a [f32]),
    /// Four little-endian bytes per sample, with no alignment promised.
    LittleEndian(&'a [u8]),
}

impl PlanarPcm<'_> {
    /// How many samples this is, or `None` for bytes that end mid-sample.
    fn sample_count(self) -> Option<usize> {
        match self {
            Self::Samples(samples) => Some(samples.len()),
            Self::LittleEndian(bytes) => bytes
                .len()
                .is_multiple_of(size_of::<f32>())
                .then_some(bytes.len() / size_of::<f32>()),
        }
    }

    #[inline]
    fn sample(self, index: usize) -> f32 {
        match self {
            Self::Samples(samples) => samples[index],
            Self::LittleEndian(bytes) => {
                let at = index * size_of::<f32>();
                f32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
            }
        }
    }
}

/// Per-host registry for JS-owned Web Audio resources.
///
/// Clones share one registry. Separately constructed production registries
/// still share the process-wide counters, so multiple live Hosts cannot each
/// consume the full process budget independently.
#[derive(Clone, Debug)]
pub struct AudioResourceRegistry {
    inner: Arc<RegistryInner>,
}

impl Default for AudioResourceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioResourceRegistry {
    pub fn new() -> Self {
        static PROCESS_USAGE: OnceLock<Arc<ProcessUsage>> = OnceLock::new();
        let limits = AudioResourceLimits::PRODUCTION;
        let process = Arc::clone(PROCESS_USAGE.get_or_init(|| {
            Arc::new(ProcessUsage::new(
                limits.max_process_bytes,
                limits.max_process_buffers,
            ))
        }));
        Self::with_parts(limits, process, 1)
    }

    fn with_parts(
        limits: AudioResourceLimits,
        process: Arc<ProcessUsage>,
        next_serial: u32,
    ) -> Self {
        Self {
            inner: Arc::new(RegistryInner {
                limits,
                process,
                state: Mutex::new(RegistryState::new(next_serial)),
            }),
        }
    }

    /// Atomically admits an exact JS backing-store footprint before allocation.
    ///
    /// No bytes are allocated here. The returned entry already owns both the
    /// runtime and process budget, so the caller can allocate only after this
    /// succeeds and can roll back with [`Self::release_buffer`] on allocation
    /// failure.
    pub fn reserve_backing(
        &self,
        runtime_generation: i64,
        format: AudioBufferFormat,
    ) -> EngineResult<AudioBackingLease> {
        let byte_len = validated_byte_len(format, self.inner.limits.max_single_bytes)?;
        let process_permit = self.inner.process.try_reserve(byte_len)?;
        self.admit(
            runtime_generation,
            format,
            byte_len,
            BackingState::Writable {
                _process_permit: process_permit,
            },
        )
    }

    /// Admit PCM that already exists natively -- a decode -- as a frozen entry.
    ///
    /// No JavaScript backing store is made: the samples stay here, interleaved
    /// as the audio thread plays them, and the `AudioBuffer` starts with no
    /// backing. Starting it reuses this snapshot without a copy; reading its
    /// channels materializes a writable copy ([`Self::prepare_materialize`]).
    /// A decoded clip that is only played therefore costs one PCM allocation,
    /// where handing it to JavaScript cost three -- the decode, the planar copy
    /// V8 adopted, and the interleaved snapshot `start()` froze back from it --
    /// with two of them alive at once. And where content's JavaScript runs in
    /// another process, the PCM never crosses to it unless content reads it.
    ///
    /// Admitted exactly as [`Self::reserve_backing`] admits: a retiring runtime
    /// or a spent budget refuses it before anything is recorded, and dropping
    /// the refused samples returns everything.
    pub fn adopt_frozen(
        &self,
        runtime_generation: i64,
        format: AudioBufferFormat,
        mut interleaved: Vec<f32>,
    ) -> EngineResult<AudioBackingLease> {
        let byte_len = validated_byte_len(format, self.inner.limits.max_single_bytes)?;
        let sample_count = checked_sample_count(format)?;
        if interleaved.len() != sample_count {
            return Err(EngineError::from_detail(
                ErrorCode::InvalidArgument,
                format!(
                    "decoded PCM length mismatch: expected {sample_count}, got {}",
                    interleaved.len()
                ),
            ));
        }
        // The budget counts the samples, so what is retained must be exactly
        // them: a decoder's spare capacity would be memory no limit sees.
        interleaved.shrink_to_fit();
        if interleaved.capacity() != sample_count {
            let mut exact = try_exact_pcm_allocation(sample_count)?;
            exact.copy_from_slice(&interleaved);
            interleaved = exact;
        }
        let process_permit = self.inner.process.try_reserve(byte_len)?;
        self.admit(
            runtime_generation,
            format,
            byte_len,
            BackingState::Frozen(Arc::new(AudioSnapshot {
                format,
                samples: interleaved,
                _process_permit: process_permit,
            })),
        )
    }

    /// Record a new entry whose process budget is already held by `backing`,
    /// charging its runtime. On refusal `backing` is dropped, which returns
    /// that budget.
    fn admit(
        &self,
        runtime_generation: i64,
        format: AudioBufferFormat,
        byte_len: usize,
        backing: BackingState,
    ) -> EngineResult<AudioBackingLease> {
        let mut state = self.inner.state.lock();

        if runtime_generation <= state.retired_through
            || state
                .runtimes
                .get(&runtime_generation)
                .is_some_and(|runtime| runtime.retiring)
        {
            return Err(EngineError::from_detail(
                ErrorCode::InvalidOperation,
                "AudioBuffer runtime generation is retired",
            ));
        }

        let (runtime_bytes, runtime_buffers) = state
            .runtimes
            .get(&runtime_generation)
            .map_or((0, 0), |usage| (usage.bytes, usage.buffers));
        let next_runtime_bytes = runtime_bytes
            .checked_add(byte_len)
            .filter(|next| *next <= self.inner.limits.max_runtime_bytes);
        let next_runtime_buffers = runtime_buffers
            .checked_add(1)
            .filter(|next| *next <= self.inner.limits.max_runtime_buffers);
        let (next_runtime_bytes, next_runtime_buffers) =
            match (next_runtime_bytes, next_runtime_buffers) {
                (Some(bytes), Some(buffers)) => (bytes, buffers),
                _ => {
                    return Err(EngineError::from_detail(
                        ErrorCode::InputSaturated,
                        "runtime AudioBuffer backing limit exceeded",
                    ));
                }
            };

        if state.serial_exhausted {
            return Err(EngineError::from_detail(
                ErrorCode::InvalidOperation,
                "AudioBuffer id space exhausted",
            ));
        }
        let serial = state.next_serial;
        if serial == MAX_PUBLIC_SERIAL {
            state.serial_exhausted = true;
        } else {
            state.next_serial += 1;
        }

        let key = AudioBufferKey {
            runtime_generation,
            serial,
        };
        let previous = state.entries.insert(
            key,
            BackingEntry {
                format,
                byte_len,
                state: backing,
            },
        );
        debug_assert!(previous.is_none(), "monotonic AudioBuffer key collided");
        let runtime = state.runtimes.entry(runtime_generation).or_default();
        runtime.bytes = next_runtime_bytes;
        runtime.buffers = next_runtime_buffers;

        Ok(AudioBackingLease {
            key,
            format,
            byte_len,
        })
    }

    /// Prepare a native immutable snapshot without changing the registry state.
    ///
    /// A writable entry requires its exact channel-major planar slice. A frozen
    /// entry requires `None` and returns the existing `Arc` without allocating.
    /// The caller publishes the returned `Arc` first and then invokes
    /// [`PreparedAudioSnapshot::commit`] under its audio-publication lock.
    pub fn prepare_snapshot(
        &self,
        key: AudioBufferKey,
        planar: Option<&[f32]>,
    ) -> EngineResult<PreparedAudioSnapshot> {
        self.prepare_snapshot_of(key, planar.map(PlanarPcm::Samples))
    }

    /// [`Self::prepare_snapshot`] for a writable entry whose planar PCM arrived
    /// as little-endian bytes: from content in another process, where the
    /// buffer crossed as a message rather than as memory this process can view
    /// as `f32`. Read straight into the interleaved snapshot, so the bytes are
    /// not first copied into an aligned planar buffer.
    pub fn prepare_snapshot_from_le_bytes(
        &self,
        key: AudioBufferKey,
        planar: &[u8],
    ) -> EngineResult<PreparedAudioSnapshot> {
        self.prepare_snapshot_of(key, Some(PlanarPcm::LittleEndian(planar)))
    }

    fn prepare_snapshot_of(
        &self,
        key: AudioBufferKey,
        planar: Option<PlanarPcm<'_>>,
    ) -> EngineResult<PreparedAudioSnapshot> {
        let (format, byte_len) = {
            let state = self.inner.state.lock();
            ensure_generation_active(&state, key.runtime_generation)?;
            let entry = state.entries.get(&key).ok_or_else(missing_backing_error)?;
            match &entry.state {
                BackingState::Frozen(snapshot) => {
                    if planar.is_some() {
                        return Err(EngineError::from_detail(
                            ErrorCode::InvalidOperation,
                            "frozen AudioBuffer cannot accept a second writable backing",
                        ));
                    }
                    return Ok(PreparedAudioSnapshot {
                        registry: Arc::clone(&self.inner),
                        key,
                        snapshot: Arc::clone(snapshot),
                        preparation: SnapshotPreparation::ReuseFrozen,
                    });
                }
                BackingState::Writable { .. } => {
                    if planar.is_none() {
                        return Err(EngineError::from_detail(
                            ErrorCode::InvalidOperation,
                            "writable AudioBuffer requires planar PCM for snapshot acquisition",
                        ));
                    }
                    (entry.format, entry.byte_len)
                }
            }
        };

        let planar = planar.expect("Writable was checked to carry planar PCM");
        let sample_count = checked_sample_count(format)?;
        if planar.sample_count() != Some(sample_count) {
            return Err(EngineError::from_detail(
                ErrorCode::InvalidArgument,
                match planar.sample_count() {
                    Some(got) => format!(
                        "AudioBuffer planar PCM length mismatch: expected {sample_count}, got {got}"
                    ),
                    None => "AudioBuffer planar PCM bytes are not f32 aligned".to_string(),
                },
            ));
        }

        // Reserve the second physical copy before its exact-size allocation.
        // Dropping either local on any later error rolls the reservation back.
        let snapshot_permit = self.inner.process.try_reserve(byte_len)?;
        let mut interleaved = try_exact_pcm_allocation(sample_count)?;
        let channels = format.channels as usize;
        let frames = format.frames as usize;
        for frame in 0..frames {
            for channel in 0..channels {
                interleaved[frame * channels + channel] = planar.sample(channel * frames + frame);
            }
        }
        let snapshot = Arc::new(AudioSnapshot {
            format,
            samples: interleaved,
            _process_permit: snapshot_permit,
        });

        Ok(PreparedAudioSnapshot {
            registry: Arc::clone(&self.inner),
            key,
            snapshot,
            preparation: SnapshotPreparation::FreezeWritable,
        })
    }

    /// Prepare an exact planar allocation from a frozen snapshot.
    ///
    /// The frozen entry remains authoritative until
    /// [`PreparedAudioBacking::commit`]. Dropping the preparation therefore
    /// rolls back both allocation and permit with no state change.
    pub fn prepare_materialize(&self, key: AudioBufferKey) -> EngineResult<PreparedAudioBacking> {
        let (format, byte_len, source_snapshot) = {
            let state = self.inner.state.lock();
            ensure_generation_active(&state, key.runtime_generation)?;
            let entry = state.entries.get(&key).ok_or_else(missing_backing_error)?;
            match &entry.state {
                BackingState::Writable { .. } => {
                    return Err(EngineError::from_detail(
                        ErrorCode::InvalidOperation,
                        "writable AudioBuffer is already materialized",
                    ));
                }
                BackingState::Frozen(snapshot) => {
                    (entry.format, entry.byte_len, Arc::clone(snapshot))
                }
            }
        };

        let sample_count = checked_sample_count(format)?;
        if source_snapshot.samples().len() != sample_count {
            return Err(EngineError::from_detail(
                ErrorCode::Internal,
                "frozen AudioBuffer snapshot length invariant violated",
            ));
        }
        let writable_permit = self.inner.process.try_reserve(byte_len)?;
        let mut planar = try_exact_pcm_allocation(sample_count)?;
        let channels = format.channels as usize;
        let frames = format.frames as usize;
        for frame in 0..frames {
            for channel in 0..channels {
                planar[channel * frames + frame] =
                    source_snapshot.samples()[frame * channels + channel];
            }
        }

        Ok(PreparedAudioBacking {
            registry: Arc::clone(&self.inner),
            key,
            source_snapshot,
            planar,
            writable_permit,
        })
    }

    /// Release one backing-store entry. Returns whether the key was live.
    ///
    /// A stale V8 finalizer may race explicit cleanup, so missing keys are a
    /// successful no-op by design.
    pub fn release_buffer(&self, key: AudioBufferKey) -> bool {
        let mut state = self.inner.state.lock();
        let Some(entry) = state.entries.remove(&key) else {
            return false;
        };
        let remove_runtime = {
            let runtime = state
                .runtimes
                .get_mut(&key.runtime_generation)
                .expect("live backing entry has runtime accounting");
            runtime.bytes = runtime
                .bytes
                .checked_sub(entry.byte_len)
                .expect("runtime AudioBuffer byte accounting underflow");
            runtime.buffers = runtime
                .buffers
                .checked_sub(1)
                .expect("runtime AudioBuffer count accounting underflow");
            runtime.bytes == 0 && runtime.buffers == 0 && !runtime.retiring
        };
        if remove_runtime {
            state.runtimes.remove(&key.runtime_generation);
        }
        drop(state);
        drop(entry);
        true
    }

    /// Return the immutable shape associated with a live backing key.
    ///
    /// This is a V8/Host control-thread lookup for snapshot/materialization
    /// setup; it must not be called by the audio real-time callback.
    pub fn format(&self, key: AudioBufferKey) -> Option<AudioBufferFormat> {
        self.inner
            .state
            .lock()
            .entries
            .get(&key)
            .map(|entry| entry.format)
    }

    /// Fence a runtime generation before its cleanup barrier and isolate drop.
    pub fn begin_retire(&self, runtime_generation: i64) {
        let mut state = self.inner.state.lock();
        if runtime_generation <= state.retired_through {
            return;
        }
        state
            .runtimes
            .entry(runtime_generation)
            .or_default()
            .retiring = true;
    }

    /// Reclaim a generation after its V8 isolate has been destroyed.
    ///
    /// This method must be called only after isolate drop. It permanently
    /// tombstones the generation, then returns every surviving process permit.
    pub fn finish_runtime_drop(&self, runtime_generation: i64) {
        let entries = {
            let mut state = self.inner.state.lock();
            debug_assert!(
                runtime_generation >= state.retired_through,
                "runtime generations must retire monotonically"
            );
            state.retired_through = state.retired_through.max(runtime_generation);
            let retired_through = state.retired_through;
            state
                .runtimes
                .retain(|generation, _| *generation > retired_through);
            let keys = state
                .entries
                .keys()
                .filter(|key| key.runtime_generation <= retired_through)
                .copied()
                .collect::<Vec<_>>();
            keys.into_iter()
                .filter_map(|key| state.entries.remove(&key))
                .collect::<Vec<_>>()
        };
        drop(entries);
    }

    #[cfg(test)]
    pub(crate) fn runtime_usage(&self, runtime_generation: i64) -> (usize, usize) {
        let state = self.inner.state.lock();
        state
            .runtimes
            .get(&runtime_generation)
            .map_or((0, 0), |usage| (usage.bytes, usage.buffers))
    }
}

fn missing_backing_error() -> EngineError {
    EngineError::from_detail(ErrorCode::NotFound, "AudioBuffer backing is not live")
}

fn ensure_generation_active(state: &RegistryState, runtime_generation: i64) -> EngineResult<()> {
    if runtime_generation <= state.retired_through
        || state
            .runtimes
            .get(&runtime_generation)
            .is_some_and(|runtime| runtime.retiring)
    {
        return Err(EngineError::from_detail(
            ErrorCode::InvalidOperation,
            "AudioBuffer runtime generation is retired",
        ));
    }
    Ok(())
}

fn checked_sample_count(format: AudioBufferFormat) -> EngineResult<usize> {
    let channels = usize::try_from(format.channels).map_err(|_| {
        EngineError::from_detail(
            ErrorCode::InvalidArgument,
            "AudioBuffer channel count does not fit this platform",
        )
    })?;
    let frames = usize::try_from(format.frames).map_err(|_| {
        EngineError::from_detail(
            ErrorCode::InvalidArgument,
            "AudioBuffer frame count does not fit this platform",
        )
    })?;
    channels.checked_mul(frames).ok_or_else(|| {
        EngineError::from_detail(
            ErrorCode::InvalidArgument,
            "AudioBuffer sample count overflow",
        )
    })
}

fn try_exact_pcm_allocation(sample_count: usize) -> EngineResult<Vec<f32>> {
    let samples = bytemuck::allocation::try_zeroed_vec(sample_count).map_err(|()| {
        EngineError::from_detail(ErrorCode::OutOfMemory, "failed to allocate AudioBuffer PCM")
    })?;
    debug_assert_eq!(samples.len(), sample_count);
    debug_assert_eq!(samples.capacity(), sample_count);
    Ok(samples)
}

fn validated_byte_len(format: AudioBufferFormat, max_single_bytes: usize) -> EngineResult<usize> {
    if !(1..=MAX_CHANNELS).contains(&format.channels) {
        return Err(EngineError::from_detail(
            ErrorCode::InvalidArgument,
            format!("AudioBuffer channels must be 1..={MAX_CHANNELS}"),
        ));
    }
    if format.frames == 0 {
        return Err(EngineError::from_detail(
            ErrorCode::InvalidArgument,
            "AudioBuffer frames must be positive",
        ));
    }
    if !(MIN_SAMPLE_RATE..=MAX_SAMPLE_RATE).contains(&format.sample_rate) {
        return Err(EngineError::from_detail(
            ErrorCode::InvalidArgument,
            format!("AudioBuffer sample rate must be {MIN_SAMPLE_RATE}..={MAX_SAMPLE_RATE}"),
        ));
    }

    let byte_len = usize::try_from(format.channels)
        .ok()
        .and_then(|channels| {
            usize::try_from(format.frames)
                .ok()
                .and_then(|frames| channels.checked_mul(frames))
        })
        .and_then(|samples| samples.checked_mul(size_of::<f32>()))
        .ok_or_else(|| {
            EngineError::from_detail(
                ErrorCode::InvalidArgument,
                "AudioBuffer backing size overflow",
            )
        })?;
    if byte_len > max_single_bytes {
        return Err(EngineError::from_detail(
            ErrorCode::InvalidArgument,
            format!("AudioBuffer backing exceeds {max_single_bytes} bytes"),
        ));
    }
    Ok(byte_len)
}

#[cfg(test)]
pub(crate) struct AudioResourceTestScope {
    limits: AudioResourceLimits,
    process: Arc<ProcessUsage>,
}

#[cfg(test)]
impl AudioResourceTestScope {
    pub(crate) fn new(limits: AudioResourceLimits) -> Self {
        Self {
            process: Arc::new(ProcessUsage::new(
                limits.max_process_bytes,
                limits.max_process_buffers,
            )),
            limits,
        }
    }

    pub(crate) fn registry(&self) -> AudioResourceRegistry {
        AudioResourceRegistry::with_parts(self.limits, Arc::clone(&self.process), 1)
    }

    pub(crate) fn registry_with_next_serial(&self, serial: u32) -> AudioResourceRegistry {
        AudioResourceRegistry::with_parts(self.limits, Arc::clone(&self.process), serial)
    }

    pub(crate) fn process_usage(&self) -> (usize, usize) {
        self.process.snapshot()
    }
}
