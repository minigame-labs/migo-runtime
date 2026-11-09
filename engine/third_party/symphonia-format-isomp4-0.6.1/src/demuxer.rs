// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::codecs::CodecParameters;
use symphonia_core::support_format;

use symphonia_core::errors::{
    decode_error, seek_error, unsupported_error, Error, Result, SeekErrorKind,
};
use symphonia_core::formats::prelude::*;
use symphonia_core::formats::probe::{ProbeFormatData, ProbeableFormat, Score, Scoreable};
use symphonia_core::formats::well_known::FORMAT_ID_ISOMP4;
use symphonia_core::io::*;
use symphonia_core::meta::{Metadata, MetadataLog};
use symphonia_core::units::Time;

use std::collections::HashMap;
use std::io::{Seek, SeekFrom};
use std::num::NonZero;
use std::sync::Arc;

use crate::atoms::{AtomError, AtomIterator, AtomType, ReadAtom};
use crate::atoms::{FtypAtom, MetaAtom, MoofAtom, MoovAtom, SidxAtom, TrakAtom};
use crate::audio_timeline::AudioTimeline;
use crate::atoms::stts::TimingCursor;
use crate::stream::*;
use symphonia_common::mpeg::audio::{AudioObjectType, AudioSpecificConfig};
use symphonia_core::codecs::audio::well_known::CODEC_ID_AAC;

use log::{debug, info, trace, warn};

const ISOMP4_FORMAT_INFO: FormatInfo = FormatInfo {
    format: FORMAT_ID_ISOMP4,
    short_name: "isomp4",
    long_name: "ISO Base Media File Format",
};

pub struct TrackState {
    /// The track number.
    track_num: usize,
    /// The track ID.
    track_id: u32,
    /// The current segment.
    cur_seg: usize,
    /// The current sample index relative to the track.
    next_sample: u32,
    /// The current sample byte position relative to the start of the track.
    next_sample_pos: u64,
    audio_timeline: Option<AudioTimeline>,
    timing_cursor: TimingCursor,
    timing_cursor_segment: usize,
}

impl TrackState {
    pub fn make(
        track_num: usize,
        trak: &TrakAtom,
        timespan: &TimeSpan,
        movie_scale: NonZero<u32>,
        fragmented: bool,
    ) -> Result<(Self, Track)> {
        let mut track = Track::new(trak.tkhd.id);

        // Create the codec parameters using the sample description atom.
        if let Some(codec_params) = trak.mdia.minf.stbl.stsd.make_codec_params() {
            track.with_codec_params(codec_params);
        }

        // Populate timing information.
        track
            .with_time_base(TimeBase::from_recip(timespan.timescale))
            .with_duration(timespan.duration);

        // If the track is an audio track, and the timescale is equal to the sample rate, then the
        // number of frames is equal to the duration. This is the case for almost all audio tracks.
        // If not, there is no generic, low overhead, & precise way to determine the number of
        // frames.
        if let Some(CodecParameters::Audio(audio)) = &track.codec_params {
            if let Some(sample_rate) = audio.sample_rate {
                if sample_rate == timespan.timescale.get() {
                    track.with_num_frames(timespan.duration.get());
                }
            }
        }

        let edits = trak.edts.as_ref().and_then(|edts| edts.elst.as_ref());
        let audio_timeline = match track.codec_params.as_mut() {
            Some(CodecParameters::Audio(audio)) if audio.codec == CODEC_ID_AAC => {
                if fragmented {
                    return unsupported_error(
                        "isomp4: fragmented AAC presentation timing is not supported",
                    );
                }
                let asc =
                    AudioSpecificConfig::read(audio.extra_data.as_deref().ok_or(
                        Error::DecodeError("isomp4: AAC requires audio specific config"),
                    )?)?;
                if asc.object_type != AudioObjectType::Lc || asc.sbr_present {
                    return unsupported_error(
                        "isomp4: only AAC-LC presentation timing is supported",
                    );
                }
                let rate = NonZero::new(asc.sample_rate)
                    .ok_or(Error::DecodeError("isomp4: zero AAC sample rate"))?;
                let timeline = AudioTimeline::new(
                    trak.mdia.mdhd.timescale,
                    rate,
                    movie_scale,
                    trak.mdia.minf.stbl.stts.total_duration,
                    asc.samples as u64,
                    edits,
                )?;
                audio.with_sample_rate(rate.get()).with_max_frames_per_packet(asc.samples as u64);
                track
                    .with_time_base(TimeBase::from_recip(rate))
                    .with_duration(Duration::new(timeline.frames()))
                    .with_num_frames(timeline.frames());
                if let Some(delay) = timeline.delay() {
                    track.with_delay(delay);
                }
                Some(timeline)
            }
            _ if edits.is_some_and(|edits| !edits.entries.is_empty()) => {
                return unsupported_error("isomp4: edits for this codec are not supported");
            }
            _ => None,
        };

        let state = Self {
            track_num,
            track_id: trak.tkhd.id,
            cur_seg: 0,
            next_sample: 0,
            next_sample_pos: 0,
            audio_timeline,
            timing_cursor: TimingCursor::default(),
            timing_cursor_segment: 0,
        };

        Ok((state, track))
    }



