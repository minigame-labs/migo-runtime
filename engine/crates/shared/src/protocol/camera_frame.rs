//! Camera preview frames, as content reads them.
//!
//! The JS-visible contract is the reference's: `onCameraFrame` hands content
//! `{width, height, data}`, `data` an `ArrayBuffer` of straight RGBA, four
//! bytes a pixel, rows top first. A `YUV_420_888` frame arrives as three
//! separate `Image.Plane` `ByteBuffer`s (Y, U, V), each with its own row and
//! pixel strides -- a separate U and V, or interleaved ones -- and this module
//! makes the one pass that reads them through those strides and writes the
//! RGBA content is given.
//!
//! It is deliberately safe and free of JNI/V8 dependencies so it is unit
//! testable on the host: the JNI layer resolves each direct buffer's full
//! capacity slice and passes it here together with the `(offset, length)`
//! window and the strides (still as signed `jint` values), and this validates
//! the window, and that it holds every sample the strides reach, with checked
//! arithmetic before reading a byte.

use crate::yuv;
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

/// Largest synchronously materialized frame, read or written: a camera
/// callback reads its three direct-buffer windows and writes one owned RGBA
/// frame into the host message, each within the same 64 MiB cap as other
/// synchronous pixel readbacks.
pub const MAX_CAMERA_FRAME_BYTES: usize = 64 * 1024 * 1024;

/// One plane: a validated `[offset, offset + len)` window of `buffer`, where
/// `buffer` is the plane's full direct-buffer capacity slice and
/// `offset`/`len` are the buffer's `position`/`remaining`, with the distance
/// between its rows and between neighbouring samples (all signed, exactly as
/// they cross JNI).
pub struct PlaneWindow<'a> {
    /// The plane's full direct-buffer capacity (index 0..capacity).
    pub buffer: &'a [u8],
    /// Window start (`ByteBuffer.position()`); must be `>= 0`.
    pub offset: i32,
    /// Window length (`ByteBuffer.remaining()`); must be `>= 0`.
    pub len: i32,
    /// Bytes between the starts of two rows (`Plane.getRowStride()`).
    pub row_stride: i32,
    /// Bytes between two neighbouring samples of a row
    /// (`Plane.getPixelStride()`): 1 for a separate plane, 2 for interleaved
    /// chroma.
    pub pixel_stride: i32,
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
    /// The summed Y/U/V payload, or the RGBA frame, is larger than the
    /// synchronous frame budget.
    FrameTooLarge,
    /// A stride was not positive, or a window does not hold every sample its
    /// strides reach for the frame's dimensions.
    StrideOutOfBounds,
    /// Allocation of the exact output buffer failed.
    AllocFailed,
}

/// A camera's YUV: camera2 produces its `YUV_420_888` with the JPEG File
/// Interchange Format's full-range BT.601 conversion.
const CAMERA_COLOUR: yuv::Colour = yuv::Colour {
    matrix: yuv::Matrix::Bt601,
    full_range: true,
};

/// The `width` x `height` frame the Y, U and V `planes` hold, as one
/// exactly-reserved RGBA buffer. Validates each window (non-negative
/// offset/length, `offset + len` within capacity, no overflow) and that it
/// reaches every sample its strides address -- the luma plane `width` x
/// `height`, each chroma plane half that, rounded up -- with checked arithmetic
/// before reading. Source slices are never mutated.
pub fn yuv_frame_to_rgba(
    planes: [PlaneWindow<'_>; 3],
    width: u32,
    height: u32,
) -> Result<Vec<u8>, CameraFramePackError> {
    if width == 0 || height == 0 {
        return Err(CameraFramePackError::DimensionsOutOfBounds);
    }
    let bytes = camera_frame_rgba_len(width, height)?;
    let (width, height) = (width as usize, height as usize);
    let (chroma_width, chroma_height) = (width.div_ceil(2), height.div_ceil(2));
    let y = strided_plane(&planes[0], width, height)?;
    let cb = strided_plane(&planes[1], chroma_width, chroma_height)?;
    let cr = strided_plane(&planes[2], chroma_width, chroma_height)?;

    let mut rgba = Vec::new();
    rgba.try_reserve_exact(bytes)
        .map_err(|_| CameraFramePackError::AllocFailed)?;
    rgba.resize(bytes, 0);
    yuv::to_rgba(&y, &cb, &cr, width, height, CAMERA_COLOUR, &mut rgba);
    Ok(rgba)
}

/// The layout of a frame of four-byte pixels a host hands over
/// (`MIGO_PIXEL_FORMAT_*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelLayout {
    Rgba,
    Bgra,
}

