//! Audio buffer cache with LRU eviction
//!
//! Caches decoded audio data to avoid repeated downloads and decoding.
//! Uses URL as key and applies LRU eviction when memory limit is exceeded.

use std::collections::{BTreeMap, HashMap};
use std::ops::Deref;
use std::sync::{Arc, Mutex};

use shared::error::{EngineError, EngineResult, ErrorCode};

use crate::decoder::DecodedAudio;
use crate::limits::{AudioAggregateLedger, AudioAggregatePermit, MAX_AUDIO_CACHE_BYTES, pcm_bytes};

/// Decoded audio retained by the cache and any player that references it.
///
/// The permit is inside the shared value, so eviction only drops the cache's
/// Arc. Managed large backing stays charged through its actual CPU disposal.
pub struct CachedAudio {
    audio: retirement::Value,
    source_timeline: Option<crate::streaming::StreamTimeline>,
}

impl CachedAudio {
    fn new(
        audio: DecodedAudio,
        permit: AudioAggregatePermit,
        source_timeline: Option<crate::streaming::StreamTimeline>,
        cpu: Option<&migo_io::audio_cpu::AudioCpu>,
    ) -> Self {
        Self {
            audio: retirement::Value::new(audio, permit, cpu),
            source_timeline,
        }
    }
    pub(crate) fn source_timeline(&self) -> Option<crate::streaming::StreamTimeline> {
        self.source_timeline
    }
}

impl Deref for CachedAudio {
    type Target = DecodedAudio;

    fn deref(&self) -> &Self::Target {
        &self.audio
    }
}
/// Cache entry with metadata
struct CacheEntry {
    /// Decoded audio data (shared across players).
    audio: Arc<CachedAudio>,
    /// Size in bytes (samples.capacity() * 4).
    size_bytes: usize,
    /// Access order (higher = more recent).
    access_order: u64,
}

/// Global audio cache with LRU eviction.
pub struct AudioCache {
    entries: HashMap<String, CacheEntry>,
    order_index: BTreeMap<u64, String>,
    current_size: usize,
    max_size: usize,
    access_counter: u64,
    aggregate: Arc<AudioAggregateLedger>,
    cpu: Option<migo_io::audio_cpu::AudioCpu>,
}

impl AudioCache {
    pub fn new() -> Self {
        Self::with_max_size_and_aggregate(
            MAX_AUDIO_CACHE_BYTES,
            AudioAggregateLedger::process_global(),
        )
    }

    pub(crate) fn with_cpu(cpu: migo_io::audio_cpu::AudioCpu) -> Self {
        Self {
            cpu: Some(cpu),
            ..Self::new()
        }
    }

    const DEFAULT_CACHE_CAPACITY: usize = 16;

    pub fn with_max_size(max_size: usize) -> Self {
        Self::with_max_size_and_aggregate(max_size, AudioAggregateLedger::process_global())
    }

    pub(crate) fn with_max_size_and_aggregate(
        max_size: usize,
        aggregate: Arc<AudioAggregateLedger>,
    ) -> Self {
        Self {
            entries: HashMap::with_capacity(Self::DEFAULT_CACHE_CAPACITY),
            order_index: BTreeMap::new(),
            current_size: 0,
            max_size,
            access_counter: 0,
            aggregate,
            cpu: None,
        }
    }

    pub fn get(&mut self, url: &str) -> Option<Arc<CachedAudio>> {
        if let Some(entry) = self.entries.get_mut(url) {
            self.order_index.remove(&entry.access_order);
            self.access_counter += 1;
            entry.access_order = self.access_counter;
            self.order_index
                .insert(self.access_counter, url.to_string());
            tracing::trace!("Cache hit: {}", url);
            Some(Arc::clone(&entry.audio))
        } else {
            tracing::trace!("Cache miss: {}", url);
            None
        }
    }

    pub fn insert(&mut self, url: String, audio: DecodedAudio) -> EngineResult<Arc<CachedAudio>> {
        let size_bytes = pcm_bytes(audio.samples.capacity())?;
        let permit = self.aggregate.try_reserve(size_bytes, "audio cache")?;
        Ok(self.insert_with_permit(url, audio, permit))
    }

    pub(crate) fn insert_with_permit(
        &mut self,
        url: String,
        audio: DecodedAudio,
        permit: AudioAggregatePermit,
    ) -> Arc<CachedAudio> {
        self.insert_with_source(url, audio, permit, None)
    }

