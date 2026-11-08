use minimp3_sys as ffi;
use std::{ffi::c_int, ptr::NonNull};

unsafe extern "C" {
    fn migo_mp3_presentation_new() -> *mut std::ffi::c_void;
    fn migo_mp3_presentation_free(p: *mut std::ffi::c_void);
    fn migo_mp3_presentation_begin(
        p: *mut std::ffi::c_void,
        frame: *const u8,
        size: c_int,
        free_bytes: c_int,
        info: *const ffi::mp3dec_frame_info_t,
        limit: u64,
        expected: *mut u64,
    ) -> c_int;
    fn migo_mp3_presentation_decode(
        p: *mut std::ffi::c_void,
        decoder: *mut ffi::mp3dec_t,
        input: *const u8,
        size: c_int,
        pcm: *mut i16,
        info: *mut ffi::mp3dec_frame_info_t,
        skip: *mut c_int,
    ) -> c_int;
    fn migo_mp3_presentation_finish(p: *const std::ffi::c_void) -> c_int;
    #[cfg(feature = "native-trace")]
    fn migo_mp3_test_full_reader(
        data: *const u8,
        size: usize,
        output: *mut i16,
        capacity: usize,
        actual: *mut usize,
    ) -> c_int;
    #[cfg(feature = "native-trace")]
    fn migo_mp3_test_codec_calls() -> usize;
    #[cfg(feature = "native-trace")]
    fn migo_mp3_test_heap_calls() -> usize;
    #[cfg(feature = "native-trace")]
    fn migo_mp3_test_live_allocations() -> usize;
}

struct Owner(NonNull<std::ffi::c_void>);
impl Owner {
    fn new() -> Self {
        Self(NonNull::new(unsafe { migo_mp3_presentation_new() }).unwrap())
    }
    fn begin(
        &self,
        frame: &[u8],
        info: &ffi::mp3dec_frame_info_t,
        free: c_int,
        limit: u64,
    ) -> (c_int, u64) {
        let mut expected = 0;
        let status = unsafe {
            migo_mp3_presentation_begin(
                self.0.as_ptr(),
                frame.as_ptr(),
                frame.len().try_into().unwrap(),
                free,
                info,
                limit,
                &mut expected,
            )
        };
        (status, expected)
    }
    fn decode(
        &self,
        decoder: &mut ffi::mp3dec_t,
        input: &[u8],
    ) -> (c_int, c_int, ffi::mp3dec_frame_info_t, [i16; 2304]) {
        let mut pcm = [0; 2304];
        let mut info = info();
        let mut skip = 0;
        let count = unsafe {
            migo_mp3_presentation_decode(
                self.0.as_ptr(),
                decoder,
                input.as_ptr(),
                input.len().try_into().unwrap(),
                pcm.as_mut_ptr(),
                &mut info,
                &mut skip,
            )
        };
        (count, skip, info, pcm)
    }
    fn finish(&self) -> c_int {
        unsafe { migo_mp3_presentation_finish(self.0.as_ptr()) }
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        unsafe { migo_mp3_presentation_free(self.0.as_ptr()) };
    }
}
fn decoder() -> ffi::mp3dec_t {
    // The published binding is a C POD; minimp3 requires initially zeroed state.
    unsafe { std::mem::zeroed() }
}
fn info() -> ffi::mp3dec_frame_info_t {
    unsafe { std::mem::zeroed() }
}
#[cfg(feature = "native-trace")]
fn heap() -> (usize, usize) {
    unsafe { (migo_mp3_test_heap_calls(), migo_mp3_test_live_allocations()) }
}
#[cfg(not(feature = "native-trace"))]
fn heap() -> (usize, usize) {
    (0, 0)
}
fn first(input: &[u8]) -> (usize, usize, ffi::mp3dec_frame_info_t, c_int) {
    let mut probe = decoder();
    let mut metadata = info();
    let view = &input[..input.len().min(8192)];
    let count = unsafe {
        ffi::mp3dec_decode_frame(
            &mut probe,
            view.as_ptr(),
            view.len() as c_int,
            std::ptr::null_mut(),
            &mut metadata,
        )
    };
    assert!(count > 0);
    (
        metadata.frame_offset as usize,
        metadata.frame_bytes as usize,
        metadata,
        probe.free_format_bytes,
    )
}
const MUSIC: &[u8] = include_bytes!("../fixtures/music.mp3");
const MUSIC_PCM: &[u8] = include_bytes!("../fixtures/music.i16le");
const CBR: &[u8] = include_bytes!("../fixtures/stereo-cbr.mp3");
const VBR: &[u8] = include_bytes!("../fixtures/mono-vbr.mp3");
const CBR_PCM: &[u8] = include_bytes!("../fixtures/stereo-cbr.i16le");
const VBR_PCM: &[u8] = include_bytes!("../fixtures/mono-vbr.i16le");

