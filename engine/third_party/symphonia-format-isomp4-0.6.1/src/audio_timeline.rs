// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! AAC-LC presentation windows, using the already parsed MP4 timing tables.
//! Packet and public track timing are normalized to PCM frames so decoder trim
//! fields have the same units as other Symphonia audio demuxers. No encoded
//! priming packets are skipped: the decoder still needs their overlap state.
use std::num::NonZero;

use symphonia_core::errors::{Error, Result, decode_error, unsupported_error};
use symphonia_core::units::{Duration, Timestamp};

use crate::atoms::ElstAtom;

#[derive(Debug)]
pub(crate) struct AudioTimeline {
    media_scale: NonZero<u32>,
    pub rate: NonZero<u32>,
    block_frames: u64,
    media_frames: u64,
    start: u64,
    end: u64,
}

#[derive(Debug)]
pub(crate) struct PacketTiming {
    pub pts: Timestamp,
    pub dur: Duration,
    pub trim_start: Duration,
    pub trim_end: Duration,
}

// u128 holds every u64 * u32 product without overflow. Conversion back to u64
// is checked; do not round through floating point or saturate corrupt timing.
fn scale(value: u64, from: NonZero<u32>, to: NonZero<u32>, exact: bool) -> Result<u64> {
    let numerator = u128::from(value) * u128::from(to.get());
    let denominator = u128::from(from.get());
    if exact && numerator % denominator != 0 {
        return unsupported_error("isomp4: AAC media timing is not sample aligned");
    }
    u64::try_from(numerator / denominator)
        .map_err(|_| Error::DecodeError("isomp4: audio time conversion overflow"))
}

impl AudioTimeline {
    pub fn new(
        media_scale: NonZero<u32>,
        rate: NonZero<u32>,
        movie_scale: NonZero<u32>,
        media_duration: u64,
        block_frames: u64,
        edits: Option<&ElstAtom>,
    ) -> Result<Self> {
        if block_frames == 0 {
            return decode_error("isomp4: zero AAC block duration");
        }
        let media_frames = scale(media_duration, media_scale, rate, true)?;
        let mut start = 0;
        let mut end = media_frames;
        if let Some(edits) = edits.filter(|edits| !edits.entries.is_empty()) {
            if edits.entries.len() != 1 {
                return unsupported_error("isomp4: multiple audio edits are not supported");
            }
            let edit = &edits.entries[0];
            if edit.media_time == -1 {
                return unsupported_error("isomp4: empty audio edits are not supported");
            }
            if edit.media_time < 0 {
                return decode_error("isomp4: invalid negative audio edit media time");
            }
            if edit.media_rate_int != 1 || edit.media_rate_frac != 0 {
                return unsupported_error("isomp4: audio edit rate must be one");
            }
            start = scale(edit.media_time as u64, media_scale, rate, true)?;
            // Movie timescales are often coarse. Keep complete PCM frames in
            // the edit, and clamp to authoritative media sample-table duration.
            let duration = scale(edit.segment_duration, movie_scale, rate, false)?;
            end = start
                .checked_add(duration)
                .ok_or(Error::DecodeError("isomp4: audio edit end overflow"))?
                .min(media_frames);
        }
        if start >= end || end > i64::MAX as u64 || media_frames > i64::MAX as u64 {
            return decode_error("isomp4: invalid or empty audio presentation window");
        }
        Ok(Self { media_scale, rate, block_frames, media_frames, start, end })
    }

    pub fn frames(&self) -> u64 {
        self.end - self.start
    }

    pub fn delay(&self) -> Option<u32> {
        self.start.try_into().ok()
    }

    pub fn packet(&self, media_ts: u64, media_dur: u64) -> Result<PacketTiming> {
        let ts = scale(media_ts, self.media_scale, self.rate, true)?;
        let dur = scale(media_dur, self.media_scale, self.rate, true)?;
        if dur == 0
            || dur > self.block_frames
            || ts.checked_add(dur).is_none_or(|end| end > self.media_frames)
        {
            return decode_error("isomp4: inconsistent AAC sample duration");
        }
        // Intersect this packet's valid media with the edit. Remaining codec
        // block frames are padding even when stts describes a short last sample.
        let trim_start = self.start.saturating_sub(ts).min(dur);
        let valid_end = self.end.saturating_sub(ts).min(dur).max(trim_start);
        let valid = valid_end - trim_start;
        let trim_end = self.block_frames - trim_start - valid;
        let pts = i64::try_from(ts)
            .ok()
            .and_then(|ts| ts.checked_sub(self.start as i64))
            .ok_or(Error::DecodeError("isomp4: audio packet timestamp overflow"))?;
        Ok(PacketTiming {
            pts: Timestamp::new(pts),
            dur: Duration::new(valid),
            trim_start: Duration::new(trim_start),
            trim_end: Duration::new(trim_end),
        })
    }

    /// Convert a presentation seek to media ticks with one AAC block of preroll.
    /// The caller resets the decoder, decodes preroll, and discards output before
    /// required_ts, as with a coarse seek in other Symphonia demuxers.
    pub fn seek_media_ts(&self, presentation_ts: Timestamp) -> Result<Timestamp> {
        let ts = u64::try_from(presentation_ts.get())
            .map_err(|_| Error::SeekError(symphonia_core::errors::SeekErrorKind::OutOfRange))?;
        if ts >= self.frames() {
            return Err(Error::SeekError(symphonia_core::errors::SeekErrorKind::OutOfRange));
        }
        let media_frame = (self.start + ts).saturating_sub(self.block_frames);
        let media_ts = scale(media_frame, self.rate, self.media_scale, false)?;
        Timestamp::try_from(media_ts)
            .map_err(|_| Error::DecodeError("isomp4: audio seek timestamp overflow"))
    }

