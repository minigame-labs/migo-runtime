//! Uploads staged ahead of the record that makes them.
//!
//! A record cannot be larger than a packet, and some uploads are: a 2048-square
//! RGBA texture is 16 MiB. The producer sends such an upload's bytes as
//! `OPR_STAGE_PAYLOAD` chunks, over as many packets as they take, and then the
//! upload's own record with `byte_length` = `STAGED_PAYLOAD`; see
//! `frame_wire::gl_resource::OPR_STAGE_PAYLOAD` for the wire's side.
//!
//! # Why the session holds one payload, not a table
//!
//! A WebGL upload takes effect where it is called, and its bytes are used once,
//! by the record right after them. So there is never more than one payload in
//! flight, and nothing names one: the stream's order is the name. A table of
//! ids would be a second ordering to keep consistent with the first.
//!
//! # What it costs in memory
//!
//! The staged bytes are capped at [`MAX_WEBGL_UPLOAD_BYTES`], the ceiling one
//! upload has on every lane, and reserved once, when the first chunk declares
//! the total. Taking them moves the vector into the command -- no copy -- so a
//! packet's commands can own at most what that packet's chunks carried plus the
//! one payload staged before it, and the credit window bounds the packets.

use shared::protocol::render_cmd::MAX_WEBGL_UPLOAD_BYTES;

use crate::resource::Payload;

/// The session's one staged upload.
#[derive(Debug, Default)]
pub struct StagedPayload {
    bytes: Vec<u8>,
    /// What the chunks declared; 0 while nothing is staged.
    total: usize,
    /// A chunk was out of order or too long, or the reservation failed. The
    /// bytes are already freed; the upload that takes this fails.
    spoiled: bool,
}

impl StagedPayload {
    pub const fn new() -> Self {
        Self {
            bytes: Vec::new(),
            total: 0,
            spoiled: false,
        }
    }

    /// One chunk: `chunk` belongs at `offset` of a payload of `total` bytes.
    ///
    /// Offset 0 starts a payload, dropping whatever was staged -- a producer
    /// that abandoned one has nothing else to say about it. Any other offset
    /// must be where the staged bytes end, for the same total.
    pub fn stage(&mut self, total: u32, offset: u32, chunk: Payload<'_>) {
        let (total, offset) = (total as usize, offset as usize);
        if offset == 0 {
            self.clear();
            self.total = total;
            if total == 0
                || total > MAX_WEBGL_UPLOAD_BYTES
                || self.bytes.try_reserve_exact(total).is_err()
            {
                self.spoil();
                return;
            }
        } else if self.spoiled || total != self.total || offset != self.bytes.len() {
            self.spoil();
            return;
        }
        if chunk.len() > total - self.bytes.len() {
            self.spoil();
            return;
        }
        chunk.extend_into(&mut self.bytes);
    }

    /// The staged bytes, if a payload is complete and unspoiled. Whatever the
    /// answer, nothing is staged afterwards.
    pub fn take(&mut self) -> Option<Vec<u8>> {
        let complete = !self.spoiled && self.total != 0 && self.bytes.len() == self.total;
        let bytes = std::mem::take(&mut self.bytes);
        self.total = 0;
        self.spoiled = false;
        complete.then_some(bytes)
    }

    /// Drop anything staged and its memory.
    pub fn clear(&mut self) {
        self.bytes = Vec::new();
        self.total = 0;
        self.spoiled = false;
    }

    /// Bytes held right now, for diagnostics and the tests that bound it.
    pub fn held_bytes(&self) -> usize {
        self.bytes.capacity()
    }

    fn spoil(&mut self) {
        self.bytes = Vec::new();
        self.spoiled = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(bytes: &[u8]) -> Payload<'_> {
        Payload::Bytes(bytes)
    }

    #[test]
    fn contiguous_chunks_make_a_payload_that_is_taken_once() {
        let mut staged = StagedPayload::new();
        staged.stage(6, 0, chunk(&[1, 2, 3]));
        staged.stage(6, 3, chunk(&[4, 5, 6]));
        assert_eq!(staged.take(), Some(vec![1, 2, 3, 4, 5, 6]));
        assert_eq!(staged.take(), None, "a payload is used by one upload");
        assert_eq!(staged.held_bytes(), 0);
    }

    #[test]
    fn words_are_read_as_their_little_endian_bytes() {
        let mut staged = StagedPayload::new();
        staged.stage(
            5,
            0,
            Payload::Words {
                words: &[0x0403_0201, 0x0000_0005],
                len: 5,
            },
        );
        assert_eq!(staged.take(), Some(vec![1, 2, 3, 4, 5]));
    }

    #[test]
    fn an_incomplete_payload_is_not_taken() {
        let mut staged = StagedPayload::new();
        staged.stage(6, 0, chunk(&[1, 2, 3]));
        assert_eq!(staged.take(), None);
        assert_eq!(staged.held_bytes(), 0);
    }

    #[test]
    fn a_gap_a_repeat_or_a_changed_total_spoils_the_payload_and_frees_it() {
        for (total, offset) in [(6, 4), (6, 2), (7, 3)] {
            let mut staged = StagedPayload::new();
            staged.stage(6, 0, chunk(&[1, 2, 3]));
            staged.stage(total, offset, chunk(&[4, 5, 6]));
            assert_eq!(staged.held_bytes(), 0, "{total} {offset}");
            staged.stage(6, 3, chunk(&[4, 5, 6]));
            assert_eq!(staged.take(), None, "{total} {offset}");
        }
    }

    #[test]
    fn a_chunk_past_the_total_spoils_the_payload() {
        let mut staged = StagedPayload::new();
        staged.stage(4, 0, chunk(&[1, 2, 3, 4, 5]));
        assert_eq!(staged.take(), None);
    }

    #[test]
    fn offset_zero_starts_over() {
        let mut staged = StagedPayload::new();
        staged.stage(6, 0, chunk(&[9, 9, 9]));
        staged.stage(2, 0, chunk(&[1, 2]));
        assert_eq!(staged.take(), Some(vec![1, 2]));
    }

    #[test]
    fn a_total_above_the_upload_ceiling_or_of_zero_is_refused_without_reserving() {
        for total in [0, MAX_WEBGL_UPLOAD_BYTES as u32 + 1, u32::MAX] {
            let mut staged = StagedPayload::new();
            staged.stage(total, 0, chunk(&[]));
            assert_eq!(staged.held_bytes(), 0, "{total}");
            assert_eq!(staged.take(), None, "{total}");
        }
    }

    #[test]
    fn the_first_chunk_reserves_the_whole_payload_once() {
        let mut staged = StagedPayload::new();
        staged.stage(1 << 20, 0, chunk(&[0; 1024]));
        let reserved = staged.held_bytes();
        assert_eq!(reserved, 1 << 20);
        staged.stage(1 << 20, 1024, chunk(&[0; 4096]));
        assert_eq!(
            staged.held_bytes(),
            reserved,
            "no chunk grows the allocation"
        );
    }
}
