use std::ffi::c_int;

use minimp3_sys as ffi;
use shared::error::{EngineError, EngineResult, ErrorCode};

use super::DecodedAudio;

/// Largest number of samples (all channels combined) one MP3 frame can decode
/// to. Mirrors the constant the `minimp3` wrapper crate derives from the same
/// `-sys` binding, which this crate does not depend on -- see the comment on
/// the `minimp3-sys` line in Cargo.toml for why.
pub(crate) const MAX_SAMPLES_PER_FRAME: usize = ffi::MINIMP3_MAX_SAMPLES_PER_FRAME as usize;

/// Largest MPEG-1 Layer III frame in bytes (320 kb/s at 32 kHz, padded).
const MAX_FRAME_BYTES: usize = 1441;

/// Bytes an MP3 frame header occupies.
const HEADER_BYTES: usize = 4;

/// Bytes that must stay unread behind the decode cursor while a stream's buffer
/// is still growing.
///
/// minimp3 keeps `mp3dec_t` -- the bit reservoir included -- only when it can
/// confirm the *next* frame's header in the same buffer; when it cannot, it
/// wipes the decoder and searches from scratch. Decoding right up to the end of
/// a partially received buffer therefore throws the reservoir away at every
/// chunk boundary, and an MP3 frame whose main data lives in that reservoir then
/// decodes to nothing at all. Two maximum frames of slack keeps the confirmation
/// available; at 128 kb/s that is under a quarter of a second of hold-back, well
/// inside the half second of buffering the player already waits for.
pub(crate) const STREAM_LOOKAHEAD_BYTES: usize = 2 * MAX_FRAME_BYTES;

/// What one call to [`Mp3FrameDecoder::decode`] did with the bytes it was given.
pub(crate) enum Mp3Step<'a> {
    /// A frame decoded. `pcm` is interleaved and borrowed from the decoder's own
    /// buffer, so it is valid until the next `decode`.
    Frame {
        pcm: &'a [i16],
        sample_rate: u32,
        channels: u32,
        consumed: usize,
    },
    /// Bytes carried no audio -- an ID3 tag, padding, or garbage -- and belong to
    /// nobody. Skipping them is progress.
    Skipped(usize),
    /// Not enough input to decide anything. The caller must keep what it has.
    NeedMoreData,
}

/// One minimp3 decoder that lives as long as the stream does, plus the fixed
/// buffer a frame decodes into.
///
/// **Both halves of that sentence are load-bearing.** Constructing a decoder per
/// chunk allocated three times over -- the 6.6 KiB `mp3dec_t`, a
/// virtual-memory-backed ring, and an 11 KiB refill buffer -- and, worse, reset
/// the bit reservoir and the MDCT overlap that MP3 frames are entitled to
/// inherit from their predecessors. Decoding into a buffer the decoder owns is
/// what removes the per-frame `Vec<i16>` the safe wrapper allocates and hands
/// back; at 1152 samples a frame that is one allocation every 26 ms of audio.
pub(crate) struct Mp3FrameDecoder {
    state: Box<ffi::mp3dec_t>,
    /// Sized by minimp3's documented maximum for a single frame, which is the
    /// contract the `pcm` pointer below is passed under.
    pcm: Box<[i16; MAX_SAMPLES_PER_FRAME]>,
}

impl Mp3FrameDecoder {
    pub(crate) fn new() -> Self {
        // `mp3dec_init` only invalidates the cached header; the rest of the
        // struct is the decoder's state and must start zeroed.
        let mut state = Box::new(ffi::mp3dec_t {
            mdct_overlap: [[0.0; 288]; 2],
            qmf_state: [0.0; 960],
            reserv: 0,
            free_format_bytes: 0,
            header: [0; 4],
            reserv_buf: [0; 511],
        });
        // SAFETY: `state` is a live, correctly aligned, fully initialised
        // `mp3dec_t` owned by this struct.
        unsafe { ffi::mp3dec_init(&mut *state) };

        Self {
            state,
            pcm: Box::new([0; MAX_SAMPLES_PER_FRAME]),
        }
    }

