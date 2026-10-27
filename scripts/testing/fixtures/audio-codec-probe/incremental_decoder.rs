//! Shared bounded push decoder. Format parsing/timing belong to the libraries;
//! this layer owns bounded input/output buffers and cooperative work limits.
use std::sync::atomic::{AtomicBool, Ordering};

use symphonia::core::{
    codecs::audio::{AudioCodecParameters, AudioDecoder},
    formats::Track,
};
use symphonia::core::{errors::Result as FormatResult, packet::PacketRef};

pub enum PacketStep<'a> {
    NeedMore,
    #[cfg(feature = "m4a-streaming-probe")]
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
    fn new() -> Self;
    fn track(&self) -> Option<&Track>;
    fn next<'a>(&mut self, input: &'a [u8], eof: bool) -> FormatResult<PacketStep<'a>>;
    fn buffered_packet<'a>(&'a self, _input: &'a [u8]) -> Option<PacketRef<'a>> {
        None
    }
    fn validate_codec(params: &AudioCodecParameters) -> Result<(), String>;
    fn create_decoder(params: &AudioCodecParameters) -> Result<Self::Codec, String>;
}

pub const ENCODED_CAPACITY: usize = 64 * 1024;
pub const PACKETS_PER_STEP: usize = 8;
pub const SAMPLES_PER_STEP: usize = PACKETS_PER_STEP * 1152 * 2;
const WORK_BYTES_PER_STEP: usize = 4096;

#[derive(Debug, PartialEq, Eq)]
pub enum Progress {
    NeedInput,
    #[cfg(feature = "m4a-streaming-probe")]
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
    #[cfg(feature = "m4a-streaming-probe")]
    source_waiting: bool,
    pub rate: u32,
    pub channels: u32,
}

impl<P: Packetizer> Decoder<P> {
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
            #[cfg(feature = "m4a-streaming-probe")]
            source_waiting: false,
            rate: 0,
            channels: 0,
        }
    }

    /// Accept at most the remaining bounded input space. The caller drains and
    /// retries the unaccepted suffix; it never appends an unbounded HTTP chunk.
    pub fn push(&mut self, data: &[u8]) -> Result<usize, String> {
        #[cfg(feature = "m4a-streaming-probe")]
        if self.source_waiting {
            return Err("source request must be resolved before accepting input".into());
        }
        if self.eof || self.finished || self.cancelled || self.failed {
            return Err("stream no longer accepts encoded input".into());
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

    #[cfg(feature = "m4a-streaming-probe")]
    pub(super) fn source_packetizer(&self) -> Option<&P> {
        (self.source_waiting && !self.failed && !self.cancelled && !self.finished)
            .then_some(&self.packets)
    }

    #[cfg(feature = "m4a-streaming-probe")]
    pub(super) fn resume_source(
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
        let mut packets = 0;
        let mut bytes = 0;
        let mut scanned = 0;
        loop {
            if self.cancelled || cancel.load(Ordering::Acquire) {
                self.cancelled = true;
                out.clear();
                return Ok(Progress::Cancelled);
            }
            #[cfg(feature = "m4a-streaming-probe")]
            if self.source_waiting {
                return Ok(Progress::NeedSource);
            }
            if packets >= self.packet_budget
                || bytes >= P::ENCODED_BYTES_PER_STEP
                || scanned >= WORK_BYTES_PER_STEP
            {
                return Ok(Progress::Yield);
            }
            let input = &self.input[self.head..];
            let (consumed, packet) = match self
                .packets
                .next(input, self.eof)
                .map_err(|e| format!("packet: {e}"))?
            {
                PacketStep::NeedMore => return Ok(Progress::NeedInput),
                #[cfg(feature = "m4a-streaming-probe")]
                PacketStep::NeedSource => {
                    self.source_waiting = true;
                    return Ok(Progress::NeedSource);
                }
                PacketStep::End => {
                    if self.total_samples == 0 {
                        return Err("stream contains no decoded samples".into());
                    }
                    // Containers may end their audio extent before HTTP EOF.
                    // Consume any non-audio tail within the same input bound,
                    // but let the transport confirm its own successful EOF.
                    if !self.eof {
                        self.input.clear();
                        self.head = 0;
                        return Ok(Progress::NeedInput);
                    }
                    self.finished = true;
                    return Ok(Progress::Finished);
                }
                PacketStep::Discard(consumed) => {
                    if consumed == 0 || consumed > input.len() {
                        return Err("invalid discarded byte count".into());
                    }
                    self.head += consumed;
                    bytes += consumed;
                    scanned += consumed;
                    if self.head == self.input.len() {
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

            if self.head == self.input.len() {
                self.input.clear();
                self.head = 0;
            }
        }
    }
}

fn validate_track<P: Packetizer>(
    track: &Track,
    cap: Option<u64>,
) -> Result<&symphonia::core::codecs::audio::AudioCodecParameters, String> {
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
