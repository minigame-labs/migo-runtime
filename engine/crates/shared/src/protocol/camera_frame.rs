//! Camera preview frame packing.
//!
//! A `YUV_420_888` camera frame arrives as three separate `Image.Plane`
//! `ByteBuffer`s (Y, U, V). The JS-visible contract is a single flat
//! `ArrayBuffer` = each plane's `position..limit` window concatenated in
//! **Y, U, V** order. This module performs the one unavoidable copy that
//! flattens the three non-contiguous plane windows into one owned `Vec<u8>`.
//!
//! It is deliberately safe and free of JNI/V8 dependencies so it is unit
//! testable on the host: the JNI layer resolves each direct buffer's full
//! capacity slice and passes it here together with the `(offset, length)`
//! window (still as signed `jint` values), and this validates the window with
//! checked arithmetic before copying only the validated sub-slice. Plane
//! padding / pixel-stride bytes inside a window are copied verbatim — this does
//! not repack, convert, or reinterpret the layout.

use std::{
    collections::HashMap,
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

/// Largest width or height accepted from a camera callback. This matches the
/// largest Canvas surface dimension and bounds every subsequent pixel-count
/// calculation before host-side copies occur.
pub const MAX_CAMERA_FRAME_DIMENSION: u32 = 8192;

#[derive(Debug)]
struct CameraFrameCreditState {
    /// Token for the command currently notifying the Host, or zero when no
    /// notification is in flight.  Tokens let a replacement command acquire
    /// the slot before an older command's RAII credit is dropped.
    active_token: AtomicU64,
    next_token: AtomicU64,
    superseded: AtomicU64,
    mailbox: Mutex<Option<CameraFrameEntry>>,
}

/// The latest packed frame waiting for the Host's camera callback.
#[derive(Debug, PartialEq, Eq)]
pub struct CameraFrameEntry {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Result of publishing one packed camera frame.
#[derive(Debug)]
pub enum CameraFramePush {
    /// No notification was in flight; the caller must enqueue the returned
    /// credit-bearing command to wake the Host.
    Notify(CameraFrameCredit),
    /// A notification is already queued or being dispatched.  The mailbox now
    /// contains this frame, replacing the older one.
    Superseded,
}

/// Admission acquired before resolving direct buffers and packing plane bytes.
#[derive(Debug)]
pub enum CameraFrameAdmission {
    /// This frame owns the notification token and will enqueue a command.
    Notify(CameraFrameCredit),
    /// A previous notification is still in flight; this frame may replace its
    /// mailbox payload without enqueueing another command.
    Replace(CameraFrameReplacement),
}

#[derive(Debug)]
pub struct CameraFrameReplacement {
    state: Arc<CameraFrameCreditState>,
}

/// RAII admission credit for one camera notification command.
///
/// The credit is carried by the admitted host command. If the command is
/// rejected, canceled, or dropped during shutdown, this guard releases the
/// notification token exactly once. A Host that has taken the mailbox clears
/// the token first, so a replacement can publish another notification before
/// the old command's destructor runs.
#[derive(Debug)]
pub struct CameraFrameCredit {
    state: Arc<CameraFrameCreditState>,
    token: u64,
}

impl Drop for CameraFrameCredit {
    fn drop(&mut self) {
        let _ = self.state.active_token.compare_exchange(
            self.token,
            0,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
}

static CAMERA_FRAME_CREDITS: LazyLock<Mutex<HashMap<(i32, u32), Arc<CameraFrameCreditState>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn camera_frame_credit_state(host_id: i32, camera_id: u32) -> Arc<CameraFrameCreditState> {
    CAMERA_FRAME_CREDITS
        .lock()
        .expect("camera frame credit registry poisoned")
        .entry((host_id, camera_id))
        .or_insert_with(|| {
            Arc::new(CameraFrameCreditState {
                active_token: AtomicU64::new(0),
                next_token: AtomicU64::new(1),
                superseded: AtomicU64::new(0),
                mailbox: Mutex::new(None),
            })
        })
        .clone()
}

/// Acquire camera admission before resolving direct buffers or copying planes.
pub fn prepare_camera_frame(host_id: i32, camera_id: u32) -> CameraFrameAdmission {
    let state = camera_frame_credit_state(host_id, camera_id);
    let token = state.next_token.fetch_add(1, Ordering::Relaxed);
    match state
        .active_token
        .compare_exchange(0, token, Ordering::AcqRel, Ordering::Acquire)
    {
        Ok(_) => CameraFrameAdmission::Notify(CameraFrameCredit { state, token }),
        Err(_) => CameraFrameAdmission::Replace(CameraFrameReplacement { state }),
    }
}

/// Acquire camera admission, then invoke the deferred packing operation.
///
/// Keeping the copy behind this seam makes the pre-copy ordering host-testable:
/// a caller cannot invoke the potentially expensive operation before admission.
pub fn with_camera_frame_admission<T>(
    host_id: i32,
    camera_id: u32,
    copy: impl FnOnce() -> Option<T>,
) -> Option<(CameraFrameAdmission, T)> {
    let admission = prepare_camera_frame(host_id, camera_id);
    copy().map(|value| (admission, value))
}

/// Publish a packed frame after the caller's pre-copy admission.
pub fn publish_camera_frame(
    admission: CameraFrameAdmission,
    entry: CameraFrameEntry,
) -> CameraFramePush {
    let state = match &admission {
        CameraFrameAdmission::Notify(credit) => Arc::clone(&credit.state),
        CameraFrameAdmission::Replace(replacement) => Arc::clone(&replacement.state),
    };
    let replacement_token = {
        let mut mailbox = state.mailbox.lock().expect("camera frame mailbox poisoned");
        if mailbox.replace(entry).is_some() {
            state.superseded.fetch_add(1, Ordering::Relaxed);
        }
        match &admission {
            CameraFrameAdmission::Notify(_) => None,
            CameraFrameAdmission::Replace(_) => {
                let token = state.next_token.fetch_add(1, Ordering::Relaxed);
                state
                    .active_token
                    .compare_exchange(0, token, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                    .then_some(token)
            }
        }
    };
    match (admission, replacement_token) {
        (CameraFrameAdmission::Notify(credit), _) => CameraFramePush::Notify(credit),
        (CameraFrameAdmission::Replace(_), Some(token)) => {
            CameraFramePush::Notify(CameraFrameCredit { state, token })
        }
        (CameraFrameAdmission::Replace(_), None) => CameraFramePush::Superseded,
    }
}

/// Convenience for tests and non-copying producers that already own a packed frame.
pub fn push_camera_frame(host_id: i32, camera_id: u32, entry: CameraFrameEntry) -> CameraFramePush {
    publish_camera_frame(prepare_camera_frame(host_id, camera_id), entry)
}

/// Take the newest frame for a camera and release its notification token.
///
/// Clearing the token while holding the mailbox mutex makes a concurrent
/// producer either replace this entry before the take, or publish a fresh
/// entry and receive a new notification credit after it.
pub fn take_camera_frame(host_id: i32, camera_id: u32) -> Option<CameraFrameEntry> {
    let state = camera_frame_credit_state(host_id, camera_id);
    let mut mailbox = state.mailbox.lock().expect("camera frame mailbox poisoned");
    let entry = mailbox.take();
    state.active_token.store(0, Ordering::Release);
    entry
}

/// Number of camera frames superseded by a newer frame before the Host took it.
pub fn camera_frame_superseded_count(host_id: i32, camera_id: u32) -> u64 {
    camera_frame_credit_state(host_id, camera_id)
        .superseded
        .load(Ordering::Relaxed)
}

/// Largest number of pixels accepted from a camera callback.
pub const MAX_CAMERA_FRAME_PIXELS: u64 =
    (MAX_CAMERA_FRAME_DIMENSION as u64) * (MAX_CAMERA_FRAME_DIMENSION as u64);

/// Largest synchronously materialized Y/U/V payload. A camera callback copies
/// its three direct-buffer windows into one owned host message, so it has the
/// same 64 MiB cap as other synchronous pixel readbacks.
pub const MAX_CAMERA_FRAME_BYTES: usize = 64 * 1024 * 1024;

/// One plane's contribution: a validated `[offset, offset + len)` window of
/// `buffer`, where `buffer` is the plane's full direct-buffer capacity slice
/// and `offset`/`len` are the buffer's `position`/`remaining` (signed, exactly
/// as they cross JNI).
pub struct PlaneWindow<'a> {
    /// The plane's full direct-buffer capacity (index 0..capacity).
    pub buffer: &'a [u8],
    /// Window start (`ByteBuffer.position()`); must be `>= 0`.
    pub offset: i32,
    /// Window length (`ByteBuffer.remaining()`); must be `>= 0`.
    pub len: i32,
}

/// Why a camera frame could not be packed. Returned instead of panicking so a
/// malformed frame from the platform is dropped, never crashing the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraFramePackError {
    /// Frame dimensions were non-positive or exceeded the supported surface
    /// dimensions/pixel count.
    DimensionsOutOfBounds,
    /// A plane's offset was negative.
    NegativeOffset,
    /// A plane's length was negative.
    NegativeLength,
    /// `offset + len` overflowed or exceeded the buffer capacity.
    WindowOutOfBounds,
    /// The summed length of the three windows overflowed `usize`.
    TotalOverflow,
    /// The summed Y/U/V payload is larger than the synchronous frame budget.
    FrameTooLarge,
    /// Allocation of the exact output buffer failed.
    AllocFailed,
}

/// Pack the three plane windows into one exactly-reserved `Vec<u8>` in Y/U/V
/// order. Validates each window (non-negative offset/length, `offset + len`
/// within capacity, no overflow) with checked arithmetic. Source slices are
/// never mutated.
pub fn pack_yuv_planes(planes: [PlaneWindow<'_>; 3]) -> Result<Vec<u8>, CameraFramePackError> {
    let y = validate_window(&planes[0])?;
    let u = validate_window(&planes[1])?;
    let v = validate_window(&planes[2])?;

    let total = validate_camera_frame_byte_lengths([y.len(), u.len(), v.len()])?;

    let mut out = Vec::new();
    out.try_reserve_exact(total)
        .map_err(|_| CameraFramePackError::AllocFailed)?;
    out.extend_from_slice(y);
    out.extend_from_slice(u);
    out.extend_from_slice(v);
    Ok(out)
}

/// Validate JNI camera dimensions before they are converted to unsigned
/// protocol fields or any direct-buffer address is borrowed. The input remains
/// signed because it is received directly from Java/JNI.
pub fn validate_camera_frame_dimensions(
    width: i32,
    height: i32,
) -> Result<(u32, u32), CameraFramePackError> {
    let width = u32::try_from(width).map_err(|_| CameraFramePackError::DimensionsOutOfBounds)?;
    let height = u32::try_from(height).map_err(|_| CameraFramePackError::DimensionsOutOfBounds)?;

    if width == 0
        || height == 0
        || width > MAX_CAMERA_FRAME_DIMENSION
        || height > MAX_CAMERA_FRAME_DIMENSION
    {
        return Err(CameraFramePackError::DimensionsOutOfBounds);
    }

    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or(CameraFramePackError::DimensionsOutOfBounds)?;
    if pixels > MAX_CAMERA_FRAME_PIXELS {
        return Err(CameraFramePackError::DimensionsOutOfBounds);
    }

    Ok((width, height))
}

/// Validate signed JNI plane window lengths before resolving or borrowing
/// their direct buffers. The packing function repeats the total-byte guard
/// after it has validated the individual buffer windows, so neither entrance
/// can reserve/copy an oversized payload.
pub fn validate_camera_frame_payload_lengths(
    lengths: [i32; 3],
) -> Result<usize, CameraFramePackError> {
    let y_len = usize::try_from(lengths[0]).map_err(|_| CameraFramePackError::NegativeLength)?;
    let u_len = usize::try_from(lengths[1]).map_err(|_| CameraFramePackError::NegativeLength)?;
    let v_len = usize::try_from(lengths[2]).map_err(|_| CameraFramePackError::NegativeLength)?;
    validate_camera_frame_byte_lengths([y_len, u_len, v_len])
}

fn validate_camera_frame_byte_lengths(lengths: [usize; 3]) -> Result<usize, CameraFramePackError> {
    let total = checked_total(lengths[0], lengths[1], lengths[2])?;
    if total > MAX_CAMERA_FRAME_BYTES {
        return Err(CameraFramePackError::FrameTooLarge);
    }
    Ok(total)
}

/// Checked sum of the three plane window lengths, mapping overflow directly to
/// [`CameraFramePackError::TotalOverflow`]. Extracted so the exact production
/// error mapping (`pack_yuv_planes` uses `checked_total(...)?`) is unit-testable
/// on raw `usize` values without allocating `usize`-sized slices (impossible in
/// practice — real camera planes are far smaller).
fn checked_total(y_len: usize, u_len: usize, v_len: usize) -> Result<usize, CameraFramePackError> {
    y_len
        .checked_add(u_len)
        .and_then(|s| s.checked_add(v_len))
        .ok_or(CameraFramePackError::TotalOverflow)
}

/// Validate one window against its buffer capacity and return the validated
/// `[offset, offset + len)` sub-slice. Rejects negative offset/length and any
/// window that overflows or reaches past the buffer.
fn validate_window<'a>(plane: &PlaneWindow<'a>) -> Result<&'a [u8], CameraFramePackError> {
    if plane.offset < 0 {
        return Err(CameraFramePackError::NegativeOffset);
    }
    if plane.len < 0 {
        return Err(CameraFramePackError::NegativeLength);
    }
    let offset = plane.offset as usize;
    let len = plane.len as usize;
    // `checked_add` is defense-in-depth: with `i32` inputs on a 64-bit target
    // this cannot overflow `usize`, but it costs nothing and stays correct if
    // the input widths ever change.
    let end = offset
        .checked_add(len)
        .ok_or(CameraFramePackError::WindowOutOfBounds)?;
    if end > plane.buffer.len() {
        return Err(CameraFramePackError::WindowOutOfBounds);
    }
    Ok(&plane.buffer[offset..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(buffer: &[u8], offset: i32, len: i32) -> PlaneWindow<'_> {
        PlaneWindow {
            buffer,
            offset,
            len,
        }
    }

    #[test]
    fn packs_three_windows_in_yuv_order_from_zero_offsets() {
        let y = [1u8, 2, 3];
        let u = [4u8, 5];
        let v = [6u8];
        let out = pack_yuv_planes([win(&y, 0, 3), win(&u, 0, 2), win(&v, 0, 1)]).unwrap();
        assert_eq!(out, [1, 2, 3, 4, 5, 6], "Y then U then V, in order");
    }

    #[test]
    fn packs_non_zero_offset_windows() {
        // Each buffer has bytes outside the window that must be excluded.
        let y = [9u8, 1, 2, 3, 9]; // window [1..4) = [1,2,3]
        let u = [9u8, 9, 4, 5]; // window [2..4) = [4,5]
        let v = [6u8, 9]; // window [0..1) = [6]
        let out = pack_yuv_planes([win(&y, 1, 3), win(&u, 2, 2), win(&v, 0, 1)]).unwrap();
        assert_eq!(out, [1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn exact_end_window_is_accepted() {
        // offset + len == capacity is valid (the last byte is included).
        let y = [1u8, 2, 3, 4];
        let out = pack_yuv_planes([win(&y, 2, 2), win(&[], 0, 0), win(&[], 0, 0)]).unwrap();
        assert_eq!(out, [3, 4]);
    }

    #[test]
    fn zero_length_windows_yield_empty_contribution() {
        // A zero-length window at any (in-bounds) offset contributes nothing.
        let y = [1u8, 2, 3];
        let out = pack_yuv_planes([win(&y, 0, 0), win(&y, 3, 0), win(&y, 1, 0)]).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn rejects_a_frame_larger_than_the_sync_camera_byte_budget_before_copying() {
        let oversized = vec![0u8; 64 * 1024 * 1024 + 1];

        assert!(
            pack_yuv_planes([
                win(&oversized, 0, oversized.len() as i32),
                win(&[], 0, 0),
                win(&[], 0, 0),
            ])
            .is_err(),
            "a camera callback must not duplicate more than 64 MiB into the host queue"
        );
    }

    #[test]
    fn rejects_dimensions_or_plane_lengths_outside_camera_limits() {
        assert_eq!(
            validate_camera_frame_dimensions(8192, 8192),
            Ok((8192, 8192))
        );
        assert_eq!(
            validate_camera_frame_dimensions(8193, 1),
            Err(CameraFramePackError::DimensionsOutOfBounds)
        );
        assert_eq!(
            validate_camera_frame_dimensions(1, -1),
            Err(CameraFramePackError::DimensionsOutOfBounds)
        );
        assert_eq!(
            validate_camera_frame_payload_lengths([(MAX_CAMERA_FRAME_BYTES + 1) as i32, 0, 0]),
            Err(CameraFramePackError::FrameTooLarge)
        );
        assert_eq!(
            validate_camera_frame_payload_lengths([-1, 0, 0]),
            Err(CameraFramePackError::NegativeLength)
        );
    }

    #[test]
    fn rejects_negative_offset() {
        let y = [1u8, 2, 3];
        assert_eq!(
            pack_yuv_planes([win(&y, -1, 1), win(&[], 0, 0), win(&[], 0, 0)]),
            Err(CameraFramePackError::NegativeOffset)
        );
    }

    #[test]
    fn rejects_negative_length() {
        let y = [1u8, 2, 3];
        assert_eq!(
            pack_yuv_planes([win(&y, 0, -1), win(&[], 0, 0), win(&[], 0, 0)]),
            Err(CameraFramePackError::NegativeLength)
        );
    }

    #[test]
    fn rejects_window_beyond_capacity() {
        let y = [1u8, 2, 3];
        // offset + len = 5 > capacity 3
        assert_eq!(
            pack_yuv_planes([win(&y, 0, 5), win(&[], 0, 0), win(&[], 0, 0)]),
            Err(CameraFramePackError::WindowOutOfBounds)
        );
        // offset alone beyond capacity
        assert_eq!(
            pack_yuv_planes([win(&y, 4, 0), win(&[], 0, 0), win(&[], 0, 0)]),
            Err(CameraFramePackError::WindowOutOfBounds)
        );
    }

    #[test]
    fn rejects_i32_max_window_as_out_of_bounds() {
        // With `i32` inputs on a 64-bit target, `offset + len` (max ~2^32)
        // cannot overflow `usize`, so this hits the out-of-capacity check, not
        // the `checked_add` guard. The guard is belt-and-suspenders; the real
        // usize-overflow path is covered by `checked_total_detects_usize_overflow`.
        let y = [1u8, 2, 3];
        assert_eq!(
            pack_yuv_planes([win(&y, i32::MAX, i32::MAX), win(&[], 0, 0), win(&[], 0, 0)]),
            Err(CameraFramePackError::WindowOutOfBounds)
        );
    }

    #[test]
    fn checked_total_maps_usize_overflow_to_total_overflow_error() {
        // Exercises the exact production error mapping: `pack_yuv_planes` calls
        // `checked_total(...)?`, so this asserts the same `TotalOverflow` value
        // the pack path would return. Unreachable with real camera planes.
        assert_eq!(checked_total(1, 2, 3), Ok(6));
        assert_eq!(checked_total(usize::MAX, 0, 0), Ok(usize::MAX));
        assert_eq!(
            checked_total(usize::MAX, 1, 0),
            Err(CameraFramePackError::TotalOverflow),
            "first add overflows"
        );
        assert_eq!(
            checked_total(usize::MAX - 1, 1, 1),
            Err(CameraFramePackError::TotalOverflow),
            "second add overflows"
        );
    }

    #[test]
    fn padding_and_pixel_stride_bytes_are_preserved_verbatim() {
        // Bytes that look like row padding / pixelStride gaps (0xFF, 0x00) are
        // inside the window and must be copied as-is, not stripped or repacked.
        let y = [10u8, 0xFF, 0x00, 11, 0xFF, 12];
        let out = pack_yuv_planes([win(&y, 0, 6), win(&[], 0, 0), win(&[], 0, 0)]).unwrap();
        assert_eq!(out, [10, 0xFF, 0x00, 11, 0xFF, 12]);
    }

    #[test]
    fn source_slices_are_not_mutated() {
        let y = [1u8, 2, 3];
        let u = [4u8, 5];
        let v = [6u8];
        let _ = pack_yuv_planes([win(&y, 0, 3), win(&u, 0, 2), win(&v, 0, 1)]).unwrap();
        assert_eq!(y, [1, 2, 3]);
        assert_eq!(u, [4, 5]);
        assert_eq!(v, [6]);
    }
    #[test]
    fn slow_consumer_sees_latest_frame_and_counts_superseded() {
        let host_id = 9701;
        let camera_id = 13;
        let first = CameraFrameEntry {
            data: vec![1],
            width: 1,
            height: 1,
        };
        let second = CameraFrameEntry {
            data: vec![2],
            width: 2,
            height: 1,
        };
        let credit = match super::push_camera_frame(host_id, camera_id, first) {
            CameraFramePush::Notify(credit) => credit,
            CameraFramePush::Superseded => panic!("first frame must notify"),
        };
        assert!(matches!(
            super::push_camera_frame(host_id, camera_id, second),
            CameraFramePush::Superseded
        ));
        assert_eq!(super::camera_frame_superseded_count(host_id, camera_id), 1);
        assert_eq!(
            super::take_camera_frame(host_id, camera_id),
            Some(CameraFrameEntry {
                data: vec![2],
                width: 2,
                height: 1,
            })
        );
        drop(credit);
    }

    #[test]
    fn mailbox_credit_releases_once_and_allows_next_notification() {
        let host_id = 9702;
        let camera_id = 14;
        let credit = match super::push_camera_frame(
            host_id,
            camera_id,
            CameraFrameEntry {
                data: vec![3],
                width: 1,
                height: 1,
            },
        ) {
            CameraFramePush::Notify(credit) => credit,
            CameraFramePush::Superseded => panic!("first frame must notify"),
        };
        drop(credit);
        let next = super::push_camera_frame(
            host_id,
            camera_id,
            CameraFrameEntry {
                data: vec![4],
                width: 1,
                height: 1,
            },
        );
        assert!(matches!(next, CameraFramePush::Notify(_)));
    }

    #[test]
    fn taking_mailbox_releases_old_credit_before_replacement() {
        let host_id = 9703;
        let camera_id = 15;
        let old_credit = match super::push_camera_frame(
            host_id,
            camera_id,
            CameraFrameEntry {
                data: vec![5],
                width: 1,
                height: 1,
            },
        ) {
            CameraFramePush::Notify(credit) => credit,
            CameraFramePush::Superseded => panic!("first frame must notify"),
        };
        assert!(super::take_camera_frame(host_id, camera_id).is_some());
        let new_result = super::push_camera_frame(
            host_id,
            camera_id,
            CameraFrameEntry {
                data: vec![6],
                width: 1,
                height: 1,
            },
        );
        assert!(matches!(new_result, CameraFramePush::Notify(_)));
        // The old credit must not clear the replacement notification token.
        drop(old_credit);
        assert!(matches!(
            super::push_camera_frame(
                host_id,
                camera_id,
                CameraFrameEntry {
                    data: vec![7],
                    width: 1,
                    height: 1,
                },
            ),
            CameraFramePush::Superseded
        ));
    }
    #[test]
    fn camera_admission_happens_before_deferred_copy() {
        let host_id = 9704;
        let camera_id = 16;
        let nested = with_camera_frame_admission(host_id, camera_id, || {
            Some(match prepare_camera_frame(host_id, camera_id) {
                CameraFrameAdmission::Replace(replacement) => replacement,
                CameraFrameAdmission::Notify(_) => {
                    panic!("deferred copy ran before camera admission")
                }
            })
        })
        .expect("outer camera admission must succeed");
        let (admission, replacement) = nested;
        drop(replacement);
        drop(admission);
    }
}