    /// Decode at most one frame from the front of `input`.
    ///
    /// The whole remaining buffer should be passed, not one frame's worth:
    /// minimp3's state-preserving fast path is conditional on the next header
    /// being visible.
    pub(crate) fn decode<'a>(&'a mut self, input: &[u8]) -> Mp3Step<'a> {
        if input.is_empty() {
            return Mp3Step::NeedMoreData;
        }

        let mut info = ffi::mp3dec_frame_info_t {
            frame_bytes: 0,
            frame_offset: 0,
            channels: 0,
            hz: 0,
            layer: 0,
            bitrate_kbps: 0,
        };

        // SAFETY: `input` is a readable slice of `len` bytes and `len` is its own
        // length clamped to what the C signature can carry; `pcm` points at
        // `MINIMP3_MAX_SAMPLES_PER_FRAME` writable samples, which is the maximum
        // one frame can produce and the size minimp3 documents for this pointer;
        // `state` is an initialised `mp3dec_t` owned here and borrowed uniquely
        // for this call. Nothing borrowed escapes the call.
        let len = input.len().min(c_int::MAX as usize) as c_int;
        let samples_per_channel = unsafe {
            ffi::mp3dec_decode_frame(
                &mut *self.state,
                input.as_ptr(),
                len,
                self.pcm.as_mut_ptr(),
                &mut info,
            )
        };

        let consumed = info.frame_bytes.max(0) as usize;

        if samples_per_channel <= 0 {
            // Two different answers arrive here and they must not be conflated.
            //
            // When minimp3 consumed *less* than it was given, it got past a real
            // frame it could not decode. Those bytes are behind us.
            //
            // When it claims everything it looked at, it means "nothing usable in
            // here" — and that is not the same as "these bytes are yours to
            // discard". A frame that simply has not arrived in full looks
            // identical from here. Any frame beginning within the last
            // `MAX_FRAME_BYTES` may still be incomplete, so that much is kept,
            // plus a header's worth minus one for a sync word straddling the
            // boundary. Everything older than that cannot be an incomplete
            // frame's start, because its frame would have fit.
            //
            // The bound is what keeps a stream of pure garbage from growing the
            // buffer without limit, and leaving nothing to skip is what stops
            // this from looping: the caller must wait for more input.
            let hold_back = if consumed >= input.len() {
                MAX_FRAME_BYTES + HEADER_BYTES - 1
            } else {
                0
            };
            return match consumed.saturating_sub(hold_back) {
                0 => Mp3Step::NeedMoreData,
                skip => Mp3Step::Skipped(skip),
            };
        }

        let channels = info.channels.max(1) as usize;
        let total = (samples_per_channel as usize).saturating_mul(channels);
        debug_assert!(total <= MAX_SAMPLES_PER_FRAME);

        Mp3Step::Frame {
            pcm: &self.pcm[..total.min(MAX_SAMPLES_PER_FRAME)],
            sample_rate: info.hz.max(0) as u32,
            channels: channels as u32,
            consumed,
        }
    }
}

