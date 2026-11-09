//! Playable PCM pulled from upstream format readers and codecs.
//!
//! One implementation serves WAVE, Ogg/Vorbis, ADTS and MP4/AAC: they differ
//! only in which reader opens the container and which codec decodes its track.
//! Output is interleaved f32 at the source rate, trimmed to the presentation
//! (encoder delay and padding removed) and positioned frame-exactly by `seek`.
use shared::error::{EngineError, EngineResult, ErrorCode};
use symphonia_core::codecs::CodecParameters;
use symphonia_core::codecs::audio::{AudioDecoder, AudioDecoderOptions, well_known as codecs};
use symphonia_core::common::Limit;
use symphonia_core::errors::Error as SymphoniaError;
use symphonia_core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia_core::io::{MediaSource, MediaSourceStream, MediaSourceStreamOptions};
use symphonia_core::meta::MetadataOptions;
use symphonia_core::units::{TimeBase, Timestamp};

/// Decoded PCM a stream producer pulls on its own thread.
pub(crate) trait PcmSource: Send {
    fn rate(&self) -> u32;
    fn channels(&self) -> u32;
    /// Playable frames after encoder delay and padding, when the container
    /// states them.
    fn frames(&self) -> Option<u64>;
    /// Append at least one frame of interleaved PCM to `out`; `false` at the
    /// end of the presentation.
    fn read(&mut self, out: &mut Vec<f32>) -> EngineResult<bool>;
    /// Make the next `read` start exactly at playable frame `frame`.
    fn seek(&mut self, frame: u64) -> EngineResult<()>;
}

/// Bytes enough to tell the formats apart (RIFF/OggS/ftyp/ADTS/ID3/MPEG sync).
pub(crate) const SNIFF_BYTES: usize = 16;

