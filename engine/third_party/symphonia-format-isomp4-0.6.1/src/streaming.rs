// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Bounded complete-index parsing and borrowed AAC-LC sample descriptors.
//!
//! The caller owns network/file access and must validate sample extents against
//! actual mdat atoms. This module performs no IO or implicit range requests.

use std::sync::Arc;

use symphonia_core::codecs::{audio::well_known::CODEC_ID_AAC, CodecParameters};
use symphonia_core::common::Limit;
use symphonia_core::errors::{decode_error, unsupported_error, Error, Result, SeekErrorKind};
use symphonia_core::formats::{SeekedTo, Track};
use symphonia_core::io::BufReader;
use symphonia_core::meta::MetadataOptions;
use symphonia_core::packet::PacketRef;
use symphonia_core::units::{Duration, Timestamp};

use crate::atoms::{
    stsz::SampleSize, Atom, AtomHeader, AtomIterator, AtomType, FtypAtom, MoovAtom, ReadAtom,
};
use crate::demuxer::{TimeSpan, TrackState};
use crate::stream::{MoovSegment, StreamSegment};

impl ReadAtom for BufReader<'_> {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AtomKind {
    FileType,
    Movie,
    MediaData,
    Padding,
    Fragmented,
    Other,
}

/// A top-level atom extent. Reading this fixed-size header never scans payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AtomSpan {
    pub kind: AtomKind,
    pub offset: u64,
    pub body_offset: u64,
    pub end: Option<u64>,
    pub header_len: usize,
}

impl AtomSpan {
    /// None means fewer than the required 8 or 16 header bytes are available.
    /// Object length is authoritative only for the exact representation read.
    pub fn read(input: &[u8], offset: u64, object_len: Option<u64>) -> Result<Option<Self>> {
        if input.len() < usize::from(AtomHeader::HEADER_SIZE) {
            return Ok(None);
        }
        if input[..4] == 1_u32.to_be_bytes() && input.len() < 16 {
            return Ok(None);
        }
        let header = AtomHeader::read(&mut BufReader::new(input))?;
        let body_offset = offset
            .checked_add(header.data_pos())
            .ok_or(Error::DecodeError("isomp4: atom body offset overflow"))?;
        let end = match header.end() {
            Some(relative) => Some(
                offset
                    .checked_add(relative)
                    .ok_or(Error::DecodeError("isomp4: atom end overflow"))?,
            ),
            None => object_len,
        };
        if end.is_some_and(|end| end < body_offset)
            || object_len.is_some_and(|len| body_offset > len || end.is_some_and(|end| end > len))
        {
            return decode_error("isomp4: atom extent exceeds object");
        }
        let kind = match header.atom_type() {
            AtomType::FileType => AtomKind::FileType,
            AtomType::Movie => AtomKind::Movie,
            AtomType::MediaData => AtomKind::MediaData,
            AtomType::Free | AtomType::Skip => AtomKind::Padding,
            AtomType::MovieFragment => AtomKind::Fragmented,
            _ => AtomKind::Other,
        };
        Ok(Some(Self {
            kind,
            offset,
            body_offset,
            end,
            header_len: header.data_pos() as usize,
        }))
    }
}

fn complete_atom<A: Atom>(input: &[u8], kind: AtomKind, cap: usize) -> Result<A> {
    if input.len() > cap {
        return decode_error("isomp4: index atom exceeds configured byte budget");
    }
    let span = AtomSpan::read(input, 0, Some(input.len() as u64))?
        .ok_or(Error::DecodeError("isomp4: truncated index atom header"))?;
    if span.kind != kind || span.end != Some(input.len() as u64) {
        return decode_error("isomp4: expected one complete index atom");
    }
    // The sync parser runs only over a complete bounded atom, never over a
    // network reader returning WouldBlock partway through a nested parse.
    let options = MetadataOptions::default().limit_total_bytes(Limit::Maximum(0));
    let mut atoms = AtomIterator::new(BufReader::new(input), Some(input.len() as u64))
        .with_metadata_options(options);
    let header = atoms
        .next_header()?
        .ok_or(Error::DecodeError("isomp4: missing index atom"))?;
    if header.size().is_none() {
        return unsupported_error("isomp4: streaming index requires an explicit atom size");
    }
    Ok(atoms.read_atom::<A>()?)
}