impl Mp3FrameDecoder {
    /// Byte length of the frame at the front of `input`, without decoding it.
    ///
    /// minimp3 answers this directly when handed a null output pointer: it
    /// locates the frame, records its length, and returns before any of the work
    /// that depends on decoder state.
    ///
    /// **That last part is the whole reason this exists.** A frame whose main
    /// data lives in a bit reservoir the decoder does not have decodes to zero
    /// samples, which from outside is indistinguishable from garbage — so a probe
    /// that measured by decoding would report "no frame here" for exactly the
    /// frames worth rescuing.
    fn frame_bytes_at_front(&mut self, input: &[u8]) -> Option<usize> {
        if input.is_empty() {
            return None;
        }

        let mut info = ffi::mp3dec_frame_info_t {
            frame_bytes: 0,
            frame_offset: 0,
            channels: 0,
            hz: 0,
            layer: 0,
            bitrate_kbps: 0,
        };

        // SAFETY: as in `decode`, except for the output pointer. minimp3 checks
        // `pcm` for null and returns the frame's sample count before writing
        // anything through it, so passing null selects its measure-only path.
        let len = input.len().min(c_int::MAX as usize) as c_int;
        let samples = unsafe {
            ffi::mp3dec_decode_frame(
                &mut *self.state,
                input.as_ptr(),
                len,
                std::ptr::null_mut(),
                &mut info,
            )
        };

        (samples > 0 && info.frame_bytes > 0).then_some(info.frame_bytes as usize)
    }
}

/// How many false starts to tolerate while looking for where the audio begins.
///
/// A tag holds no sync words at all in the ordinary case, so a handful is
/// generous. The bound is what keeps a pathological file from costing a rescan
/// of the remainder per byte.
const MAX_SYNC_CANDIDATES: usize = 32;

/// Offset of the next byte pair that could begin a frame header.
///
/// Eleven set bits is the sync word; anything else cannot start a frame. This is
/// a candidate and not a verdict — [`first_frame_at_front`] decides by asking
/// minimp3.
fn next_sync_candidate(buffer: &[u8], from: usize) -> Option<usize> {
    (from..buffer.len().saturating_sub(1))
        .find(|&index| buffer[index] == 0xFF && (buffer[index + 1] & 0xE0) == 0xE0)
}

/// The length of an ID3v2 tag at the front of `buffer`, header and footer included, when one is
/// there whole.
///
/// The tag says how long it is, in a header no frame can be taken for: "ID3", a version below 0xFF,
/// and a size in four 7-bit bytes. Going past it by that length, rather than by searching for sync
/// words, is what keeps a tag's own bytes -- text, a cover image whose JPEG is full of `FF Ex`
/// markers -- from being measured as a frame: minimp3 accepts a lone frame with nothing after it to
/// confirm it.
fn id3v2_tag_length(buffer: &[u8]) -> Option<usize> {
    let header = buffer.get(..10)?;
    if &header[..3] != b"ID3"
        || header[3] == 0xFF
        || header[4] == 0xFF
        || header[6..].iter().any(|&b| b & 0x80 != 0)
    {
        return None;
    }
    let size = header[6..]
        .iter()
        .fold(0usize, |size, &b| (size << 7) | usize::from(b));
    let footer = if header[5] & 0x10 != 0 { 10 } else { 0 };
    let total = 10 + size + footer;
    (total <= buffer.len()).then_some(total)
}

/// Where the next whole frame starts in `buffer`, and how long it is.
///
/// Returns `(bytes before the frame, frame length)`. The caller must get past the
/// first before handing the second to its real decoder: minimp3's
/// accept-a-lone-frame rule requires the frame to start at offset zero.
///
/// Searching from a later offset is what gets past a leading tag bigger than a
/// frame — an ID3v2 tag at the front and an ID3v1 tag at the end is the classic
/// pairing, and together they hid the audio from both the in-place path and a
/// front-anchored probe.
pub(crate) fn first_frame_at_front(
    probe: &mut Mp3FrameDecoder,
    buffer: &[u8],
    hint: Option<usize>,
) -> Option<(usize, usize)> {
    let mut offset = id3v2_tag_length(buffer).unwrap_or(0);
    for _ in 0..=MAX_SYNC_CANDIDATES {
        if let Some(length) = first_frame_length(probe, &buffer[offset..], hint) {
            return Some((offset, length));
        }
        offset = next_sync_candidate(buffer, offset + 1)?;
    }
    None
}