    /// Presentation units and AAC preroll are shared by pull and index seeks.
    pub(crate) fn seek_media_ts(&self, ts: Timestamp) -> Result<Timestamp> {
        if ts.is_negative() {
            return seek_error(SeekErrorKind::OutOfRange);
        }
        match &self.audio_timeline {
            Some(timeline) => timeline.seek_media_ts(ts),
            None => Ok(ts),
        }
    }

    /// Resolve all fallible timing/extent work before publishing a new cursor.
    pub(crate) fn seek_sample<S: StreamSegment + ?Sized>(
        &mut self,
        seg: &S,
        seg_idx: usize,
        sample_num: u32,
        required_ts: Timestamp,
        object_len: Option<u64>,
    ) -> Result<SeekedTo> {
        let timing = seg.sample_timing(self.track_num, sample_num)?
            .ok_or(Error::SeekError(SeekErrorKind::OutOfRange))?;
        let actual_ts = match &self.audio_timeline {
            Some(timeline) => timeline.presentation_ts(timing.ts)?,
            None => Timestamp::try_from(timing.ts)
                .map_err(|_| Error::SeekError(SeekErrorKind::OutOfRange))?,
        };
        let desc = seg.sample_data(self.track_num, sample_num, true)?;
        let offset = desc.offset.ok_or(Error::SeekError(SeekErrorKind::OutOfRange))?;
        let position = desc.base_pos.checked_add(offset)
            .ok_or(Error::DecodeError("isomp4: seek position overflow"))?;
        let end = position.checked_add(u64::from(desc.size))
            .ok_or(Error::DecodeError("isomp4: seek sample end overflow"))?;
        if object_len.is_some_and(|len| end > len) {
            return decode_error("isomp4: seek sample exceeds object");
        }
        self.cur_seg = seg_idx;
        self.next_sample = sample_num;
        self.next_sample_pos = position;
        self.timing_cursor = TimingCursor::default();
        self.timing_cursor_segment = seg_idx;
        Ok(SeekedTo { track_id: self.track_id, required_ts, actual_ts })
    }

    pub(crate) fn next_timing<S: StreamSegment + ?Sized>(
        &mut self, seg: &S, seg_idx: usize,
    ) -> Result<Option<SampleTiming>> {
        if self.timing_cursor_segment != seg_idx {
            self.timing_cursor = TimingCursor::default();
            self.timing_cursor_segment = seg_idx;
        }
        seg.sample_timing_cached(self.track_num, self.next_sample, &mut self.timing_cursor)
    }

