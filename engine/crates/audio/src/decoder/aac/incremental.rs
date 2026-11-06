//! Shared bounded push decoder. Format parsing/timing belong to the libraries;
//! this layer owns bounded input/output buffers and cooperative work limits.
use std::sync::atomic::{AtomicBool, Ordering};

use symphonia_core::{
    codecs::audio::{AudioCodecParameters, AudioDecoder},
    formats::Track,
};
use symphonia_core::{errors::Result as FormatResult, packet::PacketRef};

pub enum PacketStep<'a> {
    NeedMore,
    NeedSource,
    Discard(usize),
    Packet {
        consumed: usize,
        packet: PacketRef<'a>,
    },
    BufferedPacket {
        consumed: usize,
    },
    End,
}

pub trait Packetizer: Send + 'static {
    type Codec: AudioDecoder + Send + 'static;
    const OUTPUT_SAMPLES_PER_STEP: usize = SAMPLES_PER_STEP;
    const ENCODED_BYTES_PER_STEP: usize = WORK_BYTES_PER_STEP;
    const DISCARDS_PER_STEP: usize = usize::MAX;
    const SEEK_FROM_START: bool = false;
    // Container audio can end before its optional metadata. These bounds apply
    // to discarded bytes, independently of resident buffering and PCM duration.
    const MAX_TRAILING_BYTES: usize = 0;
    const MAX_DISCARDED_BYTES: Option<usize> = None;
    fn new() -> Self;
    fn track(&self) -> Option<&Track>;
    fn next<'a>(&mut self, input: &'a [u8], eof: bool) -> FormatResult<PacketStep<'a>>;

    fn buffered_packet<'a>(&'a self, _input: &'a [u8]) -> Option<PacketRef<'a>> {
        None
    }
    fn validate_codec(params: &AudioCodecParameters) -> Result<(), String>;
    fn create_decoder(params: &AudioCodecParameters) -> Result<Self::Codec, String>;
    fn seek(
        &mut self,
        _target: symphonia_core::units::Timestamp,
    ) -> FormatResult<symphonia_core::formats::SeekedTo> {
        Err(symphonia_core::errors::Error::SeekError(
            symphonia_core::errors::SeekErrorKind::Unseekable,
        ))
    }
    fn resume_buffered(&mut self, _input: &[u8], _head: usize) -> FormatResult<Option<usize>> {
        Ok(None)
    }
}

pub const ENCODED_CAPACITY: usize = 64 * 1024;
pub const PACKETS_PER_STEP: usize = 8;
pub const SAMPLES_PER_STEP: usize = PACKETS_PER_STEP * 1152 * 2;
const WORK_BYTES_PER_STEP: usize = 4096;

#[derive(Debug, PartialEq, Eq)]
pub enum Progress {
    NeedInput,
    NeedSource,
    Yield,
    Finished,
    Cancelled,
}

pub struct Decoder<P: Packetizer> {
    packets: P,
    decoder: Option<P::Codec>,
    packet_budget: usize,
    input: Vec<u8>,
    head: usize,
    eof: bool,
    finished: bool,
    cancelled: bool,
    failed: bool,
    total_samples: u64,
    cap: Option<u64>,
    source_waiting: bool,
    awaiting_eof: bool,
    trailing_bytes: usize,
    discarded_bytes: usize,
    seek_target: Option<symphonia_core::units::Timestamp>,
    retain_history: bool,
    pub rate: u32,
    pub channels: u32,
}

impl<P: Packetizer> Decoder<P> {
    #[cfg(test)]
    pub(crate) fn input_allocation(&self) -> (*const u8, usize) {
        (self.input.as_ptr(), self.input.capacity())
    }
    #[cfg(test)]
    pub(crate) fn input_len(&self) -> usize {
        self.input.len()
    }
    #[cfg(test)]
    pub(crate) fn discarded_bytes(&self) -> usize {
        self.discarded_bytes
    }
    pub(crate) fn presentation_track(&self) -> Option<&Track> {
        self.packets.track()
    }
    /// The optional total quota is separate from resident buffering. `None`
    /// allows long music without retaining its previous PCM in memory.
    pub fn new(cap: Option<u64>) -> Self {
        Self::with_packetizer(P::new(), cap)
    }