/// Length of the frame at the front of `buffer`, if one is there in full.
///
/// **Why this exists.** minimp3 will not accept a frame it cannot chain to a
/// successor — except when the buffer it is handed is *exactly* one frame, which
/// it accepts outright. That exception is the only way past a stream whose next
/// bytes are a tag rather than a frame, and an ID3v1 tag is 128 bytes at the end
/// of a large fraction of real MP3 files.
///
/// Growing prefixes rather than a header table: asking minimp3 what a frame's
/// length is means duplicating its bitrate and sample-rate tables here, and a
/// second opinion about frame lengths is a second thing to be wrong. `hint` is
/// the previous frame's length, which a constant-bitrate stream repeats, so the
/// usual cost is one probe rather than a scan.
///
/// **The decoder passed in must be a throwaway.** Every rejected probe resets
/// minimp3's state, and the bit reservoir is precisely what the real decoder has
/// to keep.
pub(crate) fn first_frame_length(
    probe: &mut Mp3FrameDecoder,
    buffer: &[u8],
    hint: Option<usize>,
) -> Option<usize> {
    let mut ends_a_frame =
        |len: usize| len <= buffer.len() && probe.frame_bytes_at_front(&buffer[..len]) == Some(len);

    if let Some(len) = hint
        && ends_a_frame(len)
    {
        return Some(len);
    }

    // No frame can be longer than this, so failing to find one by here means
    // there is not a whole frame at the front.
    let limit = buffer.len().min(MAX_FRAME_BYTES);
    (HEADER_BYTES..=limit).find(|&len| ends_a_frame(len))
}

/// Convert one interleaved `i16` frame to normalized `f32`, appending.
#[inline]
pub(crate) fn append_as_f32(pcm: &[i16], out: &mut Vec<f32>) {
    out.extend(pcm.iter().map(|&sample| f32::from(sample) / 32768.0));
}