fn run(input: &[u8], truncate_after: Option<usize>) -> (Vec<i16>, u64, c_int) {
    run_windows(input, truncate_after, None)
}
fn run_windows(
    input: &[u8],
    truncate_after: Option<usize>,
    lengths: Option<&[usize]>,
) -> (Vec<i16>, u64, c_int) {
    let (offset, end, metadata, free) = first(input);
    let owner = Owner::new();
    let before = heap();
    #[cfg(feature = "native-trace")]
    let codec_before = unsafe { migo_mp3_test_codec_calls() };
    let (skip_tag, expected) = owner.begin(&input[offset..end], &metadata, free, 16_777_216);
    assert!((0..=1).contains(&skip_tag));
    assert_eq!(
        before,
        heap(),
        "one metadata callback must allocate neither index nor IO state"
    );
    #[cfg(feature = "native-trace")]
    assert_eq!(
        codec_before,
        unsafe { migo_mp3_test_codec_calls() },
        "metadata must not decode or scan frames"
    );
    let mut position = if skip_tag == 1 { end } else { offset };
    let mut codec = decoder();
    let mut output = Vec::new();
    let mut steps = 0;
    while input.len() - position >= 4 {
        if truncate_after.is_some_and(|n| steps >= n) {
            break;
        }
        if lengths.is_some_and(|lengths| steps >= lengths.len()) {
            break;
        }
        // Fixture-known boundaries model the existing product cursor's exact
        // views. The first free-format call needs native header lookahead.
        let size = if steps == 0 && free > 0 {
            8192
        } else {
            lengths.map_or(8192, |lengths| lengths[steps])
        };
        let view = &input[position..(position + size).min(input.len())];
        let before = heap();
        #[cfg(feature = "native-trace")]
        let codec_before = unsafe { migo_mp3_test_codec_calls() };
        let (count, skip, decoded, pcm) = owner.decode(&mut codec, view);
        assert_eq!(before, heap(), "one codec/trim step must not allocate");
        #[cfg(feature = "native-trace")]
        assert_eq!(
            unsafe { migo_mp3_test_codec_calls() } - codec_before,
            1,
            "one bounded step must call the native codec once"
        );
        assert!(count >= 0, "decode rejected frame at {position}");
        assert!(skip >= 0 && skip as usize + count as usize <= pcm.len());
        output.extend_from_slice(&pcm[skip as usize..skip as usize + count as usize]);
        steps += 1;
        assert!(steps < 10_000);
        if decoded.frame_bytes <= 0 {
            break;
        }
        position += decoded.frame_bytes as usize;
    }
    (output, expected, owner.finish())
}

#[test]
fn complete_gapless_and_untagged_pcm_matches_frozen_upstream_output() {
    for (input, frozen, frames, channels) in [
        (MUSIC, MUSIC_PCM, 480_137, 2),
        (CBR, CBR_PCM, 6912, 2),
        (VBR, VBR_PCM, 5184, 1),
    ] {
        let (pcm, expected, status) = run(input, None);
        assert_eq!(status, 0);
        assert_eq!(pcm.len(), frames * channels);
        assert!(pcm.iter().any(|&v| v != 0));
        assert_eq!(
            pcm.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>(),
            frozen
        );
        assert_eq!(expected, if input == MUSIC { 960_274 } else { 0 });
    }
}

