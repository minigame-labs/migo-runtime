// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![warn(rust_2018_idioms)]
// The following lints are allowed in all Symphonia crates. Please see clippy.toml for their
// justification.
#![allow(clippy::comparison_chain)]
#![allow(clippy::excessive_precision)]
#![allow(clippy::identity_op)]
#![allow(clippy::manual_range_contains)]

mod atoms;
mod audio_timeline;
mod demuxer;
mod fp;
mod stream;

pub use demuxer::IsoMp4Reader;

#[cfg(test)]
mod streaming_index_boundary_contracts {
    use crate::atoms::stsc::{StscAtom, StscEntry};
    use crate::atoms::stts::{SampleDurationEntry, SttsAtom};

    #[test]
    fn chunk_groups_include_their_first_sample() {
        let table = StscAtom {
            entries: vec![
                StscEntry {
                    first_chunk: 0,
                    first_sample: 0,
                    samples_per_chunk: 3,
                    sample_desc_index: 1,
                },
                StscEntry {
                    first_chunk: 2,
                    first_sample: 6,
                    samples_per_chunk: 2,
                    sample_desc_index: 1,
                },
                StscEntry {
                    first_chunk: 5,
                    first_sample: 12,
                    samples_per_chunk: 1,
                    sample_desc_index: 1,
                },
            ],
        };
        for sample in 0..18 {
            let expected = if sample < 6 {
                0
            } else if sample < 12 {
                2
            } else {
                5
            };
            assert_eq!(
                table.find_entry_for_sample(sample).unwrap().first_chunk,
                expected,
                "sample {sample}"
            );
        }
        assert!(
            StscAtom { entries: vec![] }
                .find_entry_for_sample(0)
                .is_none()
        );
    }

    #[test]
    fn sample_count_overflow_is_rejected_before_index_use() {
        use crate::atoms::AtomIterator;
        use std::io::Cursor;
        use symphonia_core::io::MediaSourceStream;
        for (first, second, overflow) in [(u32::MAX - 10, 10, false), (u32::MAX, 1, true)] {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(&32_u32.to_be_bytes());
            bytes.extend_from_slice(b"stts");
            bytes.extend_from_slice(&0_u32.to_be_bytes());
            bytes.extend_from_slice(&2_u32.to_be_bytes());
            for count in [first, second] {
                bytes.extend_from_slice(&count.to_be_bytes());
                bytes.extend_from_slice(&1024_u32.to_be_bytes());
            }
            let reader = MediaSourceStream::new(Box::new(Cursor::new(bytes)), Default::default());
            let mut atoms = AtomIterator::new(reader, Some(32));
            atoms
                .next_header()
                .map_err(symphonia_core::errors::Error::from)
                .unwrap()
                .unwrap();
            let parsed = atoms.read_atom::<SttsAtom>();
            assert_eq!(parsed.is_err(), overflow, "sample counts {first}+{second}");
        }
    }

    #[test]
    fn time_table_arithmetic_handles_valid_large_sample_indices() {
        let table = SttsAtom {
            entries: vec![
                SampleDurationEntry {
                    sample_count: u32::MAX - 10,
                    sample_delta: 1024,
                },
                SampleDurationEntry {
                    sample_count: 10,
                    sample_delta: 1024,
                },
            ],
            total_duration: u64::from(u32::MAX) * 1024,
        };
        for sample in [0, u32::MAX - 11, u32::MAX - 10, u32::MAX - 1] {
            let timestamp = u64::from(sample) * 1024;
            assert_eq!(
                table.find_timing_for_sample(sample),
                Some((timestamp, 1024))
            );
            assert_eq!(table.find_sample_for_timestamp(timestamp), Some(sample));
        }
        assert!(table.find_timing_for_sample(u32::MAX).is_none());
    }
}

#[cfg(feature="streaming")]
pub mod streaming;