    pub(crate) fn sample_info(
        &self, track: &Track, timing: SampleTiming, seg_idx: usize,
    ) -> Result<Option<NextSampleInfo>> {
        let tb = track.time_base.expect("track always created with a timebase");
        let (ts, dur, trim_start, trim_end) = if let Some(timeline) = &self.audio_timeline {
            let packet = timeline.packet(timing.ts, u64::from(timing.dur))?;
            (packet.pts, packet.dur, packet.trim_start, packet.trim_end)
        } else {
            let ts = Timestamp::try_from(timing.ts)
                .map_err(|_| Error::DecodeError("isomp4: sample timestamp overflow"))?;
            (ts, Duration::from(timing.dur), Duration::ZERO, Duration::ZERO)
        };
        let Some(time) = tb.calc_time(ts) else { return Ok(None); };
        Ok(Some(NextSampleInfo {
            track_num: self.track_num, track_id: self.track_id,
            ts, time, dur, trim_start, trim_end, seg_idx,
        }))
    }

    pub(crate) fn consume_sample<S: StreamSegment + ?Sized>(
        &mut self, info: &NextSampleInfo, seg: &S,
    ) -> Result<SampleDataInfo> {
        let desc = seg.sample_data(self.track_num, self.next_sample, false)?;
        // A chunk-start offset is cheap and authoritative, including a chunk
        // placed before the previous one. Otherwise keep the sequential cursor.
        let pos = match desc.offset {
            Some(offset) => desc.base_pos.checked_add(offset),
            None => Some(desc.base_pos.max(self.next_sample_pos)),
        }.ok_or(Error::DecodeError("isomp4: sample position overflow"))?;
        let next_pos = pos.checked_add(u64::from(desc.size))
            .ok_or(Error::DecodeError("isomp4: sample end overflow"))?;
        let next_sample = self.next_sample.checked_add(1)
            .ok_or(Error::DecodeError("isomp4: sample index overflow"))?;
        self.cur_seg = info.seg_idx;
        self.next_sample = next_sample;
        self.next_sample_pos = next_pos;
        Ok(SampleDataInfo {pos, len:desc.size})
    }

}

/// Information regarding the next sample.
#[derive(Debug)]
pub(crate) struct NextSampleInfo {
    /// The track number of the next sample.
    pub(crate) track_num: usize,
    /// The track id.
    pub(crate) track_id: u32,
    /// The timestamp of the next sample.
    pub(crate) ts: Timestamp,
    /// The timestamp expressed in seconds.
    pub(crate) time: Time,
    /// The duration of the next sample.
    pub(crate) dur: Duration,
    pub(crate) trim_start: Duration,
    pub(crate) trim_end: Duration,
    /// The segment containing the next sample.
    pub(crate) seg_idx: usize,
}

/// Information regarding a sample.
#[derive(Debug)]
pub(crate) struct SampleDataInfo {
    /// The position of the sample in the track.
    pub(crate) pos: u64,
    /// The length of the sample.
    pub(crate) len: u32,
}

/// A representation of time, defining a duration relative to a specific frequency
#[derive(Debug)]
pub struct TimeSpan {
    pub timescale: NonZero<u32>,
    pub duration: Duration,
}

impl Default for TimeSpan {
    fn default() -> Self {
        Self { timescale: NonZero::new(1).expect("1 is non-zero"), duration: Duration::ZERO }
    }
}

impl TimeSpan {
    pub fn new(timescale: NonZero<u32>, duration: Duration) -> Self {
        TimeSpan { timescale, duration }
    }
}

/// ISO Base Media File Format (MP4, M4A, MOV, etc.) demultiplexer.
///
/// `IsoMp4Reader` implements a demuxer for the ISO Base Media File Format.
pub struct IsoMp4Reader<'s> {
    iter: AtomIterator<MediaSourceStream<'s>>,
    media_info: MediaInfo,
    tracks: Vec<Track>,
    metadata: MetadataLog,
    /// Segments of the movie. Sorted in ascending order by sequence number.
    segs: Vec<Box<dyn StreamSegment>>,
    /// State tracker for each track.
    track_states: Vec<TrackState>,
    /// Optional, movie extends atom used for fragmented streams.
    moov: Arc<MoovAtom>,
}