/// Decode a whole MP3 file.
///
/// **The decoder is handed exactly one frame at a time, and that is what makes a
/// file's frames survive its tags.** minimp3 will not accept a frame it cannot
/// chain to a successor, and what follows a file's final frame is not audio — an
/// ID3v1 tag is 128 bytes at the end of a large fraction of real files, usually
/// paired with an ID3v2 tag at the front. Offering the whole remainder lost the
/// last frame of a tagged file and lost *every* frame of a short one: a
/// two-second sound effect with an ID3v1 tag decoded to nothing at all, and
/// `decode` reported that it had produced no samples. Measured against the
/// previous implementation, which behaved identically in every case — this is a
/// defect being fixed, not one being introduced.
///
/// A lone frame is the one thing minimp3 accepts without a successor to confirm
/// it, and handing it one also keeps it on its state-preserving fast path, so the
/// bit reservoir a frame's main data may live in survives every step. The cost is
/// one extra call per frame to measure the frame's length, which returns before
/// any decoding work; see [`first_frame_at_front`].
pub fn decode(data: &[u8]) -> EngineResult<DecodedAudio> {
    let mut frames = Mp3FrameDecoder::new();
    let mut probe: Option<Mp3FrameDecoder> = None;
    let mut previous_length: Option<usize> = None;

    let mut samples: Vec<f32> = Vec::new();
    let mut sample_rate = 0u32;
    let mut channels = 0u32;
    let mut pos = 0usize;

    while pos < data.len() {
        let remaining = &data[pos..];
        let probe = probe.get_or_insert_with(Mp3FrameDecoder::new);
        let (take, measured) = match first_frame_at_front(probe, remaining, previous_length) {
            // Get past whatever sits in front of the frame first: the rule that
            // lets a lone frame through wants it at offset zero.
            Some((skip, _)) if skip > 0 => {
                pos += skip;
                continue;
            }
            Some((_, length)) => {
                previous_length = Some(length);
                (length, true)
            }
            // No whole frame ahead. Offering everything is the last thing to try
            // before giving up on the remainder.
            None => (remaining.len(), false),
        };

        match frames.decode(&remaining[..take]) {
            // A whole frame that gave nothing -- its main data in a reservoir this
            // decode never had -- is that frame lost. The frames after it carry
            // their own, and stopping here dropped every one of them.
            Mp3Step::NeedMoreData if measured => pos += take,
            Mp3Step::NeedMoreData => break,
            Mp3Step::Skipped(skipped) => pos += skipped,
            Mp3Step::Frame {
                pcm,
                sample_rate: sr,
                channels: ch,
                consumed,
            } => {
                pos += consumed;
                if sample_rate == 0 {
                    sample_rate = sr;
                    channels = ch;
                }

                // Reject a decode bomb before growing the buffer.
                if !crate::limits::pcm_samples_within_budget(
                    samples.len().saturating_add(pcm.len()),
                ) {
                    return Err(EngineError::from_detail(
                        ErrorCode::InvalidArgument,
                        "MP3 decode exceeds the PCM budget",
                    ));
                }

                append_as_f32(pcm, &mut samples);
            }
        }
    }

    if samples.is_empty() {
        return Err(EngineError::from_detail(
            ErrorCode::InvalidArgument,
            "MP3 decode produced no samples",
        ));
    }

    Ok(DecodedAudio {
        samples,
        sample_rate,
        channels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mp3_fixture;

    /// A whole file's frames must survive the tags wrapped around them.
    ///
    /// ID3v2 at the front and ID3v1 at the end is the classic pairing, and both
    /// defeat minimp3's requirement that a frame be chainable to a successor.
    /// Before the decoder was handed one frame at a time this lost the last frame
    /// of a long file and *every* frame of a short one — `decode` then reported
    /// that it had produced no samples, so a tagged sound effect failed to load
    /// rather than playing slightly short.
    ///
    /// One frame is included on purpose: a file with exactly one frame has no
    /// successor to offer under any circumstances, so it is the case the
    /// accept-a-lone-frame rule exists for.
    #[test]
    fn a_files_frames_survive_the_tags_around_them() {
        let id3v1 = {
            let mut tag = b"TAG".to_vec();
            tag.resize(128, 0);
            tag
        };
        // Larger than any frame, so it cannot be got past by probing from the
        // front and the sync search is what has to carry it.
        let id3v2 = vec![0u8; 4096];

        for frames in [1usize, 2, 6, 40] {
            for (label, leading, trailing) in [
                ("no tags", [].as_slice(), [].as_slice()),
                ("a trailing tag", [].as_slice(), id3v1.as_slice()),
                ("a leading tag", id3v2.as_slice(), [].as_slice()),
                ("tags at both ends", id3v2.as_slice(), id3v1.as_slice()),
            ] {
                let mut data = leading.to_vec();
                data.extend_from_slice(&mp3_fixture::stream(frames));
                data.extend_from_slice(trailing);

                let decoded = decode(&data)
                    .unwrap_or_else(|error| panic!("{frames} frames with {label}: {error}"));
                assert_eq!(
                    decoded.samples.len() / mp3_fixture::SAMPLES_PER_FRAME,
                    frames,
                    "a {frames}-frame file with {label} must decode every frame"
                );
            }
        }
    }

    /// A real ID3v2 tag: its header, then `body`, sized as the header says.
    fn id3v2(body: &[u8]) -> Vec<u8> {
        let n = body.len();
        let mut tag = b"ID3\x03\x00\x00".to_vec();
        tag.extend([
            (n >> 21) as u8 & 0x7F,
            (n >> 14) as u8 & 0x7F,
            (n >> 7) as u8 & 0x7F,
            n as u8 & 0x7F,
        ]);
        tag.extend_from_slice(body);
        tag
    }

    /// A tag holds bytes that look like a frame header -- text, a cover image, whose JPEG is full
    /// of `FF Ex` markers -- and it says how long it is.
    ///
    /// The frame search went by sync words alone, and minimp3 accepts a lone frame with nothing
    /// after it to confirm it, so a sync-like pair inside a tag measured as a frame. Its main data
    /// pointed into a reservoir nobody had, it decoded to nothing, and the decode stopped there: a
    /// Cocos Creator game's sound -- MPEG-2, 22.05 kHz, 2,746 frames behind a 67-byte tag with such
    /// a pair at offset 21 -- failed to load, and the game waited for it on its splash screen.
    #[test]
    fn a_tag_that_holds_a_frame_header_is_skipped_by_the_length_it_declares() {
        // An MPEG-1 Layer III header inside the tag, then side info: one whose main_data_begin
        // is not zero decodes to nothing, one that is all zero decodes to a frame of silence --
        // which is not audio either, and must not be counted as some.
        for (label, side_info) in [("pointing into a reservoir", 0xFFu8), ("silent", 0x00u8)] {
            let mut body = vec![0u8; 16];
            body.extend_from_slice(&[0xFF, 0xFB, 0x90, 0x00]);
            body.extend_from_slice(&[side_info; 32]);
            body.resize(600, 0);
            let mut data = id3v2(&body);
            data.extend_from_slice(&mp3_fixture::stream(6));

            let decoded = decode(&data)
                .unwrap_or_else(|e| panic!("the audio behind a {label} false frame: {e}"));
            assert_eq!(
                decoded.samples.len(),
                6 * mp3_fixture::SAMPLES_PER_FRAME,
                "every frame behind a tag holding a {label} false frame, and nothing else"
            );
        }
    }

    /// A tag header that claims more than the file holds is no tag to go past: the search for
    /// audio goes on from where it would without one, and finds what follows.
    #[test]
    fn a_tag_that_claims_more_than_the_file_holds_is_not_trusted() {
        let mut data = b"ID3\x03\x00\x00\x7F\x7F\x7F\x7F".to_vec();
        data.extend_from_slice(&mp3_fixture::stream(3));
        let decoded = decode(&data).expect("the frames after a header that lies about its length");
        assert_eq!(decoded.samples.len(), 3 * mp3_fixture::SAMPLES_PER_FRAME);
    }

    /// A frame that decodes to nothing -- its main data in a reservoir this decode never had -- is
    /// one frame lost, not the end of the file: the frames after it carry their own.
    #[test]
    fn a_frame_that_decodes_to_nothing_does_not_end_the_file() {
        // The fixture without its first frame: what is first now points into a reservoir that
        // was never filled.
        let stream = mp3_fixture::stream(6);
        let decoded =
            decode(&stream[mp3_fixture::FRAME_BYTES..]).expect("the frames after it decode");
        assert_eq!(
            decoded.samples.len(),
            4 * mp3_fixture::SAMPLES_PER_FRAME,
            "all but the frame that had nothing"
        );
    }

    /// The fixture has to be a real MP3 before anything built on it means
    /// something, so this is the control: a stream of frames decodes to exactly
    /// the sample count its headers promise.
    #[test]
    fn the_synthetic_stream_decodes_to_the_frames_its_headers_declare() {
        const FRAMES: usize = 4;
        let decoded = decode(&mp3_fixture::stream(FRAMES)).expect("fixture must decode");

        assert_eq!(decoded.sample_rate, mp3_fixture::SAMPLE_RATE);
        assert_eq!(decoded.channels, mp3_fixture::CHANNELS as u32);
        assert_eq!(
            decoded.samples.len(),
            FRAMES * mp3_fixture::SAMPLES_PER_FRAME,
            "every frame must have produced a full frame of samples"
        );
    }

    /// The decoder's state is what a frame's main data may live in, and a frame
    /// that points into a reservoir a reset decoder does not have decodes to
    /// nothing. Feeding the same bytes one frame at a time to one decoder must
    /// therefore give the same answer as feeding them all at once.
    #[test]
    fn a_persistent_decoder_carries_the_bit_reservoir_across_calls() {
        const FRAMES: usize = 4;
        let stream = mp3_fixture::stream(FRAMES);

        let mut persistent = Mp3FrameDecoder::new();
        let mut carried = 0usize;
        let mut pos = 0usize;
        while pos < stream.len() {
            match persistent.decode(&stream[pos..]) {
                Mp3Step::NeedMoreData => break,
                Mp3Step::Skipped(skipped) => pos += skipped,
                Mp3Step::Frame { pcm, consumed, .. } => {
                    pos += consumed;
                    carried += pcm.len();
                }
            }
        }
        assert_eq!(carried, FRAMES * mp3_fixture::SAMPLES_PER_FRAME);

        // The same bytes, but with the decoder rebuilt for each frame -- which is
        // what constructing a decoder per network chunk amounted to. Every frame
        // after the first is silently lost.
        let mut rebuilt = 0usize;
        for index in 0..FRAMES {
            let frame = &stream[index * mp3_fixture::FRAME_BYTES..][..mp3_fixture::FRAME_BYTES];
            if let Mp3Step::Frame { pcm, .. } = Mp3FrameDecoder::new().decode(frame) {
                rebuilt += pcm.len();
            }
        }
        assert_eq!(
            rebuilt,
            mp3_fixture::SAMPLES_PER_FRAME,
            "a decoder rebuilt per frame must lose every frame that needs the \
             reservoir -- if this ever stops being true the test above proves \
             nothing and the fixture needs a stronger frame"
        );
    }

    /// Leading bytes that are not a frame -- an ID3 tag is the usual one -- must
    /// be got past without being mistaken for audio.
    ///
    /// minimp3 folds them into the frame it eventually finds rather than
    /// reporting them separately, so `consumed` covers both and the caller must
    /// advance by it rather than by the frame length it might have assumed.
    #[test]
    fn leading_non_audio_bytes_are_consumed_with_the_frame_that_follows_them() {
        const PADDING: usize = 64;
        let mut input = vec![0u8; PADDING];
        input.extend_from_slice(&mp3_fixture::stream(3));

        let mut decoder = Mp3FrameDecoder::new();
        let Mp3Step::Frame { pcm, consumed, .. } = decoder.decode(&input) else {
            panic!("the frame after the padding must be found");
        };
        assert_eq!(pcm.len(), mp3_fixture::SAMPLES_PER_FRAME);
        assert_eq!(
            consumed,
            PADDING + mp3_fixture::FRAME_BYTES,
            "the padding must be consumed along with the frame it precedes"
        );
    }

    /// Bytes that might still turn into a frame must survive to be re-offered
    /// with the next chunk; consuming them would desynchronise the stream.
    ///
    /// The three cases are the three ways "no frame yet" arises, and the middle
    /// one is the one that was got wrong: a frame's first bytes are
    /// indistinguishable, from minimp3's answer alone, from bytes worth nothing.
    #[test]
    fn bytes_that_may_still_become_a_frame_are_held_back() {
        // A sync word split across a chunk boundary.
        assert!(matches!(
            Mp3FrameDecoder::new().decode(&[0xFF, 0xFB, 0x90]),
            Mp3Step::NeedMoreData
        ));

        // The front of a real frame that has not fully arrived. Discarding this
        // is silent data loss, and minimp3 reports it exactly as it reports
        // garbage: by claiming every byte it looked at.
        let frame = mp3_fixture::stream(1);
        let partial = &frame[..mp3_fixture::FRAME_BYTES / 3];
        assert!(matches!(
            Mp3FrameDecoder::new().decode(partial),
            Mp3Step::NeedMoreData
        ));

        // Pure noise must still make progress, or a stream opening with a large
        // tag would grow the buffer without limit. What is retained is bounded by
        // the largest frame that could still be incomplete.
        let noise = vec![0x5Au8; 4096];
        let retained = MAX_FRAME_BYTES + HEADER_BYTES - 1;
        assert!(matches!(
            Mp3FrameDecoder::new().decode(&noise),
            Mp3Step::Skipped(skipped) if skipped == noise.len() - retained
        ));
    }
}