/// Open the decoder the first bytes name. ADTS is checked before MPEG audio,
/// whose sync word it shares.
pub(crate) fn open(
    source: Box<dyn MediaSource>,
    prefix: &[u8],
) -> EngineResult<Box<dyn PcmSource>> {
    if let Some(container) = Container::sniff(prefix) {
        return Ok(Box::new(Symphonia::open(source, container)?));
    }
    if crate::decoder::is_mp3(prefix) {
        return Ok(Box::new(crate::mp3_source::Minimp3::open(source)?));
    }
    Err(EngineError::from_detail(
        ErrorCode::InvalidArgument,
        "not a supported audio format",
    ))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Container {
    Wave,
    Ogg,
    Adts,
    Mp4,
}

impl Container {
    /// The container a byte prefix opens; MP3 is not one of these.
    pub(crate) fn sniff(prefix: &[u8]) -> Option<Self> {
        if prefix.starts_with(b"RIFF") {
            Some(Self::Wave)
        } else if prefix.starts_with(b"OggS") {
            Some(Self::Ogg)
        } else if crate::decoder::is_m4a(prefix) {
            Some(Self::Mp4)
        } else if crate::decoder::is_adts(prefix) {
            Some(Self::Adts)
        } else {
            None
        }
    }
}

/// Decoders resynchronize after a seek on the frames before the target; this
/// many are decoded and dropped so the first published frame is exact. It
/// covers AAC's overlap-add (one 1024-frame window) and Vorbis' long block
/// with margin; PCM needs none but pays only a cheap copy.
const SEEK_PREROLL: u64 = 4096;
/// Corrupt packets tolerated per stream before the stream fails.
const MAX_BAD_PACKETS: u32 = 16;

/// A container the sniffer recognised but whose reader refuses it is not a
/// playable input (IO failures and cancellation keep their own codes).
fn open_error(error: SymphoniaError) -> EngineError {
    match error {
        SymphoniaError::IoError(_) => media_error(error),
        other => EngineError::from_detail(
            ErrorCode::InvalidArgument,
            format!("audio container: {other}"),
        ),
    }
}

fn invalid(detail: impl Into<String>) -> EngineError {
    EngineError::from_detail(ErrorCode::IoError, detail.into())
}

fn media_error(error: SymphoniaError) -> EngineError {
    match error {
        SymphoniaError::IoError(e) if e.kind() == std::io::ErrorKind::Interrupted => {
            EngineError::from_detail(ErrorCode::Cancelled, "audio source cancelled")
        }
        SymphoniaError::Unsupported(what) => {
            EngineError::from_detail(ErrorCode::Unsupported, format!("audio format: {what}"))
        }
        other => invalid(format!("audio decode: {other}")),
    }
}

/// Tags are parsed for the reader's sake only; no artwork or large text is kept.
fn format_options() -> FormatOptions {
    let mut options = FormatOptions::default();
    options.metadata_options = MetadataOptions::default()
        .limit_tag_bytes(Limit::Maximum(64 * 1024))
        .limit_visual_bytes(Limit::Maximum(0));
    options
}

/// The WAVE `data` chunk's offset and declared size, read from chunk headers
/// before the reader takes the source (the reader does not expose them).
fn wave_data_extent(source: &mut dyn MediaSource) -> std::io::Result<Option<(u64, u32)>> {
    use std::io::SeekFrom;
    let mut head = [0u8; 12];
    source.read_exact(&mut head)?;
    let mut pos = 12u64;
    let mut found = None;
    for _ in 0..64 {
        let mut chunk = [0u8; 8];
        if source.read_exact(&mut chunk).is_err() {
            break;
        }
        let size = u32::from_le_bytes(chunk[4..8].try_into().unwrap());
        if &chunk[..4] == b"data" {
            found = Some((pos + 8, size));
            break;
        }
        pos += 8 + u64::from(size) + u64::from(size & 1);
        source.seek(SeekFrom::Start(pos))?;
    }
    source.seek(SeekFrom::Start(0))?;
    Ok(found)
}

pub(crate) struct Symphonia {
    reader: Box<dyn FormatReader + 'static>,
    decoder: Box<dyn AudioDecoder>,
    track: u32,
    rate: u32,
    channels: u32,
    frames: Option<u64>,
    time_base: TimeBase,
    /// Encoder delay and first timestamp, only to keep seek requests in range.
    delay: u32,
    start_ts: i64,
    /// The next playable frame owed to the caller.
    position: u64,
    scratch: Vec<f32>,
    bad_packets: u32,
}

impl Symphonia {
    pub(crate) fn open(
        mut source: Box<dyn MediaSource>,
        container: Container,
    ) -> EngineResult<Self> {
        // A WAVE states its data size; streamed writers put the "unknown"
        // maximum there, and a data chunk past the object is a truncation.
        let mut wave_unknown_length = false;
        if container == Container::Wave
            && let Some(len) = source.byte_len()
        {
            let extent = wave_data_extent(source.as_mut())
                .map_err(|e| media_error(SymphoniaError::IoError(e)))?;
            if let Some((offset, size)) = extent {
                if size >= u32::MAX - 1 {
                    wave_unknown_length = true;
                } else if offset + u64::from(size) > len {
                    return Err(EngineError::from_detail(
                        ErrorCode::InvalidArgument,
                        "WAVE data extends beyond the object",
                    ));
                }
            }
        }
        let mss = MediaSourceStream::new(source, MediaSourceStreamOptions::default());
        let options = format_options();
        let reader: Box<dyn FormatReader + 'static> = match container {
            Container::Wave => Box::new(
                symphonia_format_riff::WavReader::try_new(mss, options).map_err(open_error)?,
            ),
            Container::Ogg => Box::new(
                symphonia_format_ogg::OggReader::try_new(mss, options).map_err(open_error)?,
            ),
            Container::Adts => Box::new(
                symphonia_codec_aac::AdtsReader::try_new(mss, options).map_err(open_error)?,
            ),
            Container::Mp4 => Box::new(
                symphonia_format_isomp4::IsoMp4Reader::try_new(mss, options).map_err(open_error)?,
            ),
        };
        let track = reader
            .default_track(TrackType::Audio)
            .ok_or_else(|| invalid("no audio track"))?;
        let Some(CodecParameters::Audio(params)) = track.codec_params.as_ref() else {
            return Err(invalid("audio track has no codec parameters"));
        };
        let opts = AudioDecoderOptions::default().gapless(true);
        let decoder: Box<dyn AudioDecoder> = match params.codec {
            codecs::CODEC_ID_VORBIS => Box::new(
                symphonia_codec_vorbis::VorbisDecoder::try_new(params, &opts)
                    .map_err(media_error)?,
            ),
            codecs::CODEC_ID_AAC => Box::new(
                symphonia_codec_aac::AacDecoder::try_new(params, &opts).map_err(media_error)?,
            ),
            _ if container == Container::Wave => Box::new(
                symphonia_codec_pcm::PcmDecoder::try_new(params, &opts).map_err(media_error)?,
            ),
            other => {
                return Err(EngineError::from_detail(
                    ErrorCode::Unsupported,
                    format!("audio codec {other:?} is not supported"),
                ));
            }
        };
        let rate = params
            .sample_rate
            .filter(|r| {
                (crate::limits::MIN_SAMPLE_RATE..=crate::limits::MAX_SAMPLE_RATE).contains(r)
            })
            .ok_or_else(|| invalid("unsupported sample rate"))?;
        let channels = params
            .channels
            .as_ref()
            .map(|c| c.count() as u32)
            .filter(|c| (1..=crate::limits::MAX_AUDIO_CHANNELS).contains(c))
            .ok_or_else(|| invalid("unsupported channel layout"))?;
        let time_base = track
            .time_base
            .or_else(|| TimeBase::try_from_recip(rate))
            .ok_or_else(|| invalid("audio track has no time base"))?;
        let frames = track.num_frames.filter(|n| *n > 0 && !wave_unknown_length);
        let (delay, start_ts) = (track.delay.unwrap_or(0), track.start_ts.get());
        let track = track.id;
        Ok(Self {
            reader,
            decoder,
            track,
            rate,
            channels,
            frames,
            time_base,
            delay,
            start_ts,
            position: 0,
            scratch: Vec::new(),
            bad_packets: 0,
        })
    }

    /// Track time-base units as frames at the decoded rate.
    fn to_frames(&self, units: u64) -> u64 {
        let (numer, denom) = (self.time_base.numer.get(), self.time_base.denom.get());
        if numer == 1 && denom == self.rate {
            return units;
        }
        let scaled = u128::from(units) * u128::from(numer) * u128::from(self.rate);
        (scaled / u128::from(denom)) as u64
    }

    fn to_units(&self, frames: u64) -> i64 {
        let (numer, denom) = (self.time_base.numer.get(), self.time_base.denom.get());
        if numer == 1 && denom == self.rate {
            return frames as i64;
        }
        let scaled =
            u128::from(frames) * u128::from(denom) / (u128::from(numer) * u128::from(self.rate));
        scaled.min(i64::MAX as u128) as i64
    }
}