    pub fn presentation_ts(&self, media_ts: u64) -> Result<Timestamp> {
        let frame = scale(media_ts, self.media_scale, self.rate, true)?;
        let ts = i64::try_from(frame)
            .ok()
            .and_then(|frame| frame.checked_sub(self.start as i64))
            .ok_or(Error::DecodeError("isomp4: audio seek timestamp overflow"))?;
        Ok(Timestamp::new(ts))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atoms::elst::ElstEntry;

    fn nz(v: u32) -> NonZero<u32> {
        NonZero::new(v).unwrap()
    }
    fn edit(duration: u64, start: i64) -> ElstAtom {
        ElstAtom {
            entries: vec![ElstEntry {
                segment_duration: duration,
                media_time: start,
                media_rate_int: 1,
                media_rate_frac: 0,
            }],
        }
    }
    fn timeline(edits: Option<&ElstAtom>) -> Result<AudioTimeline> {
        AudioTimeline::new(nz(44100), nz(44100), nz(1000), 9981, 1024, edits)
    }

    #[test]
    fn delay_and_short_last_packet_come_from_container() {
        let t = timeline(Some(&edit(204, 1024))).unwrap();
        assert_eq!(t.frames(), 8957);
        let first = t.packet(0, 1024).unwrap();
        assert_eq!((first.pts.get(), first.dur.get(), first.trim_start.get()), (-1024, 0, 1024));
        let last = t.packet(9216, 765).unwrap();
        assert_eq!((last.dur.get(), last.trim_end.get()), (765, 259));
    }

    #[test]
    fn nonstandard_delay_partial_packet_and_edit_end() {
        let t = timeline(Some(&edit(100, 211))).unwrap();
        assert_eq!(t.frames(), 4410);
        let first = t.packet(0, 1024).unwrap();
        assert_eq!((first.trim_start.get(), first.dur.get()), (211, 813));
        let last = t.packet(4096, 1024).unwrap();
        assert_eq!((last.dur.get(), last.trim_end.get()), (525, 499));
        let after = t.packet(5120, 1024).unwrap();
        assert_eq!((after.dur.get(), after.trim_end.get()), (0, 1024));
    }

    #[test]
    fn no_edit_still_discards_short_packet_codec_padding() {
        let t = timeline(None).unwrap();
        assert_eq!(t.frames(), 9981);
        let last = t.packet(9216, 765).unwrap();
        assert_eq!((last.dur.get(), last.trim_start.get(), last.trim_end.get()), (765, 0, 259));
    }

    #[test]
    fn exact_different_media_timebase_is_normalized_to_samples() {
        let t = AudioTimeline::new(
            nz(24000),
            nz(48000),
            nz(90000),
            5500,
            1024,
            Some(&edit(18000, 512)),
        )
        .unwrap();
        assert_eq!(t.frames(), 9600);
        let first = t.packet(0, 512).unwrap();
        assert_eq!((first.pts.get(), first.trim_start.get(), first.dur.get()), (-1024, 1024, 0));
        assert_eq!(t.seek_media_ts(Timestamp::new(1024)).unwrap().get(), 512);
        assert_eq!(t.presentation_ts(512).unwrap(), Timestamp::ZERO);
    }

    #[test]
    fn fractional_movie_duration_keeps_complete_frames() {
        let t = timeline(Some(&edit(1, 0))).unwrap();
        assert_eq!(t.frames(), 44);
    }

    #[test]
    fn seek_uses_edit_offset_and_preroll_and_rejects_eof() {
        let t = timeline(Some(&edit(204, 1024))).unwrap();
        assert_eq!(t.seek_media_ts(Timestamp::ZERO).unwrap(), Timestamp::ZERO);
        assert_eq!(t.seek_media_ts(Timestamp::new(3000)).unwrap().get(), 3000);
        assert!(t.seek_media_ts(Timestamp::new(-1)).is_err());
        assert!(t.seek_media_ts(Timestamp::new(8957)).is_err());
    }

    #[test]
    fn unsupported_or_invalid_edits_fail_explicitly() {
        for start in [-2, -1, 9981, i64::MAX] {
            assert!(timeline(Some(&edit(204, start))).is_err());
        }
        assert!(timeline(Some(&edit(0, 0))).is_err());
        let mut edits = edit(204, 0);
        edits.entries[0].media_rate_int = 0;
        assert!(timeline(Some(&edits)).is_err());
        edits.entries[0].media_rate_int = 1;
        edits.entries[0].media_rate_frac = 1;
        assert!(timeline(Some(&edits)).is_err());
        edits.entries[0].media_rate_frac = 0;
        edits.entries.push(edit(1, 0).entries.remove(0));
        assert!(timeline(Some(&edits)).is_err());
        assert!(timeline(Some(&ElstAtom { entries: vec![] })).is_ok());
    }

    #[test]
    fn overflow_fractional_media_and_inconsistent_blocks_are_errors() {
        assert!(timeline(Some(&edit(u64::MAX, 0))).is_err());
        assert!(AudioTimeline::new(nz(1), nz(u32::MAX), nz(1), u64::MAX, 1024, None).is_err());
        assert!(AudioTimeline::new(nz(48000), nz(44100), nz(1000), 1, 1024, None).is_err());
        let t = timeline(None).unwrap();
        for (ts, dur) in [(0, 0), (0, 1025), (9980, 2), (u64::MAX, 1)] {
            assert!(t.packet(ts, dur).is_err());
        }
    }
}
