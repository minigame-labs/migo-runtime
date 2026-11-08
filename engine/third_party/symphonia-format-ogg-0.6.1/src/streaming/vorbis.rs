// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Single logical Vorbis stream. Complete packets borrow their page. Only
//! packets crossing a page use the two reusable assembly buffers. Large unused
//! comments are counted/skipped through CRC-verified pages without assembly.
use crate::{
    logical::stamp_page_packets,
    mappings::{self, MapResult, Mapper},
    page::PageHeader,
};
use std::ops::Range;
use symphonia_core::{
    errors::{Result, decode_error, unsupported_error},
    formats::Track,
    packet::{Packet, PacketRef},
    units::{Duration, Timestamp},
};

pub enum PacketStep {
    NeedMore,
    Discard(usize),
    // The caller retains input until consumed. Borrow packet() before advancing
    // again; it may refer to input or the reusable cross-page packet storage.
    Packet { consumed: usize },
    End,
}
#[derive(Clone, Copy)]
pub struct PacketizerOptions {
    pub max_setup_bytes: usize,
    pub max_packet_bytes: usize,
    pub max_metadata_bytes: u64,
}
impl Default for PacketizerOptions {
    fn default() -> Self {
        Self {
            max_setup_bytes: 1024 * 1024,
            max_packet_bytes: 64 * 1024,
            max_metadata_bytes: 4 * 1024 * 1024,
        }
    }
}
enum Location {
    Input(Range<usize>),
    Completed,
}
#[derive(Clone, Copy)]
enum Resume {
    Tail,
    Packet,
    Replay,
}
#[derive(Clone, Copy)]
enum ReadState {
    Media,
    Pending(u64, Resume),
    Tail,
    Packet,
}
pub struct VorbisPacketizer {
    options: PacketizerOptions,
    mapper: Option<mappings::VorbisMapper>,
    serial: Option<u32>,
    sequence: u32,
    lens: Vec<u16>,
    ranges: Vec<Range<usize>>,
    packets: Vec<Packet>,
    locations: Vec<Location>,
    assembly: Vec<u8>,
    completed: Vec<u8>,
    incomplete: bool,
    skipping_metadata: bool,
    metadata_bytes: u64,
    current: usize,
    pending_page: usize,
    prev_end: Option<Timestamp>,
    eos: bool,
    failed: bool,
    object_len: Option<u64>,
    position: u64,
    read_state: ReadState,
    first_packet: bool,
    first_page: u64,
    origin: Option<i64>,
    frames: Option<u64>,
}
impl VorbisPacketizer {
    pub fn new(options: PacketizerOptions) -> Self {
        Self {
            options,
            mapper: None,
            serial: None,
            sequence: 0,
            lens: Vec::with_capacity(255),
            ranges: Vec::with_capacity(255),
            packets: Vec::with_capacity(255),
            locations: Vec::with_capacity(255),
            assembly: Vec::new(),
            completed: Vec::new(),
            incomplete: false,
            skipping_metadata: false,
            metadata_bytes: 0,
            current: 0,
            pending_page: 0,
            prev_end: None,
            eos: false,
            failed: false,
            object_len: None,
            position: 0,
            read_state: ReadState::Media,
            first_packet: true,
            first_page: 0,
            origin: None,
            frames: None,
        }
    }
    pub fn track(&self) -> Option<&Track> {
        self.mapper
            .as_ref()
            .filter(|mapper| mapper.is_ready())
            .map(|mapper| mapper.track())
    }
    pub fn packet<'a>(&'a self, input: &'a [u8]) -> Option<PacketRef<'a>> {
        if self.pending_page == 0 || self.current >= self.packets.len() {
            return None;
        }
        let mut packet = self.packets[self.current].as_packet_ref();
        packet.data = match &self.locations[self.current] {
            Location::Input(range) => input.get(range.clone())?,
            Location::Completed => &self.completed,
        };
        Some(packet)
    }
    pub fn next(&mut self, input: &[u8], eof: bool) -> Result<PacketStep> {
        if self.failed {
            return decode_error("ogg: Vorbis packetizer previously failed");
        }
        let result = self.controlled_next(input, eof);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn charge_metadata(&mut self, len: usize) -> Result<()> {
        self.metadata_bytes = self
            .metadata_bytes
            .checked_add(len as u64)
            .filter(|n| *n <= self.options.max_metadata_bytes)
            .ok_or(symphonia_core::errors::Error::LimitError(
                "ogg: metadata byte budget exceeded",
            ))?;
        Ok(())
    }
    fn append_partial(&mut self, bytes: &[u8]) -> Result<()> {
        let limit = if self.mapper.as_ref().is_some_and(|m| m.is_ready()) {
            self.options.max_packet_bytes
        } else {
            self.options.max_setup_bytes
        };
        let len = self
            .assembly
            .len()
            .checked_add(bytes.len())
            .filter(|n| *n <= limit)
            .ok_or(symphonia_core::errors::Error::LimitError(
                "ogg: packet assembly working set exceeded",
            ))?;
        if self.assembly.capacity() < len {
            self.assembly
                .try_reserve_exact(len - self.assembly.len())
                .map_err(|_| {
                    symphonia_core::errors::Error::LimitError(
                        "ogg: packet assembly allocation refused",
                    )
                })?;
        }
        self.assembly.extend_from_slice(bytes);
        Ok(())
    }
    fn complete_packet(
        &mut self,
        input: &[u8],
        range: Range<usize>,
        continued: bool,
    ) -> Result<()> {
        if continued && self.skipping_metadata {
            self.charge_metadata(range.len())?;
            self.incomplete = false;
            self.skipping_metadata = false;
            return Ok(());
        }
        let location = if continued {
            self.append_partial(&input[range])?;
            std::mem::swap(&mut self.assembly, &mut self.completed);
            self.assembly.clear();
            self.incomplete = false;
            Location::Completed
        } else {
            if self
                .mapper
                .as_ref()
                .expect("identified mapper")
                .is_discardable_prefix(&input[range.clone()])
            {
                self.charge_metadata(range.len())?;
                self.mapper
                    .as_mut()
                    .unwrap()
                    .map_packet(&input[range.start..range.start + 7])?;
                return Ok(());
            }
            let limit = if self.mapper.as_ref().unwrap().is_ready() {
                self.options.max_packet_bytes
            } else {
                self.options.max_setup_bytes
            };
            if range.len() > limit {
                return Err(symphonia_core::errors::Error::LimitError(
                    "ogg: complete packet working set exceeded",
                ));
            }
            Location::Input(range)
        };
        let bytes = match &location {
            Location::Input(range) => &input[range.clone()],
            Location::Completed => &self.completed,
        };
        let mapper = self.mapper.as_mut().expect("identified mapper");
        match mapper.map_packet(bytes)? {
            MapResult::StreamData { dur, discard } => {
                if !mapper.is_ready() {
                    return decode_error("ogg: audio before complete Vorbis setup");
                }
                let mut packet = Packet::new(
                    self.serial.unwrap(),
                    Timestamp::ZERO,
                    dur,
                    Box::<[u8]>::default(),
                );
                packet.trim_start = discard;
                self.packets.push(packet);
                self.locations.push(location);
            }
            MapResult::Setup | MapResult::SideData { .. } => {}
            MapResult::Unknown => return unsupported_error("ogg: unsupported Vorbis packet"),
        }
        Ok(())
    }
    fn save_partial(&mut self, input: &[u8], range: Range<usize>) -> Result<()> {
        if self.skipping_metadata {
            self.charge_metadata(range.len())?;
            return Ok(());
        }
        if !self.incomplete
            && self
                .mapper
                .as_ref()
                .unwrap()
                .is_discardable_prefix(&input[range.clone()])
        {
            self.charge_metadata(range.len())?;
            self.mapper
                .as_mut()
                .unwrap()
                .map_packet(&input[range.start..range.start + 7])?;
            self.skipping_metadata = true;
            self.incomplete = true;
            return Ok(());
        }
        self.append_partial(&input[range])?;
        self.incomplete = true;
        Ok(())
    }
    fn ready_packet(&self) -> PacketStep {
        PacketStep::Packet {
            consumed: if self.current + 1 == self.packets.len() {
                self.pending_page
            } else {
                0
            },
        }
    }
    fn next_inner(&mut self, input: &[u8], eof: bool) -> Result<PacketStep> {
        if self.options.max_setup_bytes < 30 || self.options.max_packet_bytes == 0 {
            return unsupported_error("ogg: invalid packet working-set policy");
        }
        if self.pending_page != 0 {
            if self.current + 1 < self.packets.len() {
                self.current += 1;
                return Ok(self.ready_packet());
            }
            self.pending_page = 0;
        }
        if self.eos {
            if !input.is_empty() {
                return unsupported_error(
                    "ogg: chained or trailing logical streams are unsupported",
                );
            }
            return Ok(if eof {
                PacketStep::End
            } else {
                PacketStep::NeedMore
            });
        }
        if input.is_empty() {
            return if eof {
                decode_error("ogg: missing final Vorbis page")
            } else {
                Ok(PacketStep::NeedMore)
            };
        }
        self.packets.clear();
        self.locations.clear();
        self.completed.clear();
        self.ranges.clear();
        let Some((consumed, page)) = crate::page::read_page_slice(input, &mut self.lens)? else {
            return if eof {
                decode_error("ogg: truncated Vorbis page")
            } else {
                Ok(PacketStep::NeedMore)
            };
        };
        let header: PageHeader = page.header;
        for packet in page.packets() {
            let start = packet.as_ptr().addr() - input.as_ptr().addr();
            self.ranges.push(start..start + packet.len());
        }
        let partial = page.packets().partial_packet().map(|part| {
            let start = part.as_ptr().addr() - input.as_ptr().addr();
            start..start + part.len()
        });
        if self.mapper.is_none() {
            if !header.is_first_page
                || header.is_continuation
                || header.sequence != 0
                || self.ranges.len() != 1
                || partial.is_some()
                || header.is_last_page
            {
                return decode_error("ogg: malformed Vorbis identification page");
            }
            self.mapper = mappings::detect_vorbis(header.serial, &input[self.ranges[0].clone()])?;
            if self.mapper.is_none() {
                return unsupported_error("ogg: streaming profile requires Vorbis");
            }
            self.serial = Some(header.serial);
            self.sequence = header.sequence;
            return Ok(PacketStep::Discard(consumed));
        }
        if Some(header.serial) != self.serial || header.is_first_page {
            return unsupported_error("ogg: multiplexed or chained Vorbis is unsupported");
        }
        if header.sequence != self.sequence.wrapping_add(1) {
            return decode_error("ogg: page sequence discontinuity");
        }
        if header.is_continuation != self.incomplete {
            return decode_error("ogg: packet continuation mismatch");
        }
        self.sequence = header.sequence;
        for index in 0..self.ranges.len() {
            let range = self.ranges[index].clone();
            self.complete_packet(input, range, index == 0 && header.is_continuation)?;
        }
        if let Some(range) = partial {
            self.save_partial(input, range)?;
        }
        if header.is_last_page && self.incomplete {
            return decode_error("ogg: final page ends in a partial packet");
        }
        if !self.packets.is_empty() {
            if header.absgp > i64::MAX as u64 {
                return decode_error("ogg: missing valid audio granule");
            }
            let page_end = self.mapper.as_ref().unwrap().absgp_to_ts(header.absgp);
            if self.prev_end.is_some_and(|prev| page_end < prev) {
                return decode_error("ogg: non-monotonic audio granule");
            }
            let (mut dur, mut discard) = (Duration::ZERO, Duration::ZERO);
            for packet in &self.packets {
                dur = dur.checked_add(packet.dur).ok_or(
                    symphonia_core::errors::Error::DecodeError("ogg: duration overflow"),
                )?;
                discard = discard.checked_add(packet.trim_start).ok_or(
                    symphonia_core::errors::Error::DecodeError("ogg: discard overflow"),
                )?;
            }
            stamp_page_packets(
                &mut self.packets,
                self.prev_end,
                page_end,
                header.is_last_page && self.prev_end.is_none(),
                dur,
                discard,
            )?;
            self.prev_end = Some(page_end);
        }
        self.eos = header.is_last_page;
        if self.packets.is_empty() {
            return Ok(PacketStep::Discard(consumed));
        }
        self.current = 0;
        self.pending_page = consumed;
        Ok(self.ready_packet())
    }
}

#[path = "vorbis_control.rs"]
mod control;