impl PcmSource for Symphonia {
    fn rate(&self) -> u32 {
        self.rate
    }
    fn channels(&self) -> u32 {
        self.channels
    }
    fn frames(&self) -> Option<u64> {
        self.frames
    }

    fn read(&mut self, out: &mut Vec<f32>) -> EngineResult<bool> {
        let channels = self.channels as usize;
        loop {
            if self.frames.is_some_and(|n| self.position >= n) {
                return Ok(false);
            }
            let packet = match self.reader.next_packet() {
                Ok(Some(packet)) => packet,
                Ok(None) => return Ok(false),
                // Without a stated length the object's end is the stream's end.
                Err(SymphoniaError::IoError(e))
                    if self.frames.is_none() && e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    return Ok(false);
                }
                Err(error) => return Err(media_error(error)),
            };
            if packet.track_id != self.track {
                continue;
            }
            let block = match self.decoder.decode(&packet) {
                Ok(block) => block,
                Err(SymphoniaError::DecodeError(_)) if self.bad_packets < MAX_BAD_PACKETS => {
                    // Upstream contract: an undecodable packet is discarded.
                    self.bad_packets += 1;
                    continue;
                }
                Err(error) => return Err(media_error(error)),
            };
            if block.spec().channels().count() != channels || block.spec().rate() != self.rate {
                return Err(invalid("audio format changed mid-stream"));
            }
            let decoded = block.frames() as u64;
            self.scratch.resize(block.samples_interleaved(), 0.0);
            block.copy_to_slice_interleaved(&mut self.scratch);
            // The packet's valid frames, placed on the presentation clock by its
            // own timestamp. Codecs that already trimmed (Vorbis, gapless) decode
            // exactly `dur`; others (AAC, PCM) leave delay/padding to us. A codec
            // still priming after a reset decodes fewer: those are the span's end.
            let span = self.to_frames(packet.dur.get());
            let valid = span.min(decoded);
            let front = self.to_frames(packet.trim_start.get()).min(decoded - valid);
            // Readers time packets on the presentation clock: the first frame
            // left after trimming the front is at `pts + trim_start` (Ogg and
            // MP4 give the delayed lead-in a negative pts).
            let lead = packet
                .pts
                .get()
                .saturating_add(packet.trim_start.get() as i64)
                .max(0) as u64;
            let start = self.to_frames(lead) + (span - valid);
            let (mut first, mut last) = (front, front + valid);
            // Drop frames before the next one owed (an exact seek's target).
            if start < self.position {
                first += (self.position - start).min(valid);
            }
            let at = start.max(self.position);
            // Never publish past a stated presentation end.
            if let Some(n) = self.frames {
                last = last.min(first + n.saturating_sub(at));
            }
            if last <= first {
                continue;
            }
            out.extend_from_slice(
                &self.scratch[first as usize * channels..last as usize * channels],
            );
            self.position = at + (last - first);
            return Ok(true);
        }
    }

    fn seek(&mut self, frame: u64) -> EngineResult<()> {
        if self.frames.is_some_and(|n| frame > n) {
            return Err(invalid("seek beyond the presentation"));
        }
        // Ask for an earlier point than the target: `read` drops what precedes
        // it by packet time, so landing early costs only decoding. The encoder
        // delay is subtracted too because upstream Ogg checks the upper bound as
        // `start_ts + num_frames` with a start_ts of -delay, refusing the last
        // `delay` frames of a valid presentation otherwise.
        let request = self.to_units(frame.saturating_sub(SEEK_PREROLL)) - i64::from(self.delay);
        let seeked = self
            .reader
            .seek(
                SeekMode::Accurate,
                SeekTo::Timestamp {
                    ts: Timestamp::new(request.max(self.start_ts)),
                    track_id: self.track,
                },
            )
            .map_err(media_error)?;
        self.decoder.reset();
        // Packets carry their own presentation times; `read` drops those before
        // the target, whatever packet the reader landed on.
        let _ = seeked;
        self.position = frame;
        Ok(())
    }
}

#[cfg(test)]
#[path = "pcm_source_tests.rs"]
mod tests;