#[test]
fn metadata_skips_only_tag_and_rejects_caller_and_global_pcm_limits() {
    let (offset, end, metadata, free) = first(MUSIC);
    let frame = &MUSIC[offset..end];
    let owner = Owner::new();
    assert_eq!(owner.begin(frame, &metadata, free, u64::MAX), (1, 960_274));
    assert!(
        owner.begin(frame, &metadata, free, u64::MAX).0 < 0,
        "metadata is consumed once"
    );
    let owner = Owner::new();
    assert!(owner.begin(frame, &metadata, free, 960_273).0 < 0);
    assert!(owner.finish() < 0);
    let mut huge = frame.to_vec();
    let xing = huge
        .windows(4)
        .position(|s| s == b"Info" || s == b"Xing")
        .unwrap();
    huge[xing + 8..xing + 12].copy_from_slice(&u32::MAX.to_be_bytes());
    let owner = Owner::new();
    assert!(
        owner.begin(&huge, &metadata, free, u64::MAX).0 < 0,
        "native quota cannot exceed 64 MiB f32"
    );
}

#[test]
fn known_presentation_cannot_finish_with_partial_pcm() {
    for steps in [0, 1, 10, 300] {
        let (pcm, expected, status) = run(MUSIC, Some(steps));
        assert_eq!(expected, 960_274);
        assert!(pcm.len() < expected as usize);
        assert!(status < 0);
    }
}

#[test]
fn free_format_417_2303_and_padded_2304_with_tag_preserve_all_frames() {
    for size in [417, 2303, 2304] {
        let mut input = Vec::new();
        let mut lengths = Vec::new();
        for index in 0..7 {
            let padded = size == 2304 && index != 0;
            let length = if size == 2304 && !padded { 2303 } else { size };
            lengths.push(length);
            let mut frame = vec![0; length];
            frame[..4].copy_from_slice(&[0xff, 0xfb, if padded { 2 } else { 0 }, 0]);
            if index != 0 {
                frame[4] = 4;
            }
            input.extend(frame);
        }
        if size == 2304 {
            input.extend_from_slice(b"TAG");
            input.extend([0; 125]);
        }
        let (pcm, expected, status) = run_windows(&input, None, Some(&lengths));
        assert_eq!(status, 0);
        assert_eq!(expected, 0);
        assert_eq!(pcm.len(), 8064 * 2);
        assert!(pcm.iter().all(|&sample| sample == 0));
    }
}

#[test]
fn native_input_limits_invalid_order_and_owner_retirement() {
    let live = heap().1;
    for phase in 0..3 {
        let owner = Owner::new();
        #[cfg(feature = "native-trace")]
        assert_eq!(heap().1, live + 1);
        assert!(owner.finish() < 0);
        assert!(owner.decode(&mut decoder(), &MUSIC[..8192]).0 < 0);
        if phase > 0 {
            let (offset, end, metadata, free) = first(MUSIC);
            assert_eq!(
                owner
                    .begin(&MUSIC[offset..end], &metadata, free, u64::MAX)
                    .0,
                1
            );
            assert!(owner.decode(&mut decoder(), &MUSIC[..8193]).0 < 0);
            if phase > 1 {
                let mut codec = decoder();
                for _ in 0..10 {
                    assert!(owner.decode(&mut codec, &MUSIC[end..end + 384]).0 >= 0);
                }
            }
        }
        drop(owner);
        assert_eq!(
            heap().1,
            live,
            "abandonment releases the only native allocation"
        );
    }
}

#[test]
fn malformed_xing_optional_fields_are_bounded_by_the_measured_frame() {
    let metadata = ffi::mp3dec_frame_info_t {
        channels: 1,
        hz: 22050,
        layer: 3,
        frame_bytes: 26,
        frame_offset: 0,
        bitrate_kbps: 8,
    };
    for (flags, extension) in
        (0..16).flat_map(|flags| [0, 1, 255].map(|extension| (flags, extension)))
    {
        let mut frame = vec![0; 26];
        frame[..4].copy_from_slice(&[0xff, 0xf3, 0x10, 0xc0]);
        frame[13..17].copy_from_slice(b"Xing");
        frame[20] = flags;
        frame[25] = extension;
        frame[21..25].copy_from_slice(&5_u32.to_be_bytes());
        let owner = Owner::new();
        let before = heap();
        let (status, expected) = owner.begin(&frame, &metadata, 0, 16_777_216);
        assert_eq!(before, heap());
        assert!(status >= 0);
        assert_eq!(
            expected,
            if flags == 1 && extension == 0 {
                5 * 576
            } else {
                0
            }
        );
    }
    let (offset, end, metadata, _) = first(MUSIC);
    for invalid_free in [i32::MIN, -1, 2305, i32::MAX] {
        let owner = Owner::new();
        assert!(
            owner
                .begin(&MUSIC[offset..end], &metadata, invalid_free, u64::MAX)
                .0
                < 0
        );
    }
    for n in 0..4 {
        let owner = Owner::new();
        assert!(owner.begin(&[0; 4][..n], &metadata, 0, u64::MAX).0 < 0);
    }
}