impl<'s> IsoMp4Reader<'s> {
    pub fn try_new(mut mss: MediaSourceStream<'s>, opts: FormatOptions) -> Result<Self> {
        // To get to beginning of the atom.
        mss.seek_buffered_rel(-4);

        let is_seekable = mss.is_seekable();

        let mut ftyp = None;
        let mut moov = None;

        // Get the total length of the stream, if possible.
        let total_len = if is_seekable {
            let pos = mss.pos();
            let len = mss.seek(SeekFrom::End(0))?;
            mss.seek(SeekFrom::Start(pos))?;
            info!("stream is seekable with len={len} bytes.");
            Some(len)
        } else {
            None
        };

        let mut metadata = opts.external_data.metadata.unwrap_or_default();

        // Parse all atoms if the stream is seekable, otherwise parse all atoms up-to the mdat atom.
        let mut it = AtomIterator::new(mss, total_len).with_metadata_options(opts.metadata_options);
        // Maps each track id to its cumulative duration (TimeSpan) as parsed from the segment
        // index.
        let mut sidx_timespans: HashMap<u32, TimeSpan> = HashMap::new();

        while let Some(header) = it.next_header()? {
            // Top-level atoms.
            match header.atom_type() {
                AtomType::FileType => {
                    ftyp = Some(it.read_atom::<FtypAtom>()?);
                }
                AtomType::Movie => {
                    moov = Some(it.read_atom::<MoovAtom>()?);
                }
                AtomType::SegmentIndex => {
                    let sidx = it.read_atom::<SidxAtom>()?;

                    // Calculate the total duration, per track, from the segment index atoms.
                    let sidx_timespan = sidx_timespans
                        .entry(sidx.reference_id)
                        .or_insert(TimeSpan::new(sidx.timescale, Duration::ZERO));

                    if sidx_timespan.timescale != sidx.timescale {
                        return unsupported_error(
                            "isomp4: different sidx timescale for the same track",
                        );
                    }

                    // Don't overflow.
                    // TODO: Duration should maybe be None since SIDX is non-authoritative.
                    sidx_timespan.duration = sidx_timespan
                        .duration
                        .checked_add(Duration::new(sidx.total_duration))
                        .ok_or(Error::DecodeError("isomp4: sidx total duration overflow"))?
                }
                AtomType::MediaData | AtomType::MovieFragment => {
                    // The mdat atom contains the codec bitstream data. For fragmented streams, a
                    // moof + mdat pair is required. If the ftyp and moov atoms have been read, then
                    // the top-level atom scan can exit here and begin playback immediately as an
                    // optimization. If not, then the scan must continue.
                    //
                    // The scan must also exit if the source is unseekable because in that case
                    // the format reader cannot skip past these atoms without dropping packets.
                    let is_playable = moov.is_some() && ftyp.is_some();

                    if is_playable || !is_seekable {
                        if !is_playable {
                            warn!("mp4 is not streamable.");
                        }
                        break;
                    }
                }
                AtomType::Meta => {
                    // Read the metadata atom and append it to the log.
                    let mut meta = it.read_atom::<MetaAtom>()?;

                    if let Some(rev) = meta.take_metadata() {
                        metadata.push(rev);
                    }
                }
                AtomType::Free => (),
                AtomType::Skip => (),
                _ => {
                    info!("skipping top-level atom: {:?}.", header.atom_type());
                }
            }
        }

        if ftyp.is_none() {
            return unsupported_error("isomp4: missing ftyp atom");
        }

        if moov.is_none() {
            return unsupported_error("isomp4: missing moov atom");
        }

        // If the top-level atom scan iterated across the entire source (e.g., if moov was the last
        // atom), then the iterator must return to the first moof or mdat atom. This is only
        // possible if the source is seekable. If it's not, then the media will be effectively
        // unplayable.
        if is_seekable && it.pending().is_none() {
            let mut mss = it.into_inner();
            mss.seek(SeekFrom::Start(0))?;

            it = AtomIterator::new(mss, total_len);

            while let Some(header) = it.next_header()? {
                if let AtomType::MovieFragment | AtomType::MediaData = header.atom_type() {
                    break;
                }
            }
        }

        // Fragments (moof + mdat pairs) are streamed. So if the pending atom is a moof, seek the
        // iterator to the start of the moof atom.
        if let Some(atom) = it.pending() {
            if atom.atom_type() == AtomType::MovieFragment {
                it.seek_atom_start()?;
            }
        }

        let mut moov = moov.expect("moov is checked for None above");

        if moov.is_fragmented() {
            if !sidx_timespans.is_empty() {
                info!("stream is segmented with a segment index.");
            } else {
                info!("stream is segmented without a segment index.");
            }
        }

        if let Some(rev) = moov.take_metadata() {
            metadata.push(rev);
        }

        // Create a track and track state for each Track (trak) atom.
        let mut tracks = Vec::with_capacity(moov.traks.len());
        let mut track_states = Vec::with_capacity(moov.traks.len());

        for (t, trak) in moov.traks.iter().enumerate() {
            // Determine the timespan of the track.
            let timespan = if moov.is_fragmented() {
                // If fragmented, prefer the duration from the sidx, if it is provided. Otherwise,
                // fallback to the mdhd.
                sidx_timespans
                    .get(&trak.tkhd.id)
                    .map(|sidx_tspan| TimeSpan::new(sidx_tspan.timescale, sidx_tspan.duration))
                    .unwrap_or_else(|| {
                        TimeSpan::new(trak.mdia.mdhd.timescale, trak.mdia.mdhd.duration.into())
                    })
            } else {
                // The media sample table bounds the available media. AAC presentation
                // timing and a supported edit are applied in TrackState::make below.
                let duration = Duration::from(trak.mdia.minf.stbl.stts.total_duration);

                TimeSpan::new(trak.mdia.mdhd.timescale, duration)
            };

            let (track_state, track) =
                TrackState::make(t, trak, &timespan, moov.mvhd.timescale, moov.is_fragmented())?;

            tracks.push(track);
            track_states.push(track_state);
        }

        // The number of tracks specified in the moov atom must match the number in the mvex atom.
        if let Some(mvex) = &moov.mvex {
            if mvex.trexs.len() != moov.traks.len() {
                return decode_error("isomp4: mvex and moov track number mismatch");
            }
        }

        // The moov atom will be shared among all segments and the demuxer using an Arc.
        let moov = Arc::new(moov);

        let segs: Vec<Box<dyn StreamSegment>> = vec![Box::new(MoovSegment::new(moov.clone()))];

        // Populate media information.
        let mut media_info = MediaInfo::new();
        media_info.with_time_base(TimeBase::from_recip(moov.mvhd.timescale));
        media_info.with_duration(Duration::new(moov.mvhd.duration));

        Ok(IsoMp4Reader { iter: it, media_info, tracks, metadata, track_states, segs, moov })
    }

