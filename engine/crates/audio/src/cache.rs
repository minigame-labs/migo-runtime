//! Audio buffer cache with LRU eviction
//!
//! Caches decoded audio data to avoid repeated downloads and decoding.
//! Uses URL as key and applies LRU eviction when memory limit is exceeded.

use std::ops::Deref;
use std::sync::{Arc, Mutex};

use shared::error::{EngineError, EngineResult, ErrorCode};

use crate::decoder::DecodedAudio;
#[path = "cache_key_storage.rs"]
mod key_storage;
use crate::limits::{AudioAggregateLedger, AudioAggregatePermit, MAX_AUDIO_CACHE_BYTES, pcm_bytes};
use key_storage::Key;
#[path = "cache_entries_storage.rs"]
mod entries_storage;
use entries_storage::Entries;
#[path = "cache_lru.rs"]
mod lru;

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
    links: lru::Links,
}

/// Global audio cache with LRU eviction.
pub struct AudioCache {
    entries: Entries,
    order_index: lru::Lru,
    current_size: usize,
    max_size: usize,
    access_counter: u64,
    aggregate: Arc<AudioAggregateLedger>,
    cpu: Option<migo_io::audio_cpu::AudioCpu>,
    metadata_host: Option<crate::node_retirement::Host>,
}

impl AudioCache {
    pub fn new() -> Self {
        Self::with_max_size_and_aggregate(
            MAX_AUDIO_CACHE_BYTES,
            AudioAggregateLedger::process_global(),
        )
    }

    #[cfg(test)]
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
        Self::with_parts(max_size, aggregate, None, None)
    }

    fn with_parts(
        max_size: usize,
        aggregate: Arc<AudioAggregateLedger>,
        cpu: Option<migo_io::audio_cpu::AudioCpu>,
        metadata_host: Option<crate::node_retirement::Host>,
    ) -> Self {
        Self {
            // Managed construction is lazy: no buckets precede admission.
            entries: if metadata_host.is_some() {
                Entries::new()
            } else {
                Entries::with_capacity(Self::DEFAULT_CACHE_CAPACITY)
            },
            order_index: lru::Lru::default(),
            current_size: 0,
            max_size,
            access_counter: 0,
            aggregate,
            cpu,
            metadata_host,
        }
    }

    pub fn get(&mut self, url: &str) -> Option<Arc<CachedAudio>> {
        if let Some(audio) = self.entries.get(url).map(|entry| Arc::clone(&entry.audio)) {
            self.order_index.touch(&mut self.entries, url);
            // Recency is carried by links; this diagnostic tag may wrap.
            self.access_counter = self.access_counter.wrapping_add(1);
            let entry = self.entries.get_mut(url).unwrap();
            entry.access_order = self.access_counter;
            tracing::trace!(cache_tag = entry.access_order, "Cache hit: {}", url);
            Some(audio)
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
        if size_bytes > self.max_size {
            tracing::debug!(
                "Audio too large to cache: {} bytes (max: {})",
                size_bytes,
                self.max_size
            );
            return Arc::new(audio);
        }

        // Caching is optional. Refusal preserves the old cache and returns the
        // already protected PCM without allocating a retained key.
        let growth = match self.entries.reserve(self.metadata_host.as_ref()) {
            Ok(growth) => growth,
            Err(_) => return Arc::new(audio),
        };
        let url = match Key::new(url, self.metadata_host.as_ref()) {
            Ok(key) => key,
            Err(_) => return Arc::new(audio),
        };
        if let Some(growth) = growth {
            growth.install(&mut self.entries);
        }
        while self.current_size + size_bytes > self.max_size && !self.entries.is_empty() {
            self.evict_lru();
        }
        self.access_counter = self.access_counter.wrapping_add(1);
        let audio = Arc::new(audio);
        self.order_index.remove(&mut self.entries, url.as_str());
        if let Some(old) = self.entries.remove(url.as_str()) {
            self.current_size = self.current_size.saturating_sub(old.size_bytes);
        }
        self.entries.insert(
            url.clone(),
            CacheEntry {
                audio: Arc::clone(&audio),
                size_bytes,
                access_order: self.access_counter,
                links: lru::Links::default(),
            },
        );
        self.order_index.append(&mut self.entries, url.clone());
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
        if let Some(url) = self.order_index.pop(&mut self.entries)
            && let Some(entry) = self.entries.remove(url.as_str())
        {
            self.current_size = self.current_size.saturating_sub(entry.size_bytes);
            tracing::debug!("Evicted from cache: {} ({} bytes)", url, entry.size_bytes);
        }
    }

    pub fn remove(&mut self, url: &str) {
        self.order_index.remove(&mut self.entries, url);
        if let Some(entry) = self.entries.remove(url) {
            self.current_size = self.current_size.saturating_sub(entry.size_bytes);
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
    #[cfg(test)]
    fn bucket_bytes(&self) -> usize {
        self.entries.allocation_bytes()
    }
    #[cfg(test)]
    fn observe_buckets(
        &mut self,
        hook: Arc<dyn Fn() + Send + Sync>,
    ) -> Arc<std::sync::atomic::AtomicUsize> {
        let freed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        self.entries.watch_deallocation(freed.clone());
        self.entries.observe(hook);
        freed
    }
    #[cfg(test)]
    fn check_bucket_host(&self, host: &crate::node_retirement::Host) -> EngineResult<()> {
        self.entries.reserve(Some(host)).map(|_| ())
    }
    #[cfg(test)]
    fn lru_keys(&self) -> impl Iterator<Item = &Key> {
        self.order_index.keys(&self.entries)
    }
    #[cfg(test)]
    fn lru_iter(&self) -> impl Iterator<Item = (&u64, &Key)> {
        self.lru_keys()
            .map(|key| (&self.entries.get(key.as_str()).unwrap().access_order, key))
    }
    #[cfg(test)]
    fn lru_empty(&self) -> bool {
        self.order_index.is_empty()
    }
    #[cfg(test)]
    fn lru_valid(&self) -> bool {
        self.order_index.valid(&self.entries)
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
    pub(crate) fn managed(
        cpu: migo_io::audio_cpu::AudioCpu,
        storage: crate::node_retirement::Host,
    ) -> Self {
        Self {
            inner: Mutex::new(AudioCache::with_parts(
                MAX_AUDIO_CACHE_BYTES,
                AudioAggregateLedger::process_global(),
                Some(cpu.clone()),
                Some(storage),
            )),
            cpu: Some(cpu),
        }
    }
    #[cfg(test)]
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