/// Validate a complete bounded file-type atom using the regular parser.
pub fn validate_file_type(input: &[u8]) -> Result<()> {
    complete_atom::<FtypAtom>(input, AtomKind::FileType, 64 * 1024).map(|_| ())
}

#[derive(Clone, Copy, Debug)]
pub struct IndexOptions {
    pub max_index_bytes: usize,
    pub max_packet_bytes: u32,
    pub object_len: Option<u64>,
}

impl Default for IndexOptions {
    fn default() -> Self {
        Self {
            max_index_bytes: 2 * 1024 * 1024,
            max_packet_bytes: 64 * 1024,
            object_len: None,
        }
    }
}

/// A sample's location and presentation timing. Payload ownership stays with IO.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IndexedPacket {
    pub offset: u64,
    pub size: u32,
    pub track_id: u32,
    pub pts: Timestamp,
    pub dur: Duration,
    pub trim_start: Duration,
    pub trim_end: Duration,
}

impl IndexedPacket {
    pub fn packet<'a>(&self, data: &'a [u8]) -> Result<PacketRef<'a>> {
        if data.len() != self.size as usize {
            return decode_error("isomp4: indexed packet length mismatch");
        }
        let mut packet = PacketRef::new(self.track_id, self.pts, self.dur, data);
        packet.trim_start = self.trim_start;
        packet.trim_end = self.trim_end;
        Ok(packet)
    }
}

/// One nonfragmented AAC-LC mono/stereo track, using the pull reader's cursor
/// and presentation timeline. There is no per-sample Vec, Box or payload copy.
pub struct AudioIndex {
    segment: MoovSegment,
    state: TrackState,
    track: Track,
    count: u32,
    options: IndexOptions,
    failed: bool,
}

impl AudioIndex {
    pub fn from_moov(input: &[u8], options: IndexOptions) -> Result<Self> {
        if options.max_index_bytes < 8 || options.max_packet_bytes == 0 {
            return decode_error("isomp4: invalid streaming index budget");
        }
        let moov = complete_atom::<MoovAtom>(input, AtomKind::Movie, options.max_index_bytes)?;
        if moov.is_fragmented() || moov.traks.len() != 1 {
            return unsupported_error("isomp4: streaming requires one nonfragmented audio track");
        }
        let trak = &moov.traks[0];
        let stbl = &trak.mdia.minf.stbl;
        let timespan = TimeSpan::new(
            trak.mdia.mdhd.timescale,
            Duration::new(stbl.stts.total_duration),
        );
        let (state, track) = TrackState::make(0, trak, &timespan, moov.mvhd.timescale, false)?;
        match track.codec_params.as_ref() {
            Some(CodecParameters::Audio(audio))
                if audio.codec == CODEC_ID_AAC
                    && audio
                        .channels
                        .as_ref()
                        .is_some_and(|c| matches!(c.count(), 1 | 2)) =>
            {
                ()
            }
            _ => return unsupported_error("isomp4: streaming requires AAC-LC mono/stereo"),
        }
        let count = stbl.stsz.sample_count;
        if count == 0 {
            return decode_error("isomp4: empty streaming sample table");
        }
        let mut timing_count = 0_u32;
        for row in &stbl.stts.entries {
            timing_count = timing_count
                .checked_add(row.sample_count)
                .ok_or(Error::DecodeError(
                    "isomp4: streaming sample count overflow",
                ))?;
        }
        if timing_count != count {
            return decode_error("isomp4: inconsistent timing/sample counts");
        }
        let valid_size = |size| size != 0 && size <= options.max_packet_bytes;
        match &stbl.stsz.sample_sizes {
            SampleSize::Constant(size) if valid_size(*size) => (),
            SampleSize::Variable(sizes)
                if sizes.len() == count as usize && sizes.iter().copied().all(valid_size) =>
            {
                ()
            }
            _ => return decode_error("isomp4: streaming packet exceeds configured byte budget"),
        }
        let chunks = match (&stbl.stco, &stbl.co64) {
            (Some(table), None) => table.chunk_offsets.len(),
            (None, Some(table)) => table.chunk_offsets.len(),
            _ => return decode_error("isomp4: expected one chunk offset table"),
        };
        let groups = &stbl.stsc.entries;
        if groups.first().is_none_or(|g| g.first_chunk != 0) || chunks == 0 {
            return decode_error("isomp4: invalid first streaming chunk");
        }
        let mut grouped_samples = 0_u64;
        for (i, group) in groups.iter().enumerate() {
            let end = groups
                .get(i + 1)
                .map_or(chunks as u64, |g| u64::from(g.first_chunk));
            let start = u64::from(group.first_chunk);
            if end <= start || end > chunks as u64 || group.sample_desc_index != 1 {
                return decode_error("isomp4: invalid streaming chunk group");
            }
            let samples = (end - start)
                .checked_mul(u64::from(group.samples_per_chunk))
                .and_then(|n| grouped_samples.checked_add(n))
                .ok_or(Error::DecodeError("isomp4: streaming chunk count overflow"))?;
            grouped_samples = samples;
        }
        if grouped_samples != u64::from(count) {
            return decode_error("isomp4: inconsistent chunk/sample counts");
        }
        Ok(Self {
            segment: MoovSegment::new(Arc::new(moov)),
            state,
            track,
            count,
            options,
            failed: false,
        })
    }

