// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::atoms::limits::*;
use crate::atoms::{Atom, AtomHeader, AtomIterator, ReadAtom, Result, decode_error};

#[derive(Debug)]
pub struct SampleDurationEntry {
    pub sample_count: u32,
    pub sample_delta: u32,
}

/// Time-to-sample atom.
#[allow(dead_code)]
#[derive(Debug)]
pub struct SttsAtom {
    pub entries: Vec<SampleDurationEntry>,
    pub total_duration: u64,
}

/// Reusable state for sequential timing lookups. No per-packet allocation.
#[derive(Default)]
pub(crate) struct TimingCursor {
    row: usize,
    first_sample: u32,
    first_ts: u64,
    #[cfg(test)]
    visited: usize,
}

impl SttsAtom {
    /// Get the timestamp and duration for the sample indicated by `sample_num`. Note, `sample_num`
    /// is indexed relative to the `SttsAtom`. Complexity of this function in O(N).
    pub fn find_timing_for_sample(&self, sample_num: u32) -> Option<(u64, u32)> {
        self.find_timing_for_sample_cached(sample_num, &mut TimingCursor::default())
    }

    /// Sequential queries retain a cursor instead of rescanning old entries.
    /// Backward queries reset the cursor; this remains correct for seek/rewind.
    pub(crate) fn find_timing_for_sample_cached(
        &self, sample_num: u32, cursor: &mut TimingCursor,
    ) -> Option<(u64, u32)> {
        if sample_num < cursor.first_sample {
            cursor.row = 0;
            cursor.first_sample = 0;
            cursor.first_ts = 0;
        }
        while let Some(entry) = self.entries.get(cursor.row) {
            #[cfg(test)] { cursor.visited += 1; }
            let end = cursor.first_sample.checked_add(entry.sample_count)?;
            if sample_num < end {
                let offset = u64::from(sample_num - cursor.first_sample);
                let ts = cursor.first_ts.checked_add(u64::from(entry.sample_delta) * offset)?;
                return Some((ts, entry.sample_delta));
            }
            cursor.first_sample = end;
            cursor.first_ts = cursor.first_ts.checked_add(
                u64::from(entry.sample_count) * u64::from(entry.sample_delta)
            )?;
            cursor.row += 1;
        }
        None
    }

    /// Get the sample that contains the timestamp indicated by `ts`. Note, the returned `sample_num`
    /// is indexed relative to the `SttsAtom`. Complexity of this function in O(N).
    pub fn find_sample_for_timestamp(&self, ts: u64) -> Option<u32> {
        let mut ts_accum = 0;
        let mut sample_num = 0;

        for entry in &self.entries {
            let delta = u64::from(entry.sample_delta) * u64::from(entry.sample_count);

            if ts_accum + delta > ts {
                sample_num += ((ts - ts_accum) / u64::from(entry.sample_delta)) as u32;
                return Some(sample_num);
            }

            ts_accum += delta;
            sample_num += entry.sample_count;
        }

        None
    }
}

impl Atom for SttsAtom {
    fn read<R: ReadAtom>(it: &mut AtomIterator<R>, _header: &AtomHeader) -> Result<Self> {
        let (_, _) = it.read_extended_header()?;

        let entry_count = it.read_u32()?;

        // Limit the maximum initial capacity to prevent malicious files from using all the
        // available memory.
        let mut entries = Vec::with_capacity(MAX_TABLE_INITIAL_CAPACITY.min(entry_count as usize));

        let mut total_duration: u64 = 0;
        let mut total_samples: u32 = 0;

        for _ in 0..entry_count {
            let sample_count = it.read_u32()?;
            let sample_delta = it.read_u32()?;

            total_samples = match total_samples.checked_add(sample_count) {
                Some(total) => total,
                None => return decode_error("isomp4 (stts): sample count overflow"),
            };

            let Some(next_duration) =
                total_duration.checked_add(u64::from(sample_count) * u64::from(sample_delta))
            else {
                return decode_error("isomp4 (stts): total duration overflow");
            };
            total_duration = next_duration;

            entries.push(SampleDurationEntry { sample_count, sample_delta });
        }

        Ok(SttsAtom { entries, total_duration })
    }
}

#[cfg(test)]
mod cursor_contracts {
    use super::*;

    #[test]
    fn sequential_timing_visits_entries_linearly_and_seeks_stay_correct() {
        let rows = 4096_u32;
        let table = SttsAtom {
            entries: (0..rows).map(|n| SampleDurationEntry {
                sample_count: 2, sample_delta: 1024 + (n % 2),
            }).collect(),
            total_duration: 0,
        };
        let mut cursor = TimingCursor::default();
        let mut expected_ts = 0_u64;
        for sample in 0..rows*2 {
            let delta = 1024 + ((sample / 2) % 2);
            assert_eq!(table.find_timing_for_sample_cached(sample, &mut cursor), Some((expected_ts, delta)));
            expected_ts += u64::from(delta);
        }
        assert!(table.find_timing_for_sample_cached(rows*2, &mut cursor).is_none());
        assert!(cursor.visited <= rows as usize * 3, "timing lookup rescanned old entries");
        for sample in [0, rows*2-1, 17, 17, 16, rows, 1] {
            let pairs = u64::from(sample / 4);
            let mut ts = pairs * (2*1024 + 2*1025);
            for n in (sample/4)*4..sample { ts += u64::from(1024 + ((n/2)%2)); }
            assert_eq!(table.find_timing_for_sample_cached(sample, &mut cursor), Some((ts, 1024 + ((sample/2)%2))));
        }
    }
}