    /// Idempotently gets information regarding the next sample of the media stream. This function
    /// selects the next sample with the lowest timestamp of all tracks.
    fn next_sample_info(&mut self) -> Result<Option<NextSampleInfo>> {
        let mut earliest: Option<NextSampleInfo> = None;
        for (state, track) in self.track_states.iter_mut().zip(&self.tracks) {
            for (delta, seg) in self.segs[state.cur_seg..].iter().enumerate() {
                if let Some(timing) = state.next_timing(&**seg, delta + state.cur_seg)? {
                    let Some(info) = state.sample_info(track, timing, delta + state.cur_seg)? else {
                        return Ok(None);
                    };
                    if earliest.as_ref().is_none_or(|prev| prev.time > info.time) {
                        earliest = Some(info);
                    }
                    break;
                }
            }
        }
        Ok(earliest)
    }

    fn consume_next_sample(&mut self, info: &NextSampleInfo) -> Result<Option<SampleDataInfo>> {
        self.track_states[info.track_num].consume_sample(info, &*self.segs[info.seg_idx]).map(Some)
    }

    fn try_read_more_segments(&mut self) -> Result<bool> {
        // If all tracks ended in the last segment, then do not try to read anymore segments.
        //
        // Note, there will always be one segment because the moov atom was converted into a segment
        // when the reader was instantiated.
        let last_seg = self.segs.last().expect("always at least one segment from moov atom");
        if last_seg.all_tracks_ended() {
            return Ok(false);
        }

        // Continue iterating over atoms until a segment (a moof + mdat atom pair) is found. All
        // other atoms will be ignored.
        loop {
            let header = match self.iter.next_header() {
                Ok(Some(header)) => header,
                Ok(None) => break,
                // If fragmented, an EOF is the only way to truly detect the end of stream.
                Err(AtomError::Other(Error::IoError(err)))
                    if self.moov.is_fragmented()
                        && err.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    break;
                }
                // Passthrough other errors.
                Err(err) => return Err(err.into()),
            };

            match header.atom_type() {
                AtomType::MediaData => {
                    return Ok(true);
                }
                AtomType::MovieFragment => {
                    let moof = self.iter.read_atom::<MoofAtom>()?;

                    // A moof segment can only be created if the media is fragmented.
                    if self.moov.is_fragmented() {
                        // Get the last segment.
                        let last_seg =
                            self.segs.last().expect("always at least one segment from moov atom");

                        // Create a new segment for the moof atom.
                        let seg = MoofSegment::new(moof, self.moov.clone(), last_seg.as_ref());

                        // Segments should have a monotonic sequence number.
                        if seg.sequence_num() <= last_seg.sequence_num() {
                            warn!("moof fragment has a non-monotonic sequence number.");
                        }

                        // Push the segment.
                        self.segs.push(Box::new(seg));
                    } else {
                        return decode_error("isomp4: moof atom present without mvex atom");
                    }
                }
                _ => {
                    trace!("skipping atom: {:?}.", header.atom_type());
                }
            }
        }

