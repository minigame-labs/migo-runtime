//! Rust 1.95.0 std/hashbrown 0.16.1 allocation layout, not entry-byte estimates.
//! build.rs pins the compiler and selects the *standard target's* control group.
//! The independent allocator test observes both allocation and actual free bytes.
use shared::error::EngineResult;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Layout {
    pub(crate) bytes: usize,
    pub(crate) capacity: usize,
}
impl Layout {
    pub(crate) fn for_entries<T>(entries: usize) -> EngineResult<Self> {
        if entries == 0 {
            return Ok(Self::default());
        }
        let group = match env!("MIGO_HASH_GROUP_WIDTH") {
            "4" => 4usize,
            "8" => 8,
            "16" => 16,
            _ => unreachable!("unreviewed standard hash group"),
        };
        let size = std::mem::size_of::<T>();
        let align = std::mem::align_of::<T>().max(group);
        let minimum = match (group, size) {
            (16, 0..=1) => 14,
            (16, 2..=3) | (8, 0..=1) => 7,
            _ => 3,
        };
        let requested = entries.max(minimum);
        let buckets = if requested < 4 {
            4
        } else if requested < 8 {
            8
        } else if requested < 15 {
            16
        } else {
            requested
                .checked_mul(8)
                .map(|n| n / 7)
                .and_then(usize::checked_next_power_of_two)
                .ok_or_else(crate::node_storage::size_overflow)?
        };
        let offset = size
            .checked_mul(buckets)
            .and_then(|n| n.checked_add(align - 1))
            .map(|n| n & !(align - 1))
            .ok_or_else(crate::node_storage::size_overflow)?;
        let bytes = offset
            .checked_add(buckets)
            .and_then(|n| n.checked_add(group))
            .filter(|n| *n <= isize::MAX as usize - (align - 1))
            .ok_or_else(crate::node_storage::size_overflow)?;
        let capacity = if buckets < 16 {
            buckets - 1
        } else {
            buckets / 8 * 7
        };
        Ok(Self { bytes, capacity })
    }
}
