//! Allocation-reusing storage update for the per-canvas WebGL uniform cache.

use std::collections::HashMap;

const MAX_CACHED_UNIFORM_VALUE_BYTES: usize = 64 * 1024;

/// Compare-and-store against the shadow for `(program, location)`.
///
/// `location_count` is the number of consecutive location slots written by
/// this setter. A cached write records the half-open range
/// `[location, location + location_count)`. GL uniform arrays can be updated
/// through either their base location or an element location; retaining only
/// the starting key lets a later element write leave a stale whole-array entry
/// behind. Because locations are opaque and this layer has no reflection map,
/// writes involving multi-location ranges conservatively invalidate other
/// entries for the same program.
///
/// Returns `true` when the upload is not redundant and must be issued.
///
/// The value stored in the existing `HashMap<..., Vec<u8>>` carries a four-byte
/// little-endian location count followed by the raw GL payload.  The map type
/// belongs to `CanvasGLState` outside this module, so the metadata is kept in
/// the value without changing that public-in-module shape.
pub(crate) fn update(
    cache: &mut HashMap<(u32, u32), Vec<u8>>,
    maximum_entries: usize,
    program: u32,
    location: u32,
    location_count: u32,
    value: &[u8],
) -> bool {
    assert!(
        maximum_entries > 0,
        "uniform cache must retain at least one entry"
    );
    let key = (program, location);
    let write_end = location.saturating_add(location_count);
    cache.retain(|(cached_program, cached_location), stored| {
        if *cached_program != program || *cached_location == location {
            return true;
        }
        let Some(count_bytes) = stored.get(..4) else {
            // Entries are only produced by this module, but a malformed
            // shadow must never make us suppress a real GL write.
            return false;
        };
        let cached_count = u32::from_le_bytes([
            count_bytes[0],
            count_bytes[1],
            count_bytes[2],
            count_bytes[3],
        ]);
        // Uniform locations are opaque and this layer has no reflection table
        // mapping an element location back to its array. A multi-location
        // write can therefore alias any other same-program entry; invalidate
        // conservatively rather than risk stale driver state. Single-element
        // writes likewise invalidate retained multi-location ranges.
        if location_count > 0 && (location_count > 1 || cached_count > 1) {
            return false;
        }
        let cached_end = cached_location.saturating_add(cached_count);
        !(location < cached_end && *cached_location < write_end)
    });
    if value.len() > MAX_CACHED_UNIFORM_VALUE_BYTES {
        // Oversized values are never retained, but they still changed every
        // overlapping cached location and must invalidate those entries.
        cache.remove(&key);
        return true;
    }

    let mut encoded_count = [0u8; 4];
    encoded_count.copy_from_slice(&location_count.to_le_bytes());
    if let Some(stored) = cache.get_mut(&key) {
        if stored.get(4..) == Some(value) && stored.get(..4) == Some(&encoded_count) {
            return false;
        }
        // Same key, new bytes or range: reuse the allocation rather than
        // reinserting.
        stored.clear();
        stored.extend_from_slice(&encoded_count);
        stored.extend_from_slice(value);
        return true;
    }

    // New key. Eviction can no longer displace it — the lookup above proved
    // it absent — so only one arbitrary old entry is removed when full.
    if cache.len() >= maximum_entries
        && let Some(evicted) = cache.keys().next().copied()
    {
        cache.remove(&evicted);
    }
    let mut stored = Vec::with_capacity(4 + value.len());
    stored.extend_from_slice(&encoded_count);
    stored.extend_from_slice(value);
    cache.insert(key, stored);
    true
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::update;

    fn update_with_count(
        cache: &mut HashMap<(u32, u32), Vec<u8>>,
        maximum_entries: usize,
        program: u32,
        location: u32,
        location_count: u32,
        value: &[u8],
    ) -> bool {
        update(
            cache,
            maximum_entries,
            program,
            location,
            location_count,
            value,
        )
    }

    #[test]
    fn changed_value_reuses_existing_allocation() {
        let mut cache = HashMap::new();
        assert!(update_with_count(&mut cache, 8, 1, 5, 1, &[1, 2, 3, 4]));
        let first = cache.get(&(1, 5)).unwrap();
        let first_allocation = first.as_ptr();
        let first_capacity = first.capacity();

        assert!(update_with_count(&mut cache, 8, 1, 5, 1, &[5, 6, 7, 8]));
        let second = cache.get(&(1, 5)).unwrap();
        assert_eq!(second.as_ptr(), first_allocation);
        assert_eq!(second.capacity(), first_capacity);
        assert_eq!(&second[4..], &[5, 6, 7, 8]);
    }

    #[test]
    fn identical_value_is_deduplicated() {
        let mut cache = HashMap::new();
        assert!(update_with_count(&mut cache, 8, 1, 5, 1, &[1, 2]));
        assert!(!update_with_count(&mut cache, 8, 1, 5, 1, &[1, 2]));
    }

    #[test]
    fn cache_never_exceeds_configured_entry_limit() {
        let mut cache = HashMap::new();
        for location in 0..16 {
            assert!(update_with_count(
                &mut cache,
                4,
                1,
                location,
                1,
                &[location as u8]
            ));
            assert!(cache.len() <= 4);
        }
    }

    #[test]
    fn oversized_value_is_not_retained() {
        let mut cache = HashMap::new();
        let oversized = vec![7u8; 64 * 1024 + 1];

        assert!(update_with_count(&mut cache, 8, 1, 5, 1, &oversized));
        assert!(
            !cache.contains_key(&(1, 5)),
            "pathological uniform values must not become retained cache capacity"
        );
    }

    /// After `uniform1fv(loc=0, [a,b,c])` caches three f32s under `(prog, 0)`,
    /// a single-element write to `loc=1` (inside that range) must invalidate
    /// `(prog, 0)`.  Without invalidation the repeat of the full-array write
    /// compares equal to the stale cache entry and is silently skipped, leaving
    /// the driver with the element value written at `loc=1` — wrong pixels, no
    /// GL error.
    ///
    /// Demonstrates the R2 overlap bug: RED with the old single-key dedup,
    /// GREEN after range-aware invalidation.
    #[test]
    fn array_write_then_element_write_then_whole_array_must_reissue() {
        let mut cache = HashMap::new();
        let array_bytes: [u8; 12] = [1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0];
        let elem_bytes: [u8; 4] = [9, 0, 0, 0];

        assert!(update_with_count(&mut cache, 8, 1, 0, 3, &array_bytes));
        assert!(update_with_count(&mut cache, 8, 1, 1, 1, &elem_bytes));
        assert!(
            update_with_count(&mut cache, 8, 1, 0, 3, &array_bytes),
            "whole-array re-upload was suppressed even though loc=1 had changed \
             in the driver — stale dedup; paints with wrong values"
        );
    }

    /// The base name `u` and its first element `u[0]` resolve to the same
    /// location. A range-count change at that location must not be deduped
    /// merely because the payload bytes happen to match.
    #[test]
    fn base_location_and_first_element_alias_reissue_on_range_change() {
        let mut cache = HashMap::new();
        let first = [1u8, 2, 3, 4];
        let whole = [1u8, 2, 3, 4, 5, 6, 7, 8];

        assert!(update_with_count(&mut cache, 8, 1, 4, 2, &whole));
        // `u[0]` is the same location as `u`, but writes one element.
        assert!(update_with_count(&mut cache, 8, 1, 4, 1, &first));
        // Repeating the original base write must restore the array.
        assert!(update_with_count(&mut cache, 8, 1, 4, 2, &whole));
    }

    #[test]
    fn array_write_covering_cached_element_invalidates_it() {
        let mut cache = HashMap::new();
        let elem_bytes: [u8; 4] = [7, 0, 0, 0];
        let array_bytes: [u8; 12] = [1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0];

        assert!(update_with_count(&mut cache, 8, 1, 2, 1, &elem_bytes));
        assert!(update_with_count(&mut cache, 8, 1, 1, 3, &array_bytes));
        assert!(
            update_with_count(&mut cache, 8, 1, 2, 1, &elem_bytes),
            "scalar at loc=2 was not invalidated by the overlapping array write"
        );
    }

    /// Conservative invalidation may evict other same-program ranges, but it
    /// must retain the common case: setting the same complete array repeatedly
    /// with no intervening writes remains deduplicated.
    #[test]
    fn repeated_whole_array_value_still_deduplicates() {
        let mut cache = HashMap::new();
        let array: [u8; 8] = [1, 0, 0, 0, 2, 0, 0, 0];

        assert!(update_with_count(&mut cache, 8, 1, 0, 2, &array));
        assert!(!update_with_count(&mut cache, 8, 1, 0, 2, &array));
    }

    #[test]
    fn matrix_array_element_write_invalidates_whole_array() {
        let mut cache = HashMap::new();
        let mat2_bytes = vec![0u8; 1 + 2 * 64];
        let mut mat1_changed = vec![0u8; 1 + 64];
        mat1_changed[1] = 1;

        // A mat4 consumes four consecutive uniform locations, so two
        // matrices occupy [0, 8), while the second matrix starts at 4.
        assert!(update_with_count(&mut cache, 8, 1, 0, 8, &mat2_bytes));
        assert!(update_with_count(&mut cache, 8, 1, 4, 4, &mat1_changed));
        assert!(
            update_with_count(&mut cache, 8, 1, 0, 8, &mat2_bytes),
            "matrix array was not invalidated after element matrix write"
        );
    }
}