        // If no atoms were returned above, then the end-of-stream has been reached.
        Ok(false)
    }

    fn seek_track_by_time(&mut self, track_num: usize, time: Time) -> Result<SeekedTo> {
        // Convert time to timestamp for the track.
        if let Some(track) = self.tracks.get(track_num) {
            let tb = track.time_base.expect("track always created with a timebase");
            let ts = tb.calc_timestamp(time).ok_or(Error::SeekError(SeekErrorKind::OutOfRange))?;
            self.seek_track_by_ts(track_num, ts)
        } else {
            seek_error(SeekErrorKind::Unseekable)
        }
    }

    fn seek_track_by_ts(&mut self, track_num: usize, ts: Timestamp) -> Result<SeekedTo> {
        debug!("seeking track_num={track_num} to frame_ts={ts}");

        struct SeekLocation {
            seg_idx: usize,
            sample_num: u32,
        }

        let required_ts = ts;
        let ts = self.track_states[track_num].seek_media_ts(ts)?;
        let mut seg_skip = 0;

        let seek_loc = 'locate: loop {
            // Iterate over all segments and attempt to find the segment and sample number that
            // contains the desired timestamp. Skip segments already examined.
            for (seg_idx, seg) in self.segs.iter().enumerate().skip(seg_skip) {
                if let Some(sample_num) = seg.ts_sample(track_num, ts.get() as u64)? {
                    break 'locate SeekLocation { seg_idx, sample_num };
                }

                // Mark the segment as examined.
                seg_skip = seg_idx + 1;
            }

            // Otherwise, try to read more segments from the stream.
            if !self.try_read_more_segments()? {
                return seek_error(SeekErrorKind::OutOfRange);
            }
        };

        let seg = &self.segs[seek_loc.seg_idx];

        let result = self.track_states[track_num].seek_sample(
            &**seg, seek_loc.seg_idx, seek_loc.sample_num, required_ts, None,
        )?;
        debug!(
            "seeked track_num={} (track_id={}) to packet_ts={} (delta={})",
            track_num, result.track_id, result.actual_ts,
            result.actual_ts.saturating_delta(required_ts),
        );
        Ok(result)
    }
}