/// The `width` x `height` frame of four-byte `layout` pixels in `pixels`, its
/// rows `row_stride` bytes apart, as content's RGBA: rows packed, opaque.
/// Validates the size, the stride and that `pixels` holds every row before
/// reading. Source slices are never mutated.
pub fn packed_frame_to_rgba(
    pixels: &[u8],
    width: u32,
    height: u32,
    row_stride: usize,
    layout: PixelLayout,
) -> Result<Vec<u8>, CameraFramePackError> {
    if width == 0
        || height == 0
        || width > MAX_CAMERA_FRAME_DIMENSION
        || height > MAX_CAMERA_FRAME_DIMENSION
    {
        return Err(CameraFramePackError::DimensionsOutOfBounds);
    }
    let bytes = camera_frame_rgba_len(width, height)?;
    let row = width as usize * 4;
    let reach = (height as usize - 1)
        .checked_mul(row_stride)
        .and_then(|start| start.checked_add(row))
        .ok_or(CameraFramePackError::StrideOutOfBounds)?;
    if row_stride < row || reach > pixels.len() {
        return Err(CameraFramePackError::StrideOutOfBounds);
    }
    let mut rgba = Vec::new();
    rgba.try_reserve_exact(bytes)
        .map_err(|_| CameraFramePackError::AllocFailed)?;
    for line in pixels.chunks(row_stride).take(height as usize) {
        for pixel in line[..row].chunks_exact(4) {
            let (red, blue) = match layout {
                PixelLayout::Rgba => (pixel[0], pixel[2]),
                PixelLayout::Bgra => (pixel[2], pixel[0]),
            };
            rgba.extend_from_slice(&[red, pixel[1], blue, 255]);
        }
    }
    Ok(rgba)
}

/// The bytes a `width` x `height` RGBA frame takes, within the frame budget.
fn camera_frame_rgba_len(width: u32, height: u32) -> Result<usize, CameraFramePackError> {
    let bytes = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(CameraFramePackError::DimensionsOutOfBounds)?;
    if bytes > MAX_CAMERA_FRAME_BYTES as u64 {
        return Err(CameraFramePackError::FrameTooLarge);
    }
    Ok(bytes as usize)
}

