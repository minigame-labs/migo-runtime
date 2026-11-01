// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
// Copyright (c) 2026 Migo contributors.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Borrowed incremental WAVE packetization. No Read is attempted until the
//! complete bounded field is available. Optional metadata is skipped without
//! retaining values; audio length is independent of the input working set.
use super::chunks::{append_fact_params, FactChunk, RiffWaveChunks, WaveFormatChunk};
use crate::common::{
    append_data_params, append_format_params, ByteOrder, ChunksReader, FormatData, PacketInfo,
    ParseChunk, ParseChunkTag,
};
use std::num::NonZero;
use symphonia_core::{
    codecs::{audio::AudioCodecParameters, CodecParameters},
    errors::{decode_error, unsupported_error, Error, Result},
    formats::Track,
    io::{BufReader, ReadBytes},
    packet::PacketRef,
    units::{Duration, TimeBase, Timestamp},
};

#[derive(Debug)]
pub enum PacketStep<'a> {
    NeedMore,
    Discard(usize),
    Packet {
        consumed: usize,
        packet: PacketRef<'a>,
    },
    End,
}

#[derive(Clone, Copy, Debug)]
pub struct PacketizerOptions {
    pub max_format_bytes: usize,
    pub max_packet_bytes: usize,
}
impl Default for PacketizerOptions {
    fn default() -> Self {
        Self {
            max_format_bytes: 64 * 1024,
            max_packet_bytes: 32 * 1024,
        }
    }
}
#[derive(Clone, Copy)]
enum State {
    Riff,
    Chunk,
    Format(u32),
    Fact(u32),
    Skip(u64),
    Data(Option<u64>),
    End,
}

/// The first WAVE data extent is audio; trailing non-audio chunks are not
/// required to finish it. Unknown RIFF/data lengths used by streaming encoders
/// are accepted; an incomplete PCM frame at actual EOF is an error.
pub struct WavPacketizer {
    options: PacketizerOptions,
    state: State,
    chunks: Option<ChunksReader<RiffWaveChunks>>,
    params: Option<AudioCodecParameters>,
    info: Option<PacketInfo>,
    fact: Option<FactChunk>,
    track: Option<Track>,
    next_pts: Timestamp,
    failed: bool,
}
impl WavPacketizer {
    pub fn new(options: PacketizerOptions) -> Self {
        Self {
            options,
            state: State::Riff,
            chunks: None,
            params: None,
            info: None,
            fact: None,
            track: None,
            next_pts: Timestamp::ZERO,
            failed: false,
        }
    }
    pub fn track(&self) -> Option<&Track> {
        self.track.as_ref()
    }