impl Scoreable for IsoMp4Reader<'_> {
    fn score(_src: ScopedStream<&mut MediaSourceStream<'_>>) -> Result<Score> {
        Ok(Score::Supported(255))
    }
}

impl ProbeableFormat<'_> for IsoMp4Reader<'_> {
    fn try_probe_new(
        mss: MediaSourceStream<'_>,
        opts: FormatOptions,
    ) -> Result<Box<dyn FormatReader + '_>> {
        Ok(Box::new(IsoMp4Reader::try_new(mss, opts)?))
    }

    fn probe_data() -> &'static [ProbeFormatData] {
        &[support_format!(
            ISOMP4_FORMAT_INFO,
            &["mp4", "m4a", "m4p", "m4b", "m4r", "m4v", "mov"],
            &["video/mp4", "audio/m4a"],
            &[b"ftyp"] // Top-level atoms
        )]
    }
}

impl FormatReader for IsoMp4Reader<'_> {
    fn format_info(&self) -> &FormatInfo {
        &ISOMP4_FORMAT_INFO
    }

    fn media_info(&self) -> &MediaInfo {
        &self.media_info
    }

    fn next_packet(&mut self) -> Result<Option<Packet>> {
        // Get the index of the track with the next-nearest (minimum) timestamp.
        let next_sample_info = loop {
            // Using the current set of segments, try to get the next sample info.
            if let Some(info) = self.next_sample_info()? {
                break info;
            } else {
                // Preserve the raw-read anchor at terminal EOF. Consuming the
                // pending mdat here made a later backward seek fail to read.
                if self.segs.last().is_some_and(|seg| seg.all_tracks_ended()) {
                    return Ok(None);
                }
                // The inner reader of the atom iterator has been used/seeked around to read
                // packets, so resync the reader and iterator by seeking to the end of the current
                // pending atom. Under regular circumstances, no actual expensive seek operation is
                // performed since the reader should be at the end of the last iterated atom if we
                // are trying to read another.
                match self.iter.seek_atom_end() {
                    Ok(_) | Err(AtomError::NoPendingAtom) => (),
                    Err(_) => return decode_error("sync lost"),
                };

                // No more segments. If the stream is unseekable, it may be the case that there are
                // more segments coming. If the stream is seekable it might be fragmented and no
                // segments are found in the moov atom. Iterate atoms until a new segment is found
                // or the end-of-stream is reached
                if !self.try_read_more_segments()? {
                    return Ok(None);
                }
            }
        };

        // Get the position and length information of the next sample.
        let sample_info = match self.consume_next_sample(&next_sample_info)? {
            Some(s) => s,
            None => return decode_error("isomp4: failed to get next sample data"),
        };

        let data =
            self.iter.read_raw_boxed_slice_exact(sample_info.pos, sample_info.len as usize)?;

        let mut packet =
            Packet::new(next_sample_info.track_id, next_sample_info.ts, next_sample_info.dur, data);
        packet.trim_start = next_sample_info.trim_start;
        packet.trim_end = next_sample_info.trim_end;
        Ok(Some(packet))
    }

    fn metadata(&mut self) -> Metadata<'_> {
        self.metadata.metadata()
    }

    fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    fn seek(&mut self, _mode: SeekMode, to: SeekTo) -> Result<SeekedTo> {
        if self.tracks.is_empty() {
            return seek_error(SeekErrorKind::Unseekable);
        }

        match to {
            SeekTo::Timestamp { ts, track_id } => {
                // The seek timestamp is in timebase units specific to the selected track. Get the
                // selected track and use the timebase to convert the timestamp into time units so
                // that the other tracks can be seeked.
                if let Some((track_num, track)) =
                    self.tracks.iter().enumerate().find(|(_, track)| track.id == track_id)
                {
                    // Convert to time units.
                    let tb = match track.time_base {
                        Some(tb) => tb,
                        None => return seek_error(SeekErrorKind::Unseekable),
                    };
                    let time =
                        tb.calc_time(ts).ok_or(Error::SeekError(SeekErrorKind::Unseekable))?;

                    // Seek all tracks excluding the primary track to the desired time.
                    for t in 0..self.track_states.len() {
                        if t != track_num {
                            self.seek_track_by_time(t, time)?;
                        }
                    }

                    // Seek the primary track and return the result.
                    self.seek_track_by_ts(track_num, ts)
                } else {
                    seek_error(SeekErrorKind::InvalidTrack)
                }
            }
            SeekTo::Time { time, track_id } => {
                // If provided, find the track number of the track with the desired track_id, or
                // default to the first track.
                let track_num = match track_id {
                    Some(id) => self
                        .tracks
                        .iter()
                        .position(|track| track.id == id)
                        .ok_or(Error::SeekError(SeekErrorKind::InvalidTrack))?,
                    None => 0,
                };

                // Seek all tracks excluding the selected track and discard the result.
                for t in 0..self.track_states.len() {
                    if t != track_num {
                        self.seek_track_by_time(t, time)?;
                    }
                }

                // Seek the primary track and return the result.
                self.seek_track_by_time(track_num, time)
            }
        }
    }

    fn into_inner<'s>(self: Box<Self>) -> MediaSourceStream<'s>
    where
        Self: 's,
    {
        self.iter.into_inner()
    }
}