    fn insert_with_source(
        &mut self,
        url: String,
        audio: DecodedAudio,
        permit: AudioAggregatePermit,
        timeline: Option<crate::streaming::StreamTimeline>,
    ) -> Arc<CachedAudio> {
        let audio = CachedAudio::new(audio, permit, timeline, self.cpu.as_ref());
        self.insert_ready(url, audio)
    }

    // An already protected backing keeps its one original physical permit.
    fn insert_ready(&mut self, url: String, audio: CachedAudio) -> Arc<CachedAudio> {
        let size_bytes = audio.samples.capacity() * std::mem::size_of::<f32>();
        if size_bytes <= self.max_size {
            while self.current_size + size_bytes > self.max_size && !self.entries.is_empty() {
                self.evict_lru();
            }
        } else {
            tracing::debug!(
                "Audio too large to cache: {} bytes (max: {})",
                size_bytes,
                self.max_size
            );
            return Arc::new(audio);
        }

        self.access_counter += 1;
        let audio = Arc::new(audio);
        if let Some(old) = self.entries.remove(&url) {
            self.current_size = self.current_size.saturating_sub(old.size_bytes);
            self.order_index.remove(&old.access_order);
        }
        self.entries.insert(
            url.clone(),
            CacheEntry {
                audio: Arc::clone(&audio),
                size_bytes,
                access_order: self.access_counter,
            },
        );
        self.order_index.insert(self.access_counter, url.clone());
        self.current_size += size_bytes;
        tracing::debug!(
            "Cached audio: {} ({} bytes, total cache: {} bytes)",
            url,
            size_bytes,
            self.current_size
        );
        audio
    }

    fn evict_lru(&mut self) {
        if let Some((&order, _)) = self.order_index.iter().next() {
            if let Some(url) = self.order_index.remove(&order) {
                if let Some(entry) = self.entries.remove(&url) {
                    self.current_size = self.current_size.saturating_sub(entry.size_bytes);
                    tracing::debug!("Evicted from cache: {} ({} bytes)", url, entry.size_bytes);
                }
            }
        }
    }