/// `plane`'s validated window as a plane of `columns` x `rows` samples: one
/// whose strides are positive and whose last sample lies inside it.
fn strided_plane<'a>(
    plane: &PlaneWindow<'a>,
    columns: usize,
    rows: usize,
) -> Result<yuv::Plane<'a>, CameraFramePackError> {
    let data = validate_window(plane)?;
    let row_stride =
        usize::try_from(plane.row_stride).map_err(|_| CameraFramePackError::StrideOutOfBounds)?;
    let pixel_stride =
        usize::try_from(plane.pixel_stride).map_err(|_| CameraFramePackError::StrideOutOfBounds)?;
    if row_stride == 0 || pixel_stride == 0 {
        return Err(CameraFramePackError::StrideOutOfBounds);
    }
    let last = (rows - 1)
        .checked_mul(row_stride)
        .and_then(|start| (columns - 1).checked_mul(pixel_stride)?.checked_add(start))
        .ok_or(CameraFramePackError::StrideOutOfBounds)?;
    if last >= data.len() {
        return Err(CameraFramePackError::StrideOutOfBounds);
    }
    Ok(yuv::Plane {
        data,
        row_stride,
        pixel_stride,
    })
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
    // The RGBA frame it becomes, within the budget too.
    camera_frame_rgba_len(width, height)?;

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
/// error mapping is unit-testable on raw `usize` values without allocating
/// `usize`-sized slices (impossible in practice — real camera planes are far
/// smaller).
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

    fn strided(
        buffer: &[u8],
        offset: i32,
        len: i32,
        row_stride: i32,
        pixel_stride: i32,
    ) -> PlaneWindow<'_> {
        PlaneWindow {
            buffer,
            offset,
            len,
            row_stride,
            pixel_stride,
        }
    }

    /// A 4 x 2 frame: the left half red, the right half grey, in full-range
    /// BT.601 (red is Y 76, Cb 85, Cr 255).
    const LUMA: [u8; 8] = [76, 76, 128, 128, 76, 76, 128, 128];
    const CB: [u8; 2] = [85, 128];
    const CR: [u8; 2] = [255, 128];

    fn near(actual: &[u8], expected: [u8; 4]) -> bool {
        actual
            .iter()
            .zip(expected)
            .all(|(&value, want)| value.abs_diff(want) <= 2)
    }

    #[test]
    fn a_separate_u_and_v_become_rgba() {
        let rgba = yuv_frame_to_rgba(
            [
                strided(&LUMA, 0, 8, 4, 1),
                strided(&CB, 0, 2, 2, 1),
                strided(&CR, 0, 2, 2, 1),
            ],
            4,
            2,
        )
        .unwrap();
        assert_eq!(rgba.len(), 4 * 2 * 4, "four bytes a pixel");
        for row in 0..2 {
            let line = &rgba[row * 16..(row + 1) * 16];
            assert!(near(&line[0..4], [254, 0, 0, 255]), "{line:?}");
            assert!(near(&line[4..8], [254, 0, 0, 255]), "{line:?}");
            assert!(near(&line[8..12], [128, 128, 128, 255]), "{line:?}");
            assert!(near(&line[12..16], [128, 128, 128, 255]), "{line:?}");
        }
    }

    #[test]
    fn interleaved_chroma_and_padded_rows_are_read_through_their_strides() {
        // The same frame as a camera's NV21-backed planes: each luma row padded
        // to 6 bytes, the window starting a byte in; V and U interleaved in one
        // buffer, U one byte after V, each a pixel stride of 2.
        let luma = [0xEE, 76, 76, 128, 128, 0xEE, 0xEE, 76, 76, 128, 128, 0xEE];
        let chroma = [255u8, 85, 128, 128];
        let rgba = yuv_frame_to_rgba(
            [
                strided(&luma, 1, 10, 6, 1),
                strided(&chroma, 1, 3, 4, 2),
                strided(&chroma, 0, 3, 4, 2),
            ],
            4,
            2,
        )
        .unwrap();
        let separate = yuv_frame_to_rgba(
            [
                strided(&LUMA, 0, 8, 4, 1),
                strided(&CB, 0, 2, 2, 1),
                strided(&CR, 0, 2, 2, 1),
            ],
            4,
            2,
        )
        .unwrap();
        assert_eq!(rgba, separate, "the padding and the gaps are never read");
    }

    #[test]
    fn an_odd_frame_reads_the_chroma_its_last_column_and_row_need() {
        // 3 x 3: the chroma planes are 2 x 2, rounded up.
        let luma = [128u8; 9];
        let chroma = [128u8; 4];
        let rgba = yuv_frame_to_rgba(
            [
                strided(&luma, 0, 9, 3, 1),
                strided(&chroma, 0, 4, 2, 1),
                strided(&chroma, 0, 4, 2, 1),
            ],
            3,
            3,
        )
        .unwrap();
        assert!(
            rgba.chunks_exact(4)
                .all(|pixel| near(pixel, [128, 128, 128, 255]))
        );
        // One chroma sample short: refused, not read past.
        assert_eq!(
            yuv_frame_to_rgba(
                [
                    strided(&luma, 0, 9, 3, 1),
                    strided(&chroma, 0, 3, 2, 1),
                    strided(&chroma, 0, 4, 2, 1),
                ],
                3,
                3,
            ),
            Err(CameraFramePackError::StrideOutOfBounds)
        );
    }

    #[test]
    fn strides_that_reach_past_the_window_or_are_not_positive_are_refused() {
        let frame = |y_row: i32, uv_pixel: i32| {
            yuv_frame_to_rgba(
                [
                    strided(&LUMA, 0, 8, y_row, 1),
                    strided(&CB, 0, 2, 2, uv_pixel),
                    strided(&CR, 0, 2, 2, 1),
                ],
                4,
                2,
            )
        };
        assert!(frame(4, 1).is_ok());
        assert_eq!(frame(5, 1), Err(CameraFramePackError::StrideOutOfBounds));
        assert_eq!(frame(4, 2), Err(CameraFramePackError::StrideOutOfBounds));
        assert_eq!(frame(0, 1), Err(CameraFramePackError::StrideOutOfBounds));
        assert_eq!(frame(4, -1), Err(CameraFramePackError::StrideOutOfBounds));
        assert_eq!(
            yuv_frame_to_rgba(
                [
                    strided(&LUMA, 0, 8, 4, 1),
                    strided(&CB, 0, 2, 2, 1),
                    strided(&CR, 0, 2, 2, 1)
                ],
                0,
                2
            ),
            Err(CameraFramePackError::DimensionsOutOfBounds)
        );
    }

    #[test]
    fn rejects_a_frame_larger_than_the_sync_camera_byte_budget_before_reading() {
        // 4096 x 4097 RGBA is a row over 64 MiB.
        assert_eq!(
            yuv_frame_to_rgba(
                [
                    strided(&LUMA, 0, 8, 4, 1),
                    strided(&CB, 0, 2, 2, 1),
                    strided(&CR, 0, 2, 2, 1)
                ],
                4096,
                4097
            ),
            Err(CameraFramePackError::FrameTooLarge),
            "a camera callback must not write more than 64 MiB into the host queue"
        );
    }

    #[test]
    fn rejects_dimensions_or_plane_lengths_outside_camera_limits() {
        assert_eq!(
            validate_camera_frame_dimensions(4096, 4096),
            Ok((4096, 4096))
        );
        assert_eq!(validate_camera_frame_dimensions(8192, 1), Ok((8192, 1)));
        assert_eq!(
            validate_camera_frame_dimensions(4096, 4097),
            Err(CameraFramePackError::FrameTooLarge),
            "its RGBA would be over the budget"
        );
        assert_eq!(
            validate_camera_frame_dimensions(0, 1),
            Err(CameraFramePackError::DimensionsOutOfBounds)
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
        assert_eq!(
            yuv_frame_to_rgba(
                [
                    strided(&LUMA, -1, 8, 4, 1),
                    strided(&CB, 0, 2, 2, 1),
                    strided(&CR, 0, 2, 2, 1)
                ],
                4,
                2
            ),
            Err(CameraFramePackError::NegativeOffset)
        );
    }

    #[test]
    fn rejects_negative_length() {
        assert_eq!(
            yuv_frame_to_rgba(
                [
                    strided(&LUMA, 0, -1, 4, 1),
                    strided(&CB, 0, 2, 2, 1),
                    strided(&CR, 0, 2, 2, 1)
                ],
                4,
                2
            ),
            Err(CameraFramePackError::NegativeLength)
        );
    }

    #[test]
    fn rejects_window_beyond_capacity() {
        // offset + len = 9 > capacity 8
        assert_eq!(
            yuv_frame_to_rgba(
                [
                    strided(&LUMA, 1, 8, 4, 1),
                    strided(&CB, 0, 2, 2, 1),
                    strided(&CR, 0, 2, 2, 1)
                ],
                4,
                2
            ),
            Err(CameraFramePackError::WindowOutOfBounds)
        );
        // offset alone beyond capacity
        assert_eq!(
            yuv_frame_to_rgba(
                [
                    strided(&LUMA, 9, 0, 4, 1),
                    strided(&CB, 0, 2, 2, 1),
                    strided(&CR, 0, 2, 2, 1)
                ],
                4,
                2
            ),
            Err(CameraFramePackError::WindowOutOfBounds)
        );
    }

    #[test]
    fn rejects_i32_max_window_as_out_of_bounds() {
        // With `i32` inputs on a 64-bit target, `offset + len` (max ~2^32)
        // cannot overflow `usize`, so this hits the out-of-capacity check, not
        // the `checked_add` guard.
        assert_eq!(
            yuv_frame_to_rgba(
                [
                    strided(&LUMA, i32::MAX, i32::MAX, 4, 1),
                    strided(&CB, 0, 2, 2, 1),
                    strided(&CR, 0, 2, 2, 1)
                ],
                4,
                2
            ),
            Err(CameraFramePackError::WindowOutOfBounds)
        );
    }

    #[test]
    fn checked_total_maps_usize_overflow_to_total_overflow_error() {
        // Unreachable with real camera planes.
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
    fn a_hosts_rgba_or_bgra_rows_become_packed_opaque_rgba() {
        // 2 x 2, rows 12 bytes apart (a 4-byte pad each), alpha left at 0.
        let rgba_rows = [
            1u8, 2, 3, 0, 4, 5, 6, 0, 0xEE, 0xEE, 0xEE, 0xEE, //
            7, 8, 9, 0, 10, 11, 12, 0,
        ];
        let packed = packed_frame_to_rgba(&rgba_rows, 2, 2, 12, PixelLayout::Rgba).unwrap();
        assert_eq!(
            packed,
            [1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255]
        );
        let bgra = packed_frame_to_rgba(&rgba_rows, 2, 2, 12, PixelLayout::Bgra).unwrap();
        assert_eq!(&bgra[..8], &[3, 2, 1, 255, 6, 5, 4, 255]);
    }

    #[test]
    fn a_hosts_frame_short_of_its_rows_or_stride_is_refused() {
        let rows = [0u8; 20];
        assert_eq!(
            packed_frame_to_rgba(&rows, 2, 2, 7, PixelLayout::Rgba),
            Err(CameraFramePackError::StrideOutOfBounds),
            "a stride shorter than a row"
        );
        assert_eq!(
            packed_frame_to_rgba(&rows[..19], 2, 2, 12, PixelLayout::Rgba),
            Err(CameraFramePackError::StrideOutOfBounds),
            "the last row cut short"
        );
        assert!(packed_frame_to_rgba(&rows, 2, 2, 12, PixelLayout::Rgba).is_ok());
        assert_eq!(
            packed_frame_to_rgba(&rows, 0, 2, 12, PixelLayout::Rgba),
            Err(CameraFramePackError::DimensionsOutOfBounds)
        );
        assert_eq!(
            packed_frame_to_rgba(&rows, 4096, 4097, 16384, PixelLayout::Rgba),
            Err(CameraFramePackError::FrameTooLarge)
        );
    }

    #[test]
    fn source_slices_are_not_mutated() {
        let (luma, cb, cr) = (LUMA, CB, CR);
        let _ = yuv_frame_to_rgba(
            [
                strided(&luma, 0, 8, 4, 1),
                strided(&cb, 0, 2, 2, 1),
                strided(&cr, 0, 2, 2, 1),
            ],
            4,
            2,
        )
        .unwrap();
        assert_eq!((luma, cb, cr), (LUMA, CB, CR));
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