#[test]
#[cfg(feature = "native-trace")]
fn original_ex_reader_keeps_frozen_pcm_after_shared_helper_extraction() {
    for (input, frozen) in [(MUSIC, MUSIC_PCM), (CBR, CBR_PCM), (VBR, VBR_PCM)] {
        let before = heap().1;
        let mut output = vec![0; frozen.len() / 2 + 2304];
        let mut actual = 0;
        let status = unsafe {
            migo_mp3_test_full_reader(
                input.as_ptr(),
                input.len(),
                output.as_mut_ptr(),
                output.len(),
                &mut actual,
            )
        };
        assert_eq!(status, 0);
        assert_eq!(actual * 2, frozen.len());
        assert_eq!(
            output[..actual]
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect::<Vec<_>>(),
            frozen
        );
        assert_eq!(heap().1, before);
    }
}

#[test]
fn format_change_retires_native_presentation_and_cannot_resume() {
    let (offset, end, metadata, free) = first(CBR);
    let owner = Owner::new();
    assert_eq!(
        owner.begin(&CBR[offset..end], &metadata, free, u64::MAX),
        (0, 0)
    );
    let mut codec = decoder();
    assert!(owner.decode(&mut codec, &CBR[offset..end]).0 > 0);
    let (offset, end, _, _) = first(VBR);
    assert!(owner.decode(&mut codec, &VBR[offset..end]).0 < 0);
    assert!(owner.finish() < 0);
    let (offset, end, _, _) = first(CBR);
    assert!(owner.decode(&mut codec, &CBR[offset..end]).0 < 0);
}

#[test]
fn arbitrary_non_frame_progress_does_not_retire_the_cursor_presentation() {
    let (offset, end, metadata, free) = first(MUSIC);
    let owner = Owner::new();
    assert_eq!(
        owner.begin(&MUSIC[offset..end], &metadata, free, u64::MAX),
        (1, 960274)
    );
    let mut codec = decoder();
    let (count, _, info, _) = owner.decode(&mut codec, &[0; 8192]);
    assert_eq!(count, 0);
    assert!(info.frame_bytes > 0 && info.hz == 0);
    assert!(
        owner.finish() < 0,
        "a skipped view is not a complete result"
    );
    assert!(
        owner.decode(&mut codec, &MUSIC[end..end + 384]).0 >= 0,
        "valid framing can resume after skipped bytes"
    );
}

unsafe extern "C" {
    fn migo_mp3_presentation_begin_stream(
        p: *mut std::ffi::c_void,
        frame: *const u8,
        size: c_int,
        free_bytes: c_int,
        info: *const ffi::mp3dec_frame_info_t,
        expected: *mut u64,
    ) -> c_int;
}

#[test]
fn progressive_metadata_uses_the_shared_kernel_without_a_contiguous_pcm_ceiling() {
    let (offset, end, metadata, free) = first(MUSIC);
    let mut frame = MUSIC[offset..end].to_vec();
    let tag = frame
        .windows(4)
        .position(|s| s == b"Info" || s == b"Xing")
        .unwrap();
    frame[tag + 8..tag + 12].copy_from_slice(&u32::MAX.to_be_bytes());
    let owner = Owner::new();
    let before = heap();
    let mut expected = 0;
    let status = unsafe {
        migo_mp3_presentation_begin_stream(
            owner.0.as_ptr(),
            frame.as_ptr(),
            frame.len() as c_int,
            free,
            &metadata,
            &mut expected,
        )
    };
    assert_eq!(
        before,
        heap(),
        "progressive metadata allocated a whole PCM result"
    );
    assert_eq!(
        status, 1,
        "progressive metadata inherited the contiguous PCM ceiling"
    );
    assert!(expected > 16_777_216);
    let repeated = unsafe {
        migo_mp3_presentation_begin_stream(
            owner.0.as_ptr(),
            frame.as_ptr(),
            frame.len() as c_int,
            free,
            &metadata,
            &mut expected,
        )
    };
    assert!(
        repeated < 0,
        "progressive metadata accepted reinitialization"
    );
}
