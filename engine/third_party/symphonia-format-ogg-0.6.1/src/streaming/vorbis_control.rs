//! Optional pinned-source presentation probe and replay. Page syntax, CRC,
//! packet durations and trimming remain in the same library implementation.
use super::*;
use symphonia_core::{
    errors::{Error, SeekErrorKind},
    formats::SeekedTo,
};

impl VorbisPacketizer {
    /// Enables bounded source requests. On NeedMore, service source_request()
    /// before supplying further sequential input. Ordinary new() stays single-pass.
    pub fn with_object_len(options: PacketizerOptions, length: u64) -> Self {
        let mut parser = Self::new(options);
        parser.object_len = Some(length);
        parser
    }
    pub fn source_request(&self) -> Option<u64> {
        match self.read_state {
            ReadState::Pending(offset, _) if !self.failed => Some(offset),
            _ => None,
        }
    }
    pub fn position(&self) -> u64 {
        self.position
    }
    pub fn resume_at(&mut self, offset: u64) -> Result<()> {
        let ReadState::Pending(expected, resume) = self.read_state else {
            return decode_error("ogg: no pending source request");
        };
        if self.failed || offset != expected {
            return decode_error("ogg: source request changed");
        }
        self.position = offset;
        self.read_state = match resume {
            Resume::Tail => ReadState::Tail,
            Resume::Packet => ReadState::Packet,
            Resume::Replay => ReadState::Media,
        };
        Ok(())
    }
    pub fn seek(&mut self, target: Timestamp) -> Result<SeekedTo> {
        let reject = || Error::SeekError(SeekErrorKind::OutOfRange);
        let (Some(frames), Some(origin)) = (self.frames, self.origin) else {
            return Err(Error::SeekError(SeekErrorKind::Unseekable));
        };
        if self.failed || self.object_len.is_none() {
            return Err(Error::SeekError(SeekErrorKind::Unseekable));
        }
        let frame = u64::try_from(target.get()).map_err(|_| reject())?;
        if frame >= frames {
            return Err(reject());
        }
        let required = origin.checked_add(target.get()).ok_or_else(reject)?;
        let serial = self.serial.ok_or_else(reject)?;
        // No allocation, source read or fallible mutation follows validation.
        // Keep the original Vec capacities; codec reset is owned by the caller.
        self.mapper = None;
        self.sequence = 0;
        self.lens.clear();
        self.ranges.clear();
        self.packets.clear();
        self.locations.clear();
        self.assembly.clear();
        self.completed.clear();
        self.incomplete = false;
        self.skipping_metadata = false;
        self.metadata_bytes = 0;
        self.current = 0;
        self.pending_page = 0;
        self.prev_end = None;
        self.eos = false;
        self.first_packet = true;
        self.read_state = ReadState::Pending(0, Resume::Replay);
        Ok(SeekedTo {
            track_id: serial,
            required_ts: Timestamp::new(required),
            actual_ts: Timestamp::new(origin),
        })
    }
    fn bind_frames(&mut self, frames: u64) -> Result<()> {
        if frames == 0 || frames > i64::MAX as u64 || self.frames.is_some_and(|n| n != frames) {
            return decode_error("ogg: source presentation changed");
        }
        self.frames = Some(frames);
        let track = self
            .mapper
            .as_mut()
            .ok_or(Error::DecodeError("ogg: source mapper missing"))?
            .track_mut();
        track.with_num_frames(frames);
        track.with_duration(Duration::new(frames));
        Ok(())
    }
    fn probe_tail(&mut self, input: &[u8], eof: bool) -> Result<PacketStep> {
        // Search at most 4096 bytes and verify at most one existing Ogg page
        // per call. Reuse the original lacing Vec and borrowed encoded window.
        let scan = input.len().saturating_sub(3).min(4096);
        let found = input[..scan + input.len().min(3)]
            .windows(4)
            .position(|w| w == b"OggS");
        let skip = match found {
            Some(0) => 0,
            Some(n) => n,
            None if scan != 0 => scan,
            None if !eof => return Ok(PacketStep::NeedMore),
            None => return decode_error("ogg: complete final source page missing"),
        };
        if skip != 0 {
            return Ok(PacketStep::Discard(skip));
        }
        let page = match crate::page::read_page_slice(input, &mut self.lens) {
            Ok(Some(page)) => page,
            Ok(None) if !eof => return Ok(PacketStep::NeedMore),
            _ => return Ok(PacketStep::Discard(1)),
        };
        let (consumed, page) = page;
        let header = page.header;
        let final_page = self.position.checked_add(consumed as u64) == self.object_len
            && header.is_last_page
            && !header.is_first_page
            && Some(header.serial) == self.serial
            && header.sequence >= self.sequence;
        if !final_page {
            return Ok(PacketStep::Discard(consumed));
        }
        let frames = i64::try_from(header.absgp)
            .ok()
            .and_then(|end| end.checked_sub(self.origin?))
            .and_then(|n| u64::try_from(n).ok())
            .ok_or(Error::DecodeError("ogg: invalid final source granule"))?;
        self.bind_frames(frames)?;
        self.read_state = ReadState::Pending(self.first_page, Resume::Packet);
        Ok(PacketStep::NeedMore)
    }
    pub(super) fn controlled_next(&mut self, input: &[u8], eof: bool) -> Result<PacketStep> {
        if self.object_len.is_none() {
            return self.next_inner(input, eof);
        }
        let step = match self.read_state {
            ReadState::Pending(_, _) => return Ok(PacketStep::NeedMore),
            ReadState::Tail => self.probe_tail(input, eof)?,
            ReadState::Packet => {
                // Restoring the first audio page may arrive in smaller chunks
                // than the page originally parsed before the tail probe.
                if input.len() < self.pending_page {
                    return if eof {
                        decode_error("ogg: truncated restored Vorbis page")
                    } else {
                        Ok(PacketStep::NeedMore)
                    };
                }
                self.read_state = ReadState::Media;
                self.ready_packet()
            }
            ReadState::Media => {
                let step = self.next_inner(input, eof)?;
                if matches!(step, PacketStep::Packet { .. }) && self.first_packet {
                    let duration = self
                        .packets
                        .iter()
                        .try_fold(0_u64, |total, p| total.checked_add(p.dur.get()))
                        .ok_or(Error::DecodeError("ogg: first page duration overflow"))?;
                    let origin =
                        self.prev_end
                            .ok_or(Error::DecodeError("ogg: first page timestamp missing"))?
                            .get()
                            .checked_sub(i64::try_from(duration).map_err(|_| {
                                Error::DecodeError("ogg: first page duration overflow")
                            })?)
                            .ok_or(Error::DecodeError("ogg: first source timestamp overflow"))?;
                    if self.origin.is_some_and(|old| old != origin) {
                        return decode_error("ogg: source origin changed");
                    }
                    self.origin = Some(origin);
                    self.first_packet = false;
                    self.first_page = self.position;
                    if let Some(frames) = self.frames {
                        self.bind_frames(frames)?;
                    } else if self.eos {
                        self.bind_frames(duration)?;
                    } else {
                        let offset = self
                            .object_len
                            .ok_or(Error::DecodeError("ogg: pinned source length missing"))?
                            .saturating_sub(crate::page::OGG_PAGE_MAX_SIZE as u64);
                        self.read_state = ReadState::Pending(offset, Resume::Tail);
                        return Ok(PacketStep::NeedMore);
                    }
                }
                step
            }
        };
        if let PacketStep::Discard(n) | PacketStep::Packet { consumed: n } = step {
            self.position = self
                .position
                .checked_add(n as u64)
                .ok_or(Error::DecodeError("ogg: source position overflow"))?;
        }
        Ok(step)
    }
}