    pub fn remove(&mut self, url: &str) {
        if let Some(entry) = self.entries.remove(url) {
            self.current_size = self.current_size.saturating_sub(entry.size_bytes);
            self.order_index.remove(&entry.access_order);
            tracing::debug!("Removed from cache: {} ({} bytes)", url, entry.size_bytes);
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.order_index.clear();
        self.current_size = 0;
        self.access_counter = 0;
        tracing::debug!("Cache cleared");
    }

    pub fn stats(&self) -> CacheStats {
        CacheStats {
            entry_count: self.entries.len(),
            current_size: self.current_size,
            max_size: self.max_size,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spare `Vec` capacity is heap the process is holding, and the resampler
    /// reserves an estimate then pushes fewer samples -- so charging by logical
    /// length let the cache hold more than its own limit. `limits.rs` already
    /// charges retained PCM by capacity, and has a test saying so.
    #[test]
    fn entries_are_charged_by_allocation_capacity_not_length() {
        super::rejection_tests::verify();
        let mut samples = Vec::with_capacity(64);
        samples.push(0.0f32);
        let capacity_bytes = samples.capacity() * std::mem::size_of::<f32>();
        assert!(capacity_bytes > samples.len() * std::mem::size_of::<f32>());

        let mut cache = AudioCache::with_max_size_and_aggregate(
            1024,
            Arc::new(AudioAggregateLedger::new(MAX_AUDIO_CACHE_BYTES)),
        );
        cache
            .insert(
                "a".into(),
                DecodedAudio {
                    samples,
                    sample_rate: 48_000,
                    channels: 1,
                },
            )
            .unwrap();

        assert_eq!(cache.stats().current_size, capacity_bytes);
    }

    #[test]
    fn eviction_is_least_recently_used_and_keeps_the_total_within_the_limit() {
        let entry = |value: f32| DecodedAudio {
            samples: vec![value; 8],
            sample_rate: 48_000,
            channels: 1,
        };
        let one_entry_bytes = 8 * std::mem::size_of::<f32>();

        let mut cache = AudioCache::with_max_size_and_aggregate(
            one_entry_bytes * 2,
            Arc::new(AudioAggregateLedger::new(MAX_AUDIO_CACHE_BYTES)),
        );
        cache.insert("a".into(), entry(1.0)).unwrap();
        cache.insert("b".into(), entry(2.0)).unwrap();
        // Touch "a" so "b" becomes the least recently used.
        assert!(cache.get("a").is_some());
        cache.insert("c".into(), entry(3.0)).unwrap();

        assert!(cache.get("b").is_none(), "the LRU entry must be evicted");
        assert!(cache.get("a").is_some());
        assert!(cache.get("c").is_some());
        assert!(cache.stats().current_size <= one_entry_bytes * 2);
    }
    #[test]
    fn eviction_keeps_aggregate_permit_until_last_cached_arc_drops() {
        super::retirement_tests::verify();
        let ledger = Arc::new(AudioAggregateLedger::new(16));
        let mut cache = AudioCache::with_max_size_and_aggregate(16, Arc::clone(&ledger));
        let cached = cache
            .insert(
                "held".into(),
                DecodedAudio {
                    samples: vec![0.0; 4],
                    sample_rate: 48_000,
                    channels: 1,
                },
            )
            .unwrap();
        assert_eq!(ledger.used_bytes(), 16);
        cache.remove("held");
        assert_eq!(ledger.used_bytes(), 16);
        drop(cached);
        assert_eq!(ledger.used_bytes(), 0);
    }
}

impl Default for AudioCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Cache statistics.
#[derive(Debug, Clone)]
pub struct CacheStats {
    pub entry_count: usize,
    pub current_size: usize,
    pub max_size: usize,
}

/// Thread-safe global cache wrapper.
pub struct GlobalAudioCache {
    inner: Mutex<AudioCache>,
    // Available before validation or lock acquisition, including poison.
    cpu: Option<migo_io::audio_cpu::AudioCpu>,
}

impl GlobalAudioCache {
    pub(crate) fn with_cpu(cpu: migo_io::audio_cpu::AudioCpu) -> Self {
        Self {
            inner: Mutex::new(AudioCache::with_cpu(cpu.clone())),
            cpu: Some(cpu),
        }
    }

    pub fn new() -> Self {
        Self {
            inner: Mutex::new(AudioCache::new()),
            cpu: None,
        }
    }

    pub fn with_max_size(max_size: usize) -> Self {
        Self {
            inner: Mutex::new(AudioCache::with_max_size(max_size)),
            cpu: None,
        }
    }

    pub fn get(&self, url: &str) -> Option<Arc<CachedAudio>> {
        self.inner.lock().ok()?.get(url)
    }

    pub fn insert(&self, url: String, audio: DecodedAudio) -> EngineResult<Arc<CachedAudio>> {
        let mut cache = self.inner.lock().map_err(|_| {
            EngineError::from_detail(ErrorCode::Internal, "audio cache lock poisoned")
        })?;
        cache.insert(url, audio)
    }

    pub(crate) fn insert_with_permit(
        &self,
        url: String,
        audio: DecodedAudio,
        permit: AudioAggregatePermit,
    ) -> EngineResult<Arc<CachedAudio>> {
        self.insert_with_source(url, audio, permit, None)
    }

    pub(crate) fn insert_with_source(
        &self,
        url: String,
        audio: DecodedAudio,
        permit: AudioAggregatePermit,
        timeline: Option<crate::streaming::StreamTimeline>,
    ) -> EngineResult<Arc<CachedAudio>> {
        // Protect the already charged input before either refusal boundary.
        let audio = CachedAudio::new(audio, permit, timeline, self.cpu.as_ref());
        if timeline.is_some_and(|t| {
            t.format() != (audio.sample_rate, audio.channels)
                || !audio.samples.len().is_multiple_of(audio.channels as usize)
                || t.output_frames() != audio.frame_count() as u64
        }) {
            return Err(EngineError::from_detail(
                ErrorCode::IoError,
                "cache timeline does not describe owned PCM",
            ));
        }
        let mut cache = self.inner.lock().map_err(|_| {
            EngineError::from_detail(ErrorCode::Internal, "audio cache lock poisoned")
        })?;
        Ok(cache.insert_ready(url, audio))
    }

    pub fn remove(&self, url: &str) {
        if let Ok(mut cache) = self.inner.lock() {
            cache.remove(url);
        }
    }

    pub fn clear(&self) {
        if let Ok(mut cache) = self.inner.lock() {
            cache.clear();
        }
    }

    pub fn stats(&self) -> Option<CacheStats> {
        self.inner.lock().ok().map(|c| c.stats())
    }
}

impl Default for GlobalAudioCache {
    fn default() -> Self {
        Self::new()
    }
}

#[path = "cache_retirement.rs"]
mod retirement;
#[cfg(test)]
#[path = "cache_retirement_tests.rs"]
mod retirement_tests;

#[cfg(test)]
#[path = "cache_rejection_tests.rs"]
mod rejection_tests;

#[cfg(test)]
pub(crate) fn observe_retirement<R>(hook: Arc<dyn Fn() + Send + Sync>, f: impl FnOnce() -> R) -> R {
    retirement::observe(hook, f)
}
