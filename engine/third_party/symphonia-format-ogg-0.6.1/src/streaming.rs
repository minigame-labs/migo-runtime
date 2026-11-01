// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Incremental, verified Ogg pages borrowing caller input. Logical-stream
//! identification, packet assembly and audio EOS are separate responsibilities.
pub use crate::page::{OGG_PAGE_MAX_SIZE, Page, PageHeader};
use symphonia_core::errors::{Result, decode_error};

pub enum PageStep<'a> {
    NeedMore,
    Page { consumed: usize, page: Page<'a> },
    End,
}

pub struct PagePacketizer {
    packet_lens: Vec<u16>,
    failed: bool,
    finished: bool,
}
impl Default for PagePacketizer {
    fn default() -> Self {
        Self::new()
    }
}
impl PagePacketizer {
    pub fn new() -> Self {
        Self {
            packet_lens: Vec::with_capacity(255),
            failed: false,
            finished: false,
        }
    }
    pub fn next<'a>(&'a mut self, input: &'a [u8], eof: bool) -> Result<PageStep<'a>> {
        if self.failed {
            return decode_error("ogg: page packetizer previously failed");
        }
        if self.finished {
            if input.is_empty() && eof {
                return Ok(PageStep::End);
            }
            self.failed = true;
            return decode_error("ogg: input after page stream end");
        }
        if input.is_empty() {
            if !eof {
                return Ok(PageStep::NeedMore);
            }
            self.finished = true;
            return Ok(PageStep::End);
        }
        match crate::page::read_page_slice(input, &mut self.packet_lens) {
            Ok(Some((consumed, page))) => Ok(PageStep::Page { consumed, page }),
            Ok(None) if !eof => Ok(PageStep::NeedMore),
            Ok(None) => {
                self.failed = true;
                decode_error("ogg: truncated page")
            }
            Err(error) => {
                self.failed = true;
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::page::PageReader;
    use std::io::Cursor;
    use symphonia_core::io::MediaSourceStream;

    // Fixture-only independent bitwise checksum; production uses core Crc32.
    fn checksum(input: &[u8]) -> u32 {
        let mut crc = 0_u32;
        for (i, &byte) in input.iter().enumerate() {
            crc ^= u32::from(if (22..26).contains(&i) { 0 } else { byte }) << 24;
            for _ in 0..8 {
                crc = if crc & 0x80000000 != 0 {
                    (crc << 1) ^ 0x04c11db7
                } else {
                    crc << 1
                };
            }
        }
        crc
    }
    fn fixture(lacing: &[u8], flags: u8) -> Vec<u8> {
        assert!(lacing.len() <= 255);
        let mut file = b"OggS\0".to_vec();
        file.push(flags);
        file.extend_from_slice(&12345_u64.to_le_bytes());
        file.extend_from_slice(&0x12345678_u32.to_le_bytes());
        file.extend_from_slice(&7_u32.to_le_bytes());
        file.extend_from_slice(&0_u32.to_le_bytes());
        file.push(lacing.len() as u8);
        file.extend_from_slice(lacing);
        let n = lacing.iter().map(|v| usize::from(*v)).sum::<usize>();
        file.extend((0..n).map(|i| (i % 251) as u8));
        let crc = checksum(&file);
        file[22..26].copy_from_slice(&crc.to_le_bytes());
        file
    }
    #[test]
    fn complete_page_is_borrowed_and_matches_existing_pull_boundaries() {
        for lacing in [
            vec![],
            vec![3, 255, 255, 4, 0, 255],
            vec![0; 255],
            vec![255; 255],
            vec![254; 255],
        ] {
            let file = fixture(&lacing, 3);
            let mut source =
                MediaSourceStream::new(Box::new(Cursor::new(file.clone())), Default::default());
            let pull = PageReader::try_new(&mut source).unwrap();
            let expected = pull.page();
            let mut parser = PagePacketizer::new();
            let PageStep::Page { consumed, page } = parser.next(&file, false).unwrap() else {
                panic!("waited for future input");
            };
            assert_eq!(consumed, file.len());
            assert!(consumed <= OGG_PAGE_MAX_SIZE);
            assert_eq!(
                (
                    page.header.serial,
                    page.header.sequence,
                    page.header.absgp,
                    page.header.is_continuation,
                    page.header.is_first_page,
                    page.header.is_last_page
                ),
                (
                    expected.header.serial,
                    expected.header.sequence,
                    expected.header.absgp,
                    expected.header.is_continuation,
                    expected.header.is_first_page,
                    expected.header.is_last_page
                )
            );
            assert_eq!(
                page.packets().collect::<Vec<_>>(),
                expected.packets().collect::<Vec<_>>()
            );
            assert_eq!(
                page.packets().partial_packet(),
                expected.packets().partial_packet()
            );
            if let Some(first) = page.packets().next() {
                assert_eq!(first.as_ptr(), file[27 + lacing.len()..].as_ptr());
            }
        }
    }
    #[test]
    fn partial_input_is_transactional_and_never_touches_missing_fields() {
        let file = fixture(&[3, 255, 255, 4, 0, 255], 2);
        let mut parser = PagePacketizer::new();
        parser.next(&file, false).unwrap();
        let before = parser.packet_lens.clone();
        for end in 0..file.len() {
            assert!(matches!(
                parser.next(&file[..end], false).unwrap(),
                PageStep::NeedMore
            ));
            assert_eq!(
                parser.packet_lens, before,
                "partial input changed descriptors"
            );
        }
        assert!(matches!(
            parser.next(&file, false).unwrap(),
            PageStep::Page { .. }
        ));
    }
    #[test]
    fn truncated_corrupt_reserved_and_invalid_pages_fail_stickily() {
        let good = fixture(&[3, 255, 1], 4);
        let mut invalid = Vec::new();
        for n in [1, 26, 27, 28, good.len() - 1] {
            invalid.push(good[..n].to_vec());
        }
        for (position, byte) in [
            (0, b'X'),
            (4, 1),
            (5, 8),
            (22, 0xff),
            (good.len() - 1, 0xff),
        ] {
            let mut bad = good.clone();
            bad[position] = byte;
            invalid.push(bad);
        }
        for bad in invalid {
            let mut parser = PagePacketizer::new();
            assert!(parser.next(&bad, true).is_err());
            assert!(parser.next(&good, true).is_err(), "failure was not sticky");
        }
    }
    #[test]
    fn page_extents_and_transport_eof_are_distinct() {
        let a = fixture(&[3], 2);
        let b = fixture(&[1], 4);
        let mut body = a.clone();
        body.extend_from_slice(&b);
        let mut parser = PagePacketizer::new();
        let PageStep::Page { consumed, .. } = parser.next(&body, false).unwrap() else {
            panic!()
        };
        assert_eq!(consumed, a.len());
        let PageStep::Page { consumed, .. } = parser.next(&body[a.len()..], false).unwrap() else {
            panic!()
        };
        assert_eq!(consumed, b.len());
        assert!(matches!(
            parser.next(&[], false).unwrap(),
            PageStep::NeedMore
        ));
        assert!(matches!(parser.next(&[], true).unwrap(), PageStep::End));
        assert!(matches!(parser.next(&[], true).unwrap(), PageStep::End));
        assert!(parser.next(&a, false).is_err());
    }
}

#[path = "streaming/vorbis.rs"]
mod vorbis;
pub use vorbis::{VorbisPacketizer, PacketStep, PacketizerOptions};
