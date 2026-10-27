//! Isolated W09 candidate, never linked into a shipped product by this probe.
//! Use Symphonia's demuxing, sample conversion and gapless support. Keep Migo's
//! allocation/format boundaries outside the codec; no parallel container parser.
use std::io::Cursor;

use symphonia::core::{
    codecs::audio::AudioDecoderOptions,
    common::Limit,
    formats::{FormatOptions, TrackType, probe::Hint},
    io::{MediaSourceStream, ReadOnlySource},
    meta::MetadataOptions,
};

use crate::decoder::DecodedAudio;

pub fn decode(data: &[u8], cap: usize) -> Result<DecodedAudio, String> {
    decode_source(data, cap, true)
}

pub fn decode_source(data: &[u8], cap: usize, seekable: bool) -> Result<DecodedAudio, String> {
    decode_source_impl(data, cap, seekable, None)
}

#[cfg(test)]
pub fn decode_retaining_metadata(data: &[u8], cap: usize) -> Result<DecodedAudio, String> {
    decode_source_impl(data, cap, true, Some(MetadataOptions::default()))
}

fn decode_source_impl(
    data: &[u8],
    cap: usize,
    seekable: bool,
    metadata_override: Option<MetadataOptions>,
) -> Result<DecodedAudio, String> {
    #[cfg(feature = "phase-probe")]
    let mut phases = profiling::Phases::new();
    if data.len() > 16 * 1024 * 1024 {
        return Err("encoded audio exceeds 16MiB".into());
    }
    let cap = cap.min(crate::limits::MAX_AUDIO_PCM_SAMPLES as usize);
    let source = if seekable {
        MediaSourceStream::new(Box::new(Cursor::new(data)), Default::default())
    } else {
        MediaSourceStream::new(
            Box::new(ReadOnlySource::new(Cursor::new(data))),
            Default::default(),
        )
    };
    let metadata = MetadataOptions::default()
        .limit_tag_bytes(Limit::Maximum(64 * 1024))
        .limit_visual_bytes(Limit::Maximum(0));
    // Migo consumes audio, not optional ID3 tags or chapters. This uses the
    // library's skip path, preserving valid media after oversized metadata.
    // Other containers' option propagation is validated separately.
    #[cfg(feature = "metadata-budgets")]
    let metadata = metadata
        .limit_total_bytes(Limit::Maximum(0))
        .limit_frame_bytes(Limit::Maximum(64 * 1024))
        .limit_nesting_depth(Limit::Maximum(2));
    let metadata = metadata_override.unwrap_or(metadata);
    #[cfg(feature = "phase-probe")]
    phases.mark("source_and_options");
    let probe = symphonia::default::get_probe();
    #[cfg(feature = "phase-probe")]
    phases.mark("probe_registry");
    let mut format = probe
        .probe(&Hint::new(), source, FormatOptions::default(), metadata)
        .map_err(|error| format!("probe: {error}"))?;
    #[cfg(feature = "phase-probe")]
    phases.mark("format_probe");
    let track = format
        .default_track(TrackType::Audio)
        .ok_or("no audio track")?;
    let track_id = track.id;
    let parameters = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or("no audio parameters")?;
    if let Some(rate) = parameters.sample_rate {
        validate_rate(rate)?;
    }
    let channels = parameters.channels.as_ref().map(|c| c.count());
    if let Some(channels) = channels {
        validate_channels(channels)?;
        // num_frames describes valid output; a codec block can be larger after
        // priming/padding are trimmed (for example a 17-frame AAC sound effect).
        // Check its scratch estimate against the global budget independently.
        for (frames, budget) in [
            (track.num_frames, cap as u64),
            (
                parameters.max_frames_per_packet,
                crate::limits::MAX_AUDIO_PCM_SAMPLES,
            ),
        ] {
            let Some(frames) = frames else { continue };
            if frames
                .checked_mul(channels as u64)
                .is_none_or(|n| n > budget)
            {
                return Err("declared audio exceeds PCM budget".into());
            }
        }
    }
    let initial_pcm_samples = match (track.num_frames, channels) {
        (Some(frames), Some(channels)) => frames
            .checked_mul(channels as u64)
            .and_then(|samples| usize::try_from(samples).ok())
            .ok_or("declared audio exceeds PCM budget")?,
        _ => 0,
    };
    #[cfg(feature = "phase-probe")]
    phases.mark("track_validation");
    let registry = symphonia::default::get_codecs();
    #[cfg(feature = "phase-probe")]
    phases.mark("codec_registry");
    let mut decoder = registry
        .make_audio_decoder(parameters, &AudioDecoderOptions::default().gapless(true))
        .map_err(|error| format!("decoder: {error}"))?;
    #[cfg(feature = "phase-probe")]
    phases.mark("decoder_construction");
    let mut output = DecodedAudio {
        samples: Vec::new(),
        sample_rate: 0,
        channels: 0,
    };
    // Reuse the demuxer's bounded frame estimate instead of repeatedly moving
    // whole PCM buffers. Unknown-length sources retain bounded geometric growth.
    output
        .samples
        .try_reserve_exact(initial_pcm_samples)
        .map_err(|_| "PCM allocation refused")?;
    #[cfg(feature = "phase-probe")]
    phases.mark("pcm_reservation");
    while let Some(packet) = format
        .next_packet()
        .map_err(|error| format!("packet: {error}"))?
    {
        if packet.track_id != track_id {
            continue;
        }
        // Do not turn corruption or unsupported profiles into successful
        // truncated audio; classify/reset/recovery policy is still under review.
        let block = decoder
            .decode(&packet)
            .map_err(|error| format!("decode: {error}"))?;
        let rate = block.spec().rate();
        let channels = block.spec().channels().count();
        validate_rate(rate)?;
        validate_channels(channels)?;
        if output.sample_rate != 0
            && (output.sample_rate != rate || output.channels as usize != channels)
        {
            return Err("audio format changed within one buffer".into());
        }
        output.sample_rate = rate;
        output.channels = channels as u32;
        let additional = block.samples_interleaved();
        let end = output
            .samples
            .len()
            .checked_add(additional)
            .ok_or("PCM length overflow")?;
        if end > cap {
            return Err("decoded audio exceeds PCM budget".into());
        }
        if end > output.samples.capacity() {
            // Bound allocated capacity as well as length. Unrestricted Vec
            // growth may reserve beyond the PCM quota near its final block.
            let target = output
                .samples
                .capacity()
                .saturating_mul(2)
                .max(end)
                .min(cap);
            output
                .samples
                .try_reserve_exact(target - output.samples.len())
                .map_err(|_| "PCM allocation refused")?;
        }
        let start = output.samples.len();
        output.samples.resize(end, 0.0);
        block.copy_to_slice_interleaved(&mut output.samples[start..]);
    }
    if output.samples.is_empty() {
        return Err("no decoded samples".into());
    }
    #[cfg(feature = "phase-probe")]
    {
        phases.mark("packet_decode_convert_append");
        phases.emit();
    }
    Ok(output)
}