impl ReadAtom for MediaSourceStream<'_> {}

impl From<AtomError> for Error {
    fn from(value: AtomError) -> Self {
        // Map all atom iteration errors to decode errors.
        let msg = match value {
            AtomError::InvalidAtomSize => "isomp4: invalid atom size",
            AtomError::InvalidUtf8 => "isomp4: invalid utf-8 string",
            AtomError::MaximumDepthReached => "isomp4: maximum recursion depth reached",
            AtomError::NoParentAtom => "isomp4: no parent atom",
            AtomError::NoPendingAtom => "isomp4: no atom pending read",
            AtomError::Overrun => "isomp4: overrun while reading atom",
            AtomError::SeekOutOfRange => "isomp4: out-of-bounds seek for a non-seekable stream",
            AtomError::UnexpectedEndOfAtom => "isomp4: unexpected end of atom",
            AtomError::UnexpectedPosition => "isomp4: unexpected position",
            AtomError::UnexpectedUnknownSizeAtom => "isomp4: unknown size atom has sized parent",
            AtomError::UnexpectedReadOperation => "isomp4: unexpected read operation",
            AtomError::UnknownAtomSize => "isomp4: unknown atom size",
            AtomError::Other(err) => return err,
        };
        Error::DecodeError(msg)
    }
}

// fn convert_timescale(
//     duration: u64,
//     src_timescale: NonZero<u32>,
//     dst_timescale: NonZero<u32>,
// ) -> Duration {
//     if src_timescale == dst_timescale {
//         return Duration::from(duration);
//     }
//     Duration::from(
//         ((duration as u128 * dst_timescale.get() as u128) / src_timescale.get() as u128) as u64,
//     )
// }