    pub(crate) fn with_packetizer(packets: P, cap: Option<u64>) -> Self {
        Self {
            packets,
            decoder: None,
            packet_budget: PACKETS_PER_STEP,
            input: Vec::with_capacity(ENCODED_CAPACITY),
            head: 0,
            eof: false,
            finished: false,
            cancelled: false,
            failed: false,
            total_samples: 0,
            cap,
            source_waiting: false,
            awaiting_eof: false,
            trailing_bytes: 0,
            discarded_bytes: 0,
            seek_target: None,
            retain_history: false,
            rate: 0,
            channels: 0,
        }
    }

    pub(crate) fn awaits_source_eof(&self) -> bool {
        self.awaiting_eof
    }
    pub(crate) fn awaits_source(&self) -> bool {
        self.source_waiting
    }
    pub(crate) fn retain_history(&mut self) {
        self.retain_history = true;
    }
    fn accept_tail(&mut self, bytes: usize) -> Result<(), String> {
        let total = self
            .trailing_bytes
            .checked_add(bytes)
            .ok_or("container tail byte overflow")?;
        if total > P::MAX_TRAILING_BYTES
            || P::MAX_DISCARDED_BYTES
                .is_some_and(|limit| total.saturating_add(self.discarded_bytes) > limit)
        {
            return Err("stream metadata byte budget exceeded".into());
        }
        self.trailing_bytes = total;
        Ok(())
    }
    /// Accept at most the remaining bounded input space. The caller drains and
    /// retries the unaccepted suffix; it never appends an unbounded HTTP chunk.
    pub fn push(&mut self, data: &[u8]) -> Result<usize, String> {
        if self.source_waiting {
            return Err("source request must be resolved before accepting input".into());
        }
        if self.eof || self.finished || self.cancelled || self.failed {
            return Err("stream no longer accepts encoded input".into());
        }
        if self.awaiting_eof && !data.is_empty() {
            let n = data.len().min(WORK_BYTES_PER_STEP);
            self.accept_tail(n)?;
            return Ok(n);
        }
        if data.is_empty() {
            return Ok(0);
        }
        if self.head != 0 && self.input.len() == ENCODED_CAPACITY {
            self.input.copy_within(self.head.., 0);
            self.input.truncate(self.input.len() - self.head);
            self.head = 0;
        }
        let accepted = data.len().min(ENCODED_CAPACITY - self.input.len());
        self.input.extend_from_slice(&data[..accepted]);
        Ok(accepted)
    }

    pub fn finish_input(&mut self) {
        self.eof = true;
    }

    pub(crate) fn seek(
        &mut self,
        target: symphonia_core::units::Timestamp,
    ) -> FormatResult<symphonia_core::formats::SeekedTo> {
        use symphonia_core::errors::Error;
        if self.failed || self.cancelled {
            return Err(Error::DecodeError("stream decoder is retired"));
        }
        let seeked = self.packets.seek(target)?;
        if let Some(decoder) = &mut self.decoder {
            decoder.reset();
        }
        self.seek_target = Some(seeked.required_ts);
        self.source_waiting = true;
        self.finished = false;
        self.awaiting_eof = false;
        self.total_samples = 0;
        self.trailing_bytes = 0;
        self.discarded_bytes = 0;
        if self.retain_history
            && let Some(head) = self.packets.resume_buffered(&self.input, self.head)?
        {
            self.head = head;
            self.source_waiting = false;
        }
        Ok(seeked)
    }

    pub(crate) fn source_packetizer(&self) -> Option<&P> {
        (self.source_waiting && !self.failed && !self.cancelled && !self.finished)
            .then_some(&self.packets)
    }

    pub(crate) fn resume_source(
        &mut self,
        reset_input: bool,
        resume: impl FnOnce(&mut P) -> Result<(), String>,
    ) -> Result<(), String> {
        let result = if self.source_packetizer().is_some() {
            resume(&mut self.packets)
        } else {
            Err("decoder has no active source request".into())
        };
        if result.is_err() {
            self.failed = true;
            return result;
        }
        if reset_input {
            // Encoded bytes belong to the previous read position. The codec
            // and PCM timeline stay live across an indexed source reposition.
            self.input.clear();
            self.head = 0;
            self.eof = false;
        }
        self.source_waiting = false;
        Ok(())
    }

