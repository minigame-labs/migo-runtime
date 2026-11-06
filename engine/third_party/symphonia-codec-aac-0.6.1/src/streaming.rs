// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Borrowed AAC-LC/ADTS framing over caller-owned incremental input.
//! ADTS header parsing and track initialization are shared with AdtsReader.
use crate::adts::{AdtsHeader, SAMPLES_PER_AAC_PACKET};
use symphonia_core::{
    errors::{Error, Result, decode_error},
    formats::Track,
    io::BufReader,
    packet::PacketRef,
    units::Timestamp,
};
use symphonia_metadata::id3v2::Id3v2Reader;

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
    pub max_metadata_bytes: u64,
    pub max_junk_bytes: u64,
}
impl Default for PacketizerOptions {
    fn default() -> Self {
        Self {
            max_metadata_bytes: 4 * 1024 * 1024,
            max_junk_bytes: 64 * 1024,
        }
    }
}

pub struct AdtsPacketizer {
    options: PacketizerOptions,
    track: Option<Track>,
    next_pts: Timestamp,
    tag_remaining: u64,
    metadata_bytes: u64,
    junk_bytes: u64,
    failed: bool,
    finished: bool,
}
impl AdtsPacketizer {
    pub fn new(options: PacketizerOptions) -> Self {
        Self {
            options,
            track: None,
            next_pts: Timestamp::ZERO,
            tag_remaining: 0,
            metadata_bytes: 0,
            junk_bytes: 0,
            failed: false,
            finished: false,
        }
    }
    pub fn track(&self) -> Option<&Track> {
        self.track.as_ref()
    }
    pub fn next<'a>(&mut self, input: &'a [u8], eof: bool) -> Result<PacketStep<'a>> {
        if self.failed {
            return decode_error("adts: packetizer previously failed");
        }
        let result = self.next_inner(input, eof);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn skip_tag(&mut self, input: &[u8], eof: bool) -> Result<PacketStep<'static>> {
        if eof && (input.len() as u64) < self.tag_remaining {
            return decode_error("adts: truncated metadata extent");
        }
        let consumed = self.tag_remaining.min(input.len() as u64).min(4096) as usize;
        if consumed == 0 {
            return Ok(PacketStep::NeedMore);
        }
        self.tag_remaining -= consumed as u64;
        Ok(PacketStep::Discard(consumed))
    }
    fn begin_tag(&mut self, len: u64, input: &[u8], eof: bool) -> Result<PacketStep<'static>> {
        self.metadata_bytes = self
            .metadata_bytes
            .checked_add(len)
            .filter(|n| *n <= self.options.max_metadata_bytes)
            .ok_or(Error::LimitError("adts: metadata byte budget exceeded"))?;
        self.tag_remaining = len;
        self.skip_tag(input, eof)
    }
    fn discard_junk(&mut self, n: usize) -> Result<PacketStep<'static>> {
        self.junk_bytes = self
            .junk_bytes
            .checked_add(n as u64)
            .filter(|n| *n <= self.options.max_junk_bytes)
            .ok_or(Error::LimitError(
                "adts: synchronization byte budget exceeded",
            ))?;
        Ok(PacketStep::Discard(n))
    }
    fn next_inner<'a>(&mut self, input: &'a [u8], eof: bool) -> Result<PacketStep<'a>> {
        if self.finished {
            return if input.is_empty() && eof {
                Ok(PacketStep::End)
            } else {
                decode_error("adts: input after stream end")
            };
        }
        if input.is_empty() {
            if !eof {
                return Ok(PacketStep::NeedMore);
            }
            if self.tag_remaining != 0 {
                return decode_error("adts: truncated metadata extent");
            }
            self.finished = true;
            return Ok(PacketStep::End);
        }
        if self.tag_remaining != 0 {
            return self.skip_tag(input, eof);
        }
        if input.starts_with(b"ID3") {
            if input.len() < 10 {
                return if eof {
                    decode_error("adts: truncated ID3 header")
                } else {
                    Ok(PacketStep::NeedMore)
                };
            }
            let len = Id3v2Reader::total_tag_len(input[..10].try_into().expect("length checked"))?;
            return self.begin_tag(u64::from(len), input, eof);
        }
        if input.starts_with(b"TAG") {
            return self.begin_tag(128, input, eof);
        }
        if input.len() < 3 {
            if !eof {
                return Ok(PacketStep::NeedMore);
            }
            if input[0] == 0xff || b"ID3".starts_with(input) || b"TAG".starts_with(input) {
                return decode_error("adts: truncated header");
            }
            if self.track.is_some() {
                return decode_error("adts: lost frame synchronization");
            }
            return self.discard_junk(input.len());
        }
        let sync = u16::from_be_bytes([input[0], input[1]]);
        if !AdtsHeader::is_sync_word(sync) {
            if self.track.is_some() {
                return decode_error("adts: lost frame synchronization");
            }
            return self.discard_junk(1);
        }
        let header_len = if sync & 1 == 0 {
            AdtsHeader::SIZE_WITH_CRC
        } else {
            AdtsHeader::SIZE_NO_CRC
        } as usize;
        if input.len() < header_len {
            return if eof {
                decode_error("adts: truncated frame header")
            } else {
                Ok(PacketStep::NeedMore)
            };
        }
        let header = AdtsHeader::read_no_resync(&mut BufReader::new(input))?;
        let len = usize::from(header.frame_len);
        if len == header_len {
            return decode_error("adts: empty frame payload");
        }
        if input.len() < len {
            return if eof {
                decode_error("adts: truncated frame payload")
            } else {
                Ok(PacketStep::NeedMore)
            };
        }
        if let Some(track) = &self.track {
            header.validate_signal(track)?;
        } else {
            self.track = Some(header.track()?);
        }
        let pts = self.next_pts;
        self.next_pts = pts
            .checked_add(SAMPLES_PER_AAC_PACKET)
            .ok_or(Error::DecodeError("adts: packet timestamp overflow"))?;
        Ok(PacketStep::Packet {
            consumed: len,
            packet: PacketRef::new(0, pts, SAMPLES_PER_AAC_PACKET, &input[header_len..len]),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frame(crc: bool, rate: u8, channels: u8, payload: &[u8]) -> Vec<u8> {
        let len = payload.len() + if crc { 9 } else { 7 };
        assert!(len < 8192 && rate < 16 && channels < 8);
        let mut frame = vec![
            0xff,
            if crc { 0xf0 } else { 0xf1 },
            0x40 | rate << 2 | channels >> 2,
            (channels & 3) << 6 | (len >> 11) as u8,
            (len >> 3) as u8,
            ((len & 7) << 5) as u8 | 0x1f,
            0xfc,
        ];
        if crc {
            frame.extend_from_slice(&[0, 0]);
        }
        frame.extend_from_slice(payload);
        frame
    }
    #[test]
    fn first_complete_frame_is_borrowed_without_future_input() {
        for crc in [false, true] {
            let input = frame(crc, 3, 2, &[1, 2, 3, 4]);
            let mut stream = AdtsPacketizer::new(Default::default());
            for n in 0..input.len() {
                assert!(matches!(
                    stream.next(&input[..n], false).unwrap(),
                    PacketStep::NeedMore
                ));
                assert!(stream.track().is_none());
            }
            let PacketStep::Packet { consumed, packet } = stream.next(&input, false).unwrap()
            else {
                panic!()
            };
            assert_eq!(consumed, input.len());
            assert_eq!(packet.data, &[1, 2, 3, 4]);
            assert_eq!(packet.data.as_ptr(), input[consumed - 4..].as_ptr());
            assert_eq!((packet.pts.get(), packet.dur.get()), (0, 1024));
            assert!(stream.track().unwrap().num_frames.is_none());
            let PacketStep::Packet { packet, .. } = stream.next(&input, false).unwrap() else {
                panic!()
            };
            assert_eq!(packet.pts.get(), 1024);
            assert!(matches!(stream.next(&[], true).unwrap(), PacketStep::End));
        }
    }
    #[test]
    fn strict_profile_fields_truncation_and_failures_are_terminal() {
        let good = frame(false, 3, 2, &[1, 2, 3]);
        let mut bad = Vec::new();
        for n in 1..good.len() {
            bad.push(good[..n].to_vec());
        }
        for (offset, byte) in [(2, 0x0c), (2, 0x7c), (2, 0x4c), (6, 0xfd)] {
            let mut file = good.clone();
            file[offset] = byte;
            if offset == 2 && byte == 0x4c {
                file[3] &= 0x3f;
            }
            bad.push(file);
        }
        bad.push(frame(false, 3, 6, &[1, 2]));
        bad.push(frame(false, 3, 2, &[]));
        for file in bad {
            let mut stream = AdtsPacketizer::new(Default::default());
            assert!(stream.next(&file, true).is_err(), "accepted {file:?}");
            assert!(stream.next(&good, true).is_err(), "failure was not sticky");
        }
        let mut stream = AdtsPacketizer::new(Default::default());
        stream.next(&good, false).unwrap();
        assert!(stream.next(&frame(false, 4, 2, &[1]), false).is_err());
    }
    #[test]
    fn metadata_is_skipped_in_bounded_steps_without_parsing_fake_sync() {
        let len = 2 * 1024 * 1024;
        let mut file = b"ID3\x04\0\0".to_vec();
        for shift in [21, 14, 7, 0] {
            file.push(((len >> shift) & 0x7f) as u8);
        }
        file.resize(file.len() + len, 0xff);
        let audio = frame(false, 3, 1, &[1, 2]);
        file.extend_from_slice(&audio);
        for fragment in [1, 3, 137, 8192, 65536] {
            let mut stream = AdtsPacketizer::new(Default::default());
            let (mut offset, mut available, mut packets) = (0, 0, 0);
            loop {
                match stream
                    .next(&file[offset..available], available == file.len())
                    .unwrap()
                {
                    PacketStep::NeedMore => {
                        assert!(available < file.len());
                        available = (available + fragment).min(file.len());
                    }
                    PacketStep::Discard(n) => {
                        assert!(n > 0 && n <= 4096);
                        offset += n;
                    }
                    PacketStep::Packet { consumed, packet } => {
                        offset += consumed;
                        packets += 1;
                        assert_eq!(packet.data, &[1, 2]);
                    }
                    PacketStep::End => break,
                }
            }
            assert_eq!((offset, packets), (file.len(), 1));
        }
        let mut stream = AdtsPacketizer::new(PacketizerOptions {
            max_metadata_bytes: 1024,
            ..Default::default()
        });
        assert!(stream.next(&file[..10], false).is_err());
        let mut stream = AdtsPacketizer::new(PacketizerOptions {
            max_junk_bytes: 0,
            ..Default::default()
        });
        assert!(stream.next(b"junk", false).is_err());
    }
}