    /// NeedMore consumes no input and does not advance a field parser. Errors
    /// are sticky; use a new packetizer for a new stream.
    pub fn next<'a>(&mut self, input: &'a [u8], eof: bool) -> Result<PacketStep<'a>> {
        if self.failed {
            return decode_error("wav: stream packetizer previously failed");
        }
        let result = self.next_inner(input, eof);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn need<'a>(eof: bool) -> Result<PacketStep<'a>> {
        if eof {
            decode_error("wav: truncated stream")
        } else {
            Ok(PacketStep::NeedMore)
        }
    }
    fn next_inner<'a>(&mut self, input: &'a [u8], eof: bool) -> Result<PacketStep<'a>> {
        if self.options.max_format_bytes < 16 || self.options.max_packet_bytes == 0 {
            return unsupported_error("wav: invalid packetizer working-set policy");
        }
        match self.state {
            State::Riff => {
                if input.len() < 12 {
                    return Self::need(eof);
                }
                let len = super::read_wave_header(&mut BufReader::new(&input[..12]))?;
                self.chunks = Some(ChunksReader::new(len, ByteOrder::LittleEndian));
                self.state = State::Chunk;
                Ok(PacketStep::Discard(12))
            }
            State::Chunk => {
                let chunks = self.chunks.as_mut().expect("RIFF header initialized");
                let needed = chunks.next_header_bytes();
                if input.len() < needed {
                    return Self::need(eof);
                }
                let mut reader = BufReader::new(&input[..needed]);
                let Some((tag, len)) = chunks.next_header(&mut reader)? else {
                    return decode_error("wav: missing data chunk");
                };
                self.state = match RiffWaveChunks::parse_tag(tag, len) {
                    Some(RiffWaveChunks::Format(_)) => State::Format(len),
                    Some(RiffWaveChunks::Fact(_)) => State::Fact(len),
                    Some(RiffWaveChunks::Data(_)) => {
                        let Some(params) = self.params.take() else {
                            return decode_error("wav: missing format chunk");
                        };
                        let info = self.info.as_ref().expect("format initialized packet info");
                        let data_len = (len != u32::MAX).then_some(u64::from(len));
                        if data_len.is_some_and(|len| len % info.block_size.get() != 0) {
                            return decode_error("wav: data ends inside an audio frame");
                        }
                        let rate = NonZero::new(params.sample_rate.expect("validated sample rate"))
                            .expect("validated nonzero rate");
                        let mut track = Track::new(0);
                        track.with_codec_params(CodecParameters::Audio(params));
                        track.with_time_base(TimeBase::from_recip(rate));
                        if let Some(fact) = self.fact.take() {
                            append_fact_params(&mut track, &fact);
                        }
                        if let Some(len) = data_len {
                            append_data_params(&mut track, len, info);
                        }
                        self.track = Some(track);
                        State::Data(data_len)
                    }
                    _ if len == 0 => State::Chunk,
                    _ => State::Skip(u64::from(len)),
                };
                Ok(PacketStep::Discard(reader.pos() as usize))
            }
            State::Format(len) => {
                if self.params.is_some() {
                    return decode_error("wav: duplicate format chunk");
                }
                if len < 16 {
                    return decode_error("wav: malformed fmt chunk");
                }
                if len as usize > self.options.max_format_bytes {
                    return unsupported_error("wav: fmt exceeds packetizer working set");
                }
                if input.len() < len as usize {
                    return Self::need(eof);
                }
                let format = WaveFormatChunk::parse(
                    &mut BufReader::new(&input[..len as usize]),
                    *b"fmt ",
                    len,
                )?;
                if format.sample_rate == 0 {
                    return decode_error("wav: sample rate is zero");
                }
                if matches!(format.format_data, FormatData::Adpcm(_)) {
                    return unsupported_error("wav: incremental profile excludes ADPCM");
                }
                let mut info = format.packet_info()?;
                if info.frames_per_block.get() != 1 {
                    return unsupported_error(
                        "wav: incremental profile requires independently framed PCM",
                    );
                }
                let max_blocks = (self.options.max_packet_bytes as u64 / info.block_size.get())
                    .min(info.max_blocks_per_packet.get());
                let Some(max_blocks) = NonZero::new(max_blocks) else {
                    return unsupported_error("wav: audio frame exceeds packet working set");
                };
                info.max_blocks_per_packet = max_blocks;
                info.max_frames_per_packet = max_blocks;
                let mut params = AudioCodecParameters::new();
                params
                    .with_max_frames_per_packet(max_blocks.get())
                    .with_frames_per_block(1);
                append_format_params(&mut params, format.format_data, format.sample_rate);
                self.params = Some(params);
                self.info = Some(info);
                self.state = State::Chunk;
                Ok(PacketStep::Discard(len as usize))
            }
            State::Fact(len) => {
                if len != 4 {
                    return decode_error("wav: malformed fact chunk");
                }
                if input.len() < 4 {
                    return Self::need(eof);
                }
                self.fact = Some(FactChunk::parse(
                    &mut BufReader::new(&input[..4]),
                    *b"fact",
                    4,
                )?);
                self.state = State::Chunk;
                Ok(PacketStep::Discard(4))
            }
            State::Skip(left) => {
                if input.is_empty() {
                    return Self::need(eof);
                }
                let consumed = left.min(input.len() as u64).min(4096) as usize;
                let remaining = left - consumed as u64;
                self.state = if remaining == 0 {
                    State::Chunk
                } else {
                    State::Skip(remaining)
                };
                Ok(PacketStep::Discard(consumed))
            }
            State::Data(left) => {
                if left == Some(0) || (left.is_none() && input.is_empty() && eof) {
                    self.state = State::End;
                    return Ok(PacketStep::End);
                }
                let info = self.info.as_ref().expect("data initialized packet info");
                let width = info.block_size.get();
                if eof && left.is_some_and(|left| left > input.len() as u64) {
                    return decode_error("wav: truncated data extent");
                }
                if eof && left.is_none() && input.len() as u64 % width != 0 {
                    return decode_error("wav: data ends inside an audio frame");
                }
                let target =
                    left.unwrap_or(u64::MAX)
                        .min(width * info.max_blocks_per_packet.get()) as usize;
                let size = input.len().min(target) / width as usize * width as usize;
                if size == 0 {
                    return Self::need(eof);
                }
                let duration = Duration::from(size as u64 / width);
                let packet = PacketRef::new(0, self.next_pts, duration, &input[..size]);
                self.next_pts = self
                    .next_pts
                    .checked_add(duration)
                    .ok_or(Error::DecodeError("wav: stream timestamp overflow"))?;
                self.state = State::Data(left.map(|left| left - size as u64));
                Ok(PacketStep::Packet {
                    consumed: size,
                    packet,
                })
            }
            State::End => Ok(PacketStep::End),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(output: &mut Vec<u8>, tag: &[u8; 4], payload: &[u8]) {
        output.extend_from_slice(tag);
        output.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        output.extend_from_slice(payload);
        if payload.len() & 1 != 0 {
            output.push(0);
        }
    }
    fn wave(
        channels: u16,
        bits: u16,
        floating: bool,
        frames: usize,
        metadata: usize,
    ) -> (Vec<u8>, Vec<u8>) {
        let width = channels as usize * bits as usize / 8;
        let audio: Vec<_> = (0..frames * width)
            .map(|x| (x.wrapping_mul(17) & 255) as u8)
            .collect();
        let mut file = b"RIFF\0\0\0\0WAVE".to_vec();
        if metadata != 0 {
            chunk(&mut file, b"LIST", &vec![0x5a; metadata]);
        }
        let mut format = Vec::new();
        format.extend_from_slice(&(if floating { 3_u16 } else { 1_u16 }).to_le_bytes());
        format.extend_from_slice(&channels.to_le_bytes());
        format.extend_from_slice(&44100_u32.to_le_bytes());
        format.extend_from_slice(&(44100 * width as u32).to_le_bytes());
        format.extend_from_slice(&(width as u16).to_le_bytes());
        format.extend_from_slice(&bits.to_le_bytes());
        chunk(&mut file, b"fmt ", &format);
        if floating {
            chunk(&mut file, b"fact", &(frames as u32).to_le_bytes());
        }
        chunk(&mut file, b"data", &audio);
        let size = file.len() as u32 - 8;
        file[4..8].copy_from_slice(&size.to_le_bytes());
        (file, audio)
    }
    fn drain(file: &[u8], fragment: usize, width: usize) -> Result<(Vec<u8>, usize)> {
        let mut reader = WavPacketizer::new(PacketizerOptions::default());
        let (mut offset, mut available, mut frames) = (0, 0, 0);
        let mut audio = Vec::new();
        for _ in 0..file.len().saturating_mul(3) + 1024 {
            match reader.next(&file[offset..available], available == file.len())? {
                PacketStep::NeedMore => {
                    assert!(available < file.len(), "EOF cannot request more bytes");
                    available = (available + fragment).min(file.len());
                }
                PacketStep::Discard(n) => {
                    assert!(n > 0 && n <= 4096 && n <= available - offset);
                    offset += n;
                }
                PacketStep::Packet { consumed, packet } => {
                    assert!(consumed > 0 && consumed <= 32768 && consumed % width == 0);
                    assert_eq!(
                        packet.data.as_ptr(),
                        file[offset..].as_ptr(),
                        "packet must borrow the supplied input"
                    );
                    assert_eq!(packet.pts, Timestamp::new(frames as i64));
                    assert_eq!(packet.dur, Duration::from((consumed / width) as u64));
                    assert_eq!(packet.trim_start, Duration::ZERO);
                    frames += consumed / width;
                    audio.extend_from_slice(packet.data);
                    offset += consumed;
                }
                PacketStep::End => {
                    let track = reader.track().expect("data initialized track");
                    assert_eq!(
                        track.time_base,
                        Some(TimeBase::from_recip(NonZero::new(44100).unwrap()))
                    );
                    assert!(matches!(reader.next(&[], false).unwrap(), PacketStep::End));
                    return Ok((audio, frames));
                }
            }
        }
        panic!("packetizer did not make bounded progress");
    }

    #[test]
    fn all_pcm_widths_channels_and_partitions_preserve_borrowed_packets_and_timestamps() {
        for channels in [1, 2] {
            for (bits, floating) in [
                (8, false),
                (16, false),
                (24, false),
                (32, false),
                (32, true),
            ] {
                let width = channels as usize * bits as usize / 8;
                let (file, expected) = wave(channels, bits, floating, 2317, 3);
                for fragment in [1, 2, 3, 7, 13, 128, 8192, 65536] {
                    let (actual, frames) = drain(&file, fragment, width).unwrap();
                    assert_eq!(frames, 2317, "{channels}/{bits}/{floating}/{fragment}");
                    assert_eq!(actual, expected, "{channels}/{bits}/{floating}/{fragment}");
                }
            }
        }
    }

    #[test]
    fn one_complete_frame_can_be_published_before_the_rest_of_the_extent() {
        let (file, _) = wave(2, 16, false, 8192, 0);
        let mut reader = WavPacketizer::new(PacketizerOptions::default());
        let mut position = 0;
        while position < 44 {
            match reader.next(&file[position..48], false).unwrap() {
                PacketStep::Discard(n) => position += n,
                _ => panic!("header was not consumed"),
            }
        }
        match reader.next(&file[position..48], false).unwrap() {
            PacketStep::Packet { consumed, packet } => {
                assert_eq!(consumed, 4);
                assert_eq!(packet.dur, Duration::from(1_u64));
            }
            _ => panic!("a complete PCM frame was unnecessarily held for a full packet"),
        }
    }

    #[test]
    fn large_unused_metadata_is_skipped_in_bounded_steps() {
        let (file, expected) = wave(1, 24, false, 17, 2 * 1024 * 1024 + 1);
        for fragment in [257, 65536] {
            let (actual, frames) = drain(&file, fragment, 3).unwrap();
            assert_eq!(actual, expected);
            assert_eq!(frames, 17);
        }
    }

    #[test]
    fn truncated_fields_and_data_fail_normally_and_errors_are_sticky() {
        let (file, _) = wave(2, 16, false, 17, 3);
        // Data is even-sized, so every strict prefix lacks a required byte.
        for end in 0..file.len() {
            assert!(
                drain(&file[..end], 7, 4).is_err(),
                "accepted truncated prefix {end}"
            );
        }
        let mut broken = file.clone();
        broken[4..8].copy_from_slice(&4_u32.to_le_bytes());
        let mut reader = WavPacketizer::new(PacketizerOptions::default());
        assert!(matches!(
            reader.next(&broken, true),
            Ok(PacketStep::Discard(12))
        ));
        assert!(reader.next(&broken[12..], true).is_err());
        assert!(reader.next(&file, true).is_err());
        let mut reader = WavPacketizer::new(PacketizerOptions::default());
        let mut bad_magic = file.clone();
        bad_magic[0] = b'X';
        assert!(reader.next(&bad_magic, true).is_err());
        assert!(
            reader.next(&file, true).is_err(),
            "terminal failure cannot restart the parser"
        );
    }

    #[test]
    fn declared_misalignment_zero_rate_and_working_set_overflow_are_rejected() {
        let (file, _) = wave(2, 16, false, 17, 0);
        let mut exceeds_parent = file.clone();
        exceeds_parent[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(
            drain(&exceeds_parent, 65536, 4).is_err(),
            "a data extent cannot exceed a known RIFF parent"
        );
        let mut misaligned = file.clone();
        misaligned[40..44].copy_from_slice(&3_u32.to_le_bytes());
        assert!(drain(&misaligned, 65536, 4).is_err());
        let mut zero_rate = file.clone();
        zero_rate[24..28].copy_from_slice(&0_u32.to_le_bytes());
        assert!(drain(&zero_rate, 65536, 4).is_err());
        let mut reader = WavPacketizer::new(PacketizerOptions {
            max_format_bytes: 16,
            max_packet_bytes: 3,
        });
        let mut offset = 0;
        loop {
            match reader.next(&file[offset..], true) {
                Ok(PacketStep::Discard(n)) => offset += n,
                Err(_) => break,
                _ => panic!("a whole stereo frame exceeds the configured packet budget"),
            }
        }
    }

    #[test]
    fn a_one_frame_adpcm_block_does_not_expand_the_pcm_profile() {
        let mut file = b"RIFF\0\0\0\0WAVE".to_vec();
        let mut format = Vec::new();
        for n in [0x11_u16, 1] {
            format.extend_from_slice(&n.to_le_bytes());
        }
        format.extend_from_slice(&44100_u32.to_le_bytes());
        format.extend_from_slice(&44100_u32.to_le_bytes());
        for n in [4_u16, 4, 2, 1] {
            format.extend_from_slice(&n.to_le_bytes());
        }
        chunk(&mut file, b"fmt ", &format);
        chunk(&mut file, b"data", &[0, 0, 0, 0]);
        let size = file.len() as u32 - 8;
        file[4..8].copy_from_slice(&size.to_le_bytes());
        assert!(
            drain(&file, 65536, 4).is_err(),
            "one frame per block is not sufficient to identify PCM"
        );
    }

    #[test]
    fn excessive_channel_counts_return_errors_instead_of_panicking() {
        for channels in [32768_u16, 65535] {
            let (mut file, _) = wave(1, 32, false, 17, 0);
            file[22..24].copy_from_slice(&channels.to_le_bytes());
            let result = std::panic::catch_unwind(|| drain(&file, 65536, 4));
            assert!(
                result.is_ok(),
                "untrusted fmt channels caused a parser panic: {channels}"
            );
            assert!(result.unwrap().is_err());
        }
    }

    #[test]
    fn unknown_lengths_use_real_eof_and_reject_incomplete_frames() {
        let (mut file, expected) = wave(2, 16, false, 17, 0);
        file[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        file[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(drain(&file, 3, 4).unwrap(), (expected, 17));
        file.pop();
        assert!(drain(&file, 3, 4).is_err());
    }
}