// Only compiled into the isolated diagnostic binary. Fixed storage avoids
// measuring profile-vector allocation as codec initialization; JSON emission
// happens after every phase has been captured. Not used for performance claims.
#[cfg(feature = "phase-probe")]
mod profiling {
    use std::time::Instant;

    pub struct Phases {
        last: Instant,
        last_cpu: Option<u64>,
        stages: [(&'static str, u64, Option<u64>); 8],
        len: usize,
    }

    impl Phases {
        pub fn new() -> Self {
            Self {
                last: Instant::now(),
                last_cpu: thread_cpu_ns(),
                stages: [("", 0, None); 8],
                len: 0,
            }
        }

        pub fn mark(&mut self, name: &'static str) {
            let now = Instant::now();
            let cpu = thread_cpu_ns();
            self.stages[self.len] = (
                name,
                now.duration_since(self.last).as_nanos() as u64,
                cpu.zip(self.last_cpu).map(|(now, last)| now - last),
            );
            self.len += 1;
            self.last = now;
            self.last_cpu = cpu;
        }

        pub fn emit(&self) {
            eprintln!("W09_PHASES {}", serde_json::json!(&self.stages[..self.len]));
        }
    }

    fn thread_cpu_ns() -> Option<u64> {
        #[cfg(target_os = "linux")]
        {
            let mut time = std::mem::MaybeUninit::<libc::timespec>::uninit();
            // SAFETY: the clock writes one valid timespec on success. The output
            // is initialized/read only when clock_gettime reports success.
            if unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, time.as_mut_ptr()) } != 0
            {
                return None;
            }
            let time = unsafe { time.assume_init() };
            u64::try_from(time.tv_sec)
                .ok()?
                .checked_mul(1_000_000_000)?
                .checked_add(u64::try_from(time.tv_nsec).ok()?)
        }
        #[cfg(not(target_os = "linux"))]
        None
    }
}

fn validate_channels(channels: usize) -> Result<(), String> {
    if channels == 0 || channels > crate::limits::MAX_AUDIO_CHANNELS as usize {
        return Err("channel count outside Migo limits".into());
    }
    Ok(())
}

fn validate_rate(rate: u32) -> Result<(), String> {
    if !(crate::limits::MIN_SAMPLE_RATE..=crate::limits::MAX_SAMPLE_RATE).contains(&rate) {
        return Err("sample rate outside Migo limits".into());
    }
    Ok(())
}