    /// Runs off the async transport executor. One step has bounded packets,
    /// samples and scanning; no blocking Read and no per-stream OS thread.
    pub fn drain(&mut self, out: &mut Vec<f32>, cancel: &AtomicBool) -> Result<Progress, String> {
        if self.failed {
            return Err("stream decoder previously failed".into());
        }
        let result = self.drain_inner(out, cancel);
        if result.is_err() {
            self.failed = true;
            out.clear();
        }
        result
    }

    fn drain_inner(&mut self, out: &mut Vec<f32>, cancel: &AtomicBool) -> Result<Progress, String> {
        if !out.is_empty() || out.capacity() < P::OUTPUT_SAMPLES_PER_STEP {
            return Err("stream destination must be empty and preallocated".into());
        }
        if self.finished {
            return Ok(Progress::Finished);
        }
        if self.awaiting_eof {
            if self.eof {
                self.finished = true;
                return Ok(Progress::Finished);
            }
            return Ok(Progress::NeedInput);
        }
        let mut packets = 0;
        let mut bytes = 0;
        let mut scanned = 0;
        let mut discards = 0;
        loop {
            if self.cancelled || cancel.load(Ordering::Acquire) {
                self.cancelled = true;
                out.clear();
                return Ok(Progress::Cancelled);
            }
            if self.source_waiting {
                return Ok(Progress::NeedSource);
            }
            if packets >= self.packet_budget
                || bytes >= P::ENCODED_BYTES_PER_STEP
                || scanned >= WORK_BYTES_PER_STEP
                || discards >= P::DISCARDS_PER_STEP
            {
                return Ok(Progress::Yield);
            }
            let input = &self.input[self.head..];
            let (consumed, packet) = match self
                .packets
                .next(input, self.eof)
                .map_err(|e| format!("packet: {e}"))?
            {
                PacketStep::NeedMore => {
                    if self.eof || (self.head == 0 && self.input.len() == ENCODED_CAPACITY) {
                        return Err("packet exceeded its encoded window or ended incomplete".into());
                    }
                    return Ok(Progress::NeedInput);
                }
                PacketStep::NeedSource => {
                    self.source_waiting = true;
                    continue;
                }
                PacketStep::End => {
                    if self.total_samples == 0 {
                        return Err("stream contains no decoded samples".into());
                    }
                    // Containers may end their audio extent before HTTP EOF.
                    // Consume any non-audio tail within the same input bound,
                    // but let the transport confirm its own successful EOF.
                    if !self.eof {
                        let tail = input.len();
                        self.accept_tail(tail)?;
                        self.awaiting_eof = true;
                        if self.retain_history {
                            self.head = self.input.len();
                        } else {
                            self.input.clear();
                            self.head = 0;
                        }
                        return Ok(Progress::NeedInput);
                    }
                    self.finished = true;
                    return Ok(Progress::Finished);
                }
                PacketStep::Discard(consumed) => {
                    if consumed == 0 || consumed > input.len() {
                        return Err("invalid discarded byte count".into());
                    }
                    self.discarded_bytes = self
                        .discarded_bytes
                        .checked_add(consumed)
                        .ok_or("metadata byte overflow")?;
                    if P::MAX_DISCARDED_BYTES.is_some_and(|limit| self.discarded_bytes > limit) {
                        return Err("stream metadata byte budget exceeded".into());
                    }
                    self.head += consumed;
                    bytes += consumed;
                    scanned += consumed;
                    discards += 1;
                    if self.head == self.input.len() && !self.retain_history {
                        self.input.clear();
                        self.head = 0;
                    }
                    continue;
                }
                PacketStep::Packet { consumed, packet } => {
                    // The packet borrows directly from the bounded input. It
                    // remains live through decode_ref, before compaction/reuse.
                    // Some encapsulations (ADTS) borrow only the payload after
                    // their header; all packet bytes must still be in input.
                    debug_assert!(
                        packet.data.as_ptr().addr() >= input.as_ptr().addr()
                            && packet
                                .data
                                .as_ptr()
                                .addr()
                                .checked_add(packet.data.len())
                                .is_some_and(|end| end <= input.as_ptr().addr() + consumed)
                    );
                    if consumed == 0 || consumed > input.len() {
                        return Err("invalid packet byte count".into());
                    }
                    (consumed, packet)
                }
                PacketStep::BufferedPacket { consumed } => {
                    if consumed > input.len() {
                        return Err("invalid buffered packet byte count".into());
                    }
                    let packet = self
                        .packets
                        .buffered_packet(input)
                        .ok_or("missing buffered packet")?;
                    (consumed, packet)
                }
            };
            if self.decoder.is_none() {
                let track = self.packets.track().expect("packet has track");
                let params = validate_track::<P>(track, self.cap)?;
                self.rate = params.sample_rate.expect("validated rate");
                self.channels = params
                    .channels
                    .as_ref()
                    .expect("validated channels")
                    .count() as u32;
                // Bound native scratch before constructing the typed codec.
                let max_samples = params
                    .max_frames_per_packet
                    .filter(|n| *n != 0)
                    .and_then(|n| n.checked_mul(self.channels as u64))
                    .and_then(|n| usize::try_from(n).ok())
                    .filter(|n| *n <= P::OUTPUT_SAMPLES_PER_STEP)
                    .ok_or("decoded packet exceeds step scratch budget")?;
                self.packet_budget = PACKETS_PER_STEP.min(P::OUTPUT_SAMPLES_PER_STEP / max_samples);
                self.decoder = Some(P::create_decoder(params)?);
            }
            let block = self
                .decoder
                .as_mut()
                .expect("initialized decoder")
                .decode_ref(&packet)
                .map_err(|e| format!("decode: {e}"))?;
            if block.spec().rate() != self.rate
                || block.spec().channels().count() != self.channels as usize
            {
                return Err("stream format changed".into());
            }
            let skip = if let Some(target) = self.seek_target {
                let start = packet
                    .pts
                    .get()
                    .checked_add(
                        i64::try_from(packet.trim_start.get())
                            .map_err(|_| "invalid packet trim")?,
                    )
                    .ok_or("packet presentation timestamp overflow")?;
                let skip = usize::try_from(target.get().saturating_sub(start).max(0))
                    .unwrap_or(usize::MAX)
                    .min(block.frames());
                if skip < block.frames() {
                    self.seek_target = None;
                }
                skip
            } else {
                0
            };
            let block = block.slice(skip..);
            let additional = block.samples_interleaved();
            let total = self
                .total_samples
                .checked_add(additional as u64)
                .ok_or("PCM length overflow")?;
            if self.cap.is_some_and(|cap| total > cap) {
                return Err("decoded stream exceeds PCM budget".into());
            }
            let start = out.len();
            let end = start
                .checked_add(additional)
                .filter(|end| *end <= P::OUTPUT_SAMPLES_PER_STEP)
                .ok_or("decoded packet exceeds step scratch budget")?;
            out.resize(end, 0.0);
            block.copy_to_slice_interleaved(&mut out[start..]);
            self.total_samples = total;
            self.head += consumed;
            bytes += consumed;
            packets += 1;

            if self.head == self.input.len() && !self.retain_history {
                self.input.clear();
                self.head = 0;
            }
        }
    }
}

fn validate_track<P: Packetizer>(
    track: &Track,
    cap: Option<u64>,
) -> Result<&symphonia_core::codecs::audio::AudioCodecParameters, String> {
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or("no audio parameters")?;
    P::validate_codec(params)?;
    let rate = params.sample_rate.ok_or("unknown stream sample rate")?;
    let channels = params
        .channels
        .as_ref()
        .ok_or("unknown stream channel count")?
        .count();
    if !(crate::limits::MIN_SAMPLE_RATE..=crate::limits::MAX_SAMPLE_RATE).contains(&rate)
        || !(1..=crate::limits::MAX_AUDIO_CHANNELS as usize).contains(&channels)
    {
        return Err("stream format outside Migo limits".into());
    }
    if track
        .num_frames
        .zip(cap)
        .is_some_and(|(frames, cap)| frames.checked_mul(channels as u64).is_none_or(|n| n > cap))
    {
        return Err("declared stream exceeds PCM budget".into());
    }
    Ok(params)
}