    pub fn track(&self) -> &Track {
        &self.track
    }
    pub fn packet_count(&self) -> u32 {
        self.count
    }

    pub fn rewind(&mut self) -> Result<()> {
        if self.failed {
            return decode_error("isomp4: streaming index previously failed");
        }
        self.state.reset();
        Ok(())
    }

    /// Seek in presentation PCM frames with the pull reader's AAC preroll.
    /// This moves only the index cursor. The caller must reset the codec,
    /// resolve the next packet's pinned source extent and discard preroll PCM
    /// before required_ts. No IO or decoder work is hidden in this operation.
    /// Out-of-range targets preserve the cursor; corrupt extents fail closed.
    pub fn seek(&mut self, ts: Timestamp) -> Result<SeekedTo> {
        if self.failed {
            return decode_error("isomp4: streaming index previously failed");
        }
        let result = self.seek_inner(ts);
        self.failed = result.as_ref().is_err_and(|error| !matches!(error, Error::SeekError(_)));
        result
    }

    fn seek_inner(&mut self, required_ts: Timestamp) -> Result<SeekedTo> {
        let media_ts = self.state.seek_media_ts(required_ts)?;
        let sample = self.segment.ts_sample(0, media_ts.get() as u64)?
            .ok_or(Error::SeekError(SeekErrorKind::OutOfRange))?;
        self.state.seek_sample(&self.segment, 0, sample, required_ts, self.options.object_len)
    }

    pub fn next_packet(&mut self) -> Result<Option<IndexedPacket>> {
        if self.failed {
            return decode_error("isomp4: streaming index previously failed");
        }
        let result = self.next_inner();
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn next_inner(&mut self) -> Result<Option<IndexedPacket>> {
        if self.state.sample_number() == self.count {
            return Ok(None);
        }
        let timing = self
            .state
            .next_timing(&self.segment, 0)?
            .ok_or(Error::DecodeError(
                "isomp4: streaming sample timing missing",
            ))?;
        let info = self
            .state
            .sample_info(&self.track, timing, 0)?
            .ok_or(Error::DecodeError(
                "isomp4: streaming sample time out of range",
            ))?;
        let data = self.state.consume_sample(&info, &self.segment)?;
        let end = data
            .pos
            .checked_add(u64::from(data.len))
            .ok_or(Error::DecodeError("isomp4: streaming sample end overflow"))?;
        if self.options.object_len.is_some_and(|len| end > len) {
            return decode_error("isomp4: streaming sample exceeds object");
        }
        Ok(Some(IndexedPacket {
            offset: data.pos,
            size: data.len,
            track_id: info.track_id,
            pts: info.ts,
            dur: info.dur,
            trim_start: info.trim_start,
            trim_end: info.trim_end,
        }))
    }
}

mod sequential;
pub use sequential::{BufferedStep, IndexRequest, PacketStep, Packetizer, PacketizerOptions};
