//! `createVideoDecoder` on Android: the NDK's media extractor and codec.
//!
//! The codec renders into an image reader of YUV_420_888, the one output every
//! decoder supports whatever its own buffer layout -- planar, semi-planar or a
//! vendor's tiling -- and each image's planes become straight RGBA through
//! their strides, in the matrix and range the codec reports. Rendered images
//! are matched to their buffers by timestamp, so one rendered before a seek's
//! flush is never taken for one after it.

use std::{
    ffi::{CStr, CString, c_char, c_int, c_void},
    fs::File,
    os::fd::AsRawFd,
    ptr,
    sync::{Condvar, Mutex},
    time::{Duration, Instant},
};

use shared::{
    protocol::error::ServiceError,
    services::{FrameTiming, VideoDecoderService, VideoFrameSource},
};

use crate::{
    android::surface::ANativeWindow,
    yuv::{self, Colour, Matrix, Plane},
};

#[repr(C)]
struct AMediaExtractor(c_void);
#[repr(C)]
struct AMediaFormat(c_void);
#[repr(C)]
struct AMediaCodec(c_void);
#[repr(C)]
struct AImageReader(c_void);
#[repr(C)]
struct AImage(c_void);

#[repr(C)]
#[derive(Default)]
struct AMediaCodecBufferInfo {
    offset: i32,
    size: i32,
    presentation_time_us: i64,
    flags: u32,
}

#[repr(C)]
struct AImageReaderImageListener {
    context: *mut c_void,
    on_image_available: Option<unsafe extern "C" fn(*mut c_void, *mut AImageReader)>,
}

#[repr(C)]
#[derive(Default)]
struct AImageCropRect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

/// `off_t`, which is 32 bits on the 32-bit ABIs.
#[cfg(target_pointer_width = "64")]
type OffT = i64;
#[cfg(not(target_pointer_width = "64"))]
type OffT = i32;

const AMEDIA_OK: i32 = 0;
const AMEDIA_IMGREADER_NO_BUFFER_AVAILABLE: i32 = -30000;
const AMEDIAEXTRACTOR_SEEK_PREVIOUS_SYNC: i32 = 0;
const AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM: u32 = 4;
const AMEDIACODEC_INFO_TRY_AGAIN_LATER: isize = -1;
const AMEDIACODEC_INFO_OUTPUT_FORMAT_CHANGED: isize = -2;
const AMEDIACODEC_INFO_OUTPUT_BUFFERS_CHANGED: isize = -3;
const AIMAGE_FORMAT_YUV_420_888: i32 = 0x23;

#[link(name = "mediandk")]
unsafe extern "C" {
    fn AMediaExtractor_new() -> *mut AMediaExtractor;
    fn AMediaExtractor_delete(extractor: *mut AMediaExtractor) -> i32;
    fn AMediaExtractor_setDataSourceFd(
        extractor: *mut AMediaExtractor,
        fd: c_int,
        offset: i64,
        length: i64,
    ) -> i32;
    fn AMediaExtractor_setDataSource(
        extractor: *mut AMediaExtractor,
        location: *const c_char,
    ) -> i32;
    fn AMediaExtractor_getTrackCount(extractor: *mut AMediaExtractor) -> usize;
    fn AMediaExtractor_getTrackFormat(
        extractor: *mut AMediaExtractor,
        index: usize,
    ) -> *mut AMediaFormat;
    fn AMediaExtractor_selectTrack(extractor: *mut AMediaExtractor, index: usize) -> i32;
    fn AMediaExtractor_readSampleData(
        extractor: *mut AMediaExtractor,
        buffer: *mut u8,
        capacity: usize,
    ) -> isize;
    fn AMediaExtractor_getSampleTime(extractor: *mut AMediaExtractor) -> i64;
    fn AMediaExtractor_advance(extractor: *mut AMediaExtractor) -> bool;
    fn AMediaExtractor_seekTo(extractor: *mut AMediaExtractor, position_us: i64, mode: i32) -> i32;

    fn AMediaFormat_delete(format: *mut AMediaFormat) -> i32;
    fn AMediaFormat_getString(
        format: *mut AMediaFormat,
        name: *const c_char,
        out: *mut *const c_char,
    ) -> bool;
    fn AMediaFormat_getInt32(format: *mut AMediaFormat, name: *const c_char, out: *mut i32)
    -> bool;

    fn AMediaCodec_createDecoderByType(mime: *const c_char) -> *mut AMediaCodec;
    fn AMediaCodec_configure(
        codec: *mut AMediaCodec,
        format: *const AMediaFormat,
        surface: *mut ANativeWindow,
        crypto: *mut c_void,
        flags: u32,
    ) -> i32;
    fn AMediaCodec_start(codec: *mut AMediaCodec) -> i32;
    fn AMediaCodec_stop(codec: *mut AMediaCodec) -> i32;
    fn AMediaCodec_delete(codec: *mut AMediaCodec) -> i32;
    fn AMediaCodec_flush(codec: *mut AMediaCodec) -> i32;
    fn AMediaCodec_dequeueInputBuffer(codec: *mut AMediaCodec, timeout_us: i64) -> isize;
    fn AMediaCodec_getInputBuffer(
        codec: *mut AMediaCodec,
        index: usize,
        size: *mut usize,
    ) -> *mut u8;
    fn AMediaCodec_queueInputBuffer(
        codec: *mut AMediaCodec,
        index: usize,
        offset: OffT,
        size: usize,
        time_us: u64,
        flags: u32,
    ) -> i32;
    fn AMediaCodec_dequeueOutputBuffer(
        codec: *mut AMediaCodec,
        info: *mut AMediaCodecBufferInfo,
        timeout_us: i64,
    ) -> isize;
    fn AMediaCodec_getOutputFormat(codec: *mut AMediaCodec) -> *mut AMediaFormat;
    fn AMediaCodec_releaseOutputBuffer(codec: *mut AMediaCodec, index: usize, render: bool) -> i32;

    fn AImageReader_new(
        width: i32,
        height: i32,
        format: i32,
        max_images: i32,
        reader: *mut *mut AImageReader,
    ) -> i32;
    fn AImageReader_delete(reader: *mut AImageReader);
    fn AImageReader_getWindow(reader: *mut AImageReader, window: *mut *mut ANativeWindow) -> i32;
    fn AImageReader_acquireNextImage(reader: *mut AImageReader, image: *mut *mut AImage) -> i32;
    fn AImageReader_setImageListener(
        reader: *mut AImageReader,
        listener: *mut AImageReaderImageListener,
    ) -> i32;

    fn AImage_delete(image: *mut AImage);
    fn AImage_getTimestamp(image: *const AImage, timestamp_ns: *mut i64) -> i32;
    fn AImage_getPlaneData(
        image: *const AImage,
        plane: i32,
        data: *mut *mut u8,
        length: *mut i32,
    ) -> i32;
    fn AImage_getPlaneRowStride(image: *const AImage, plane: i32, stride: *mut i32) -> i32;
    fn AImage_getPlanePixelStride(image: *const AImage, plane: i32, stride: *mut i32) -> i32;
    fn AImage_getCropRect(image: *const AImage, rect: *mut AImageCropRect) -> i32;
}

/// The engine's video decoding on Android.
pub struct AndroidVideoDecoder;

impl VideoDecoderService for AndroidVideoDecoder {
    fn open(&self, source: &str) -> Result<Box<dyn VideoFrameSource>, ServiceError> {
        Ok(Box::new(Source::open(source)?))
    }
}

/// How long the decoder may go without a frame before it is taken for stuck.
const STALL: Duration = Duration::from_secs(5);
/// How long a rendered buffer may take to reach the image reader.
const IMAGE_WAIT: Duration = Duration::from_secs(2);
/// The images the reader holds at once: the frame held across a seek, the one
/// after it, and the one arriving.
const READER_IMAGES: i32 = 4;

fn failure(what: &str, status: impl std::fmt::Display) -> ServiceError {
    ServiceError::system(format!("{what} failed ({status})"))
}

fn key(name: &CStr) -> *const c_char {
    name.as_ptr()
}

/// An image the reader lent, given back when dropped.
struct Image(*mut AImage);

impl Drop for Image {
    fn drop(&mut self) {
        // SAFETY: acquired from the reader and deleted exactly once.
        unsafe { AImage_delete(self.0) }
    }
}

/// Woken by the reader's thread when an image arrives.
#[derive(Default)]
struct Arrivals {
    count: Mutex<u64>,
    arrived: Condvar,
}

unsafe extern "C" fn image_available(context: *mut c_void, _reader: *mut AImageReader) {
    // SAFETY: the context is the source's boxed `Arrivals`, which outlives the
    // reader that calls this.
    let arrivals = unsafe { &*(context as *const Arrivals) };
    *arrivals
        .count
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) += 1;
    arrivals.arrived.notify_all();
}

struct Source {
    extractor: *mut AMediaExtractor,
    codec: *mut AMediaCodec,
    reader: *mut AImageReader,
    /// The listener and what it wakes, boxed so the reader's pointers to them
    /// stay put; dropped after the reader.
    _listener: Box<AImageReaderImageListener>,
    arrivals: Box<Arrivals>,
    /// The file the extractor reads, open for as long as it does.
    _file: Option<File>,
    width: u32,
    height: u32,
    colour: Colour,
    input_done: bool,
    output_done: bool,
    /// After a seek: frames up to here are only candidates for the one shown
    /// there.
    seek_target: Option<f64>,
    /// The latest such candidate.
    held: Option<(Image, f64)>,
    /// The frame decoded past the target, kept for the call after the held
    /// one is shown.
    pending: Option<(Image, f64)>,
}

// SAFETY: the NDK objects are used from the one decode thread that owns the
// source; the reader's listener touches only `Arrivals`, which is Sync.
unsafe impl Send for Source {}

impl Source {
    fn open(source: &str) -> Result<Self, ServiceError> {
        // SAFETY: every NDK call below is given objects this function created
        // and still owns; each is released on every path, by `Drop` once the
        // source exists and by `Partial` before.
        unsafe {
            let mut partial = Partial::default();
            partial.extractor = AMediaExtractor_new();
            if partial.extractor.is_null() {
                return Err(ServiceError::system(
                    "the media extractor could not be created",
                ));
            }
            let remote = source.starts_with("http://") || source.starts_with("https://");
            if remote {
                let url = CString::new(source)
                    .map_err(|_| ServiceError::invalid_param("the source is not a URL"))?;
                let status = AMediaExtractor_setDataSource(partial.extractor, url.as_ptr());
                if status != AMEDIA_OK {
                    return Err(failure("opening the video", status));
                }
            } else {
                // A path the media server may not reach is opened here and
                // lent as a descriptor.
                let file = File::open(source)
                    .map_err(|error| ServiceError::system(format!("{source}: {error}")))?;
                let length = file.metadata().map(|meta| meta.len() as i64).unwrap_or(0);
                let status =
                    AMediaExtractor_setDataSourceFd(partial.extractor, file.as_raw_fd(), 0, length);
                if status != AMEDIA_OK {
                    return Err(failure("opening the video", status));
                }
                partial.file = Some(file);
            }

            let (track, format) = Self::video_track(partial.extractor)?;
            partial.format = format;
            let status = AMediaExtractor_selectTrack(partial.extractor, track);
            if status != AMEDIA_OK {
                return Err(failure("selecting the video track", status));
            }
            let mut mime: *const c_char = ptr::null();
            AMediaFormat_getString(format, key(c"mime"), &mut mime);
            let (mut width, mut height) = (0i32, 0i32);
            if !AMediaFormat_getInt32(format, key(c"width"), &mut width)
                || !AMediaFormat_getInt32(format, key(c"height"), &mut height)
                || width <= 0
                || height <= 0
            {
                return Err(ServiceError::system("the video track has no size"));
            }

            let status = AImageReader_new(
                width,
                height,
                AIMAGE_FORMAT_YUV_420_888,
                READER_IMAGES,
                &mut partial.reader,
            );
            if status != AMEDIA_OK || partial.reader.is_null() {
                return Err(failure("creating the image reader", status));
            }
            let arrivals = Box::new(Arrivals::default());
            let mut listener = Box::new(AImageReaderImageListener {
                context: &*arrivals as *const Arrivals as *mut c_void,
                on_image_available: Some(image_available),
            });
            let status = AImageReader_setImageListener(partial.reader, &mut *listener);
            if status != AMEDIA_OK {
                return Err(failure("listening to the image reader", status));
            }
            let mut window: *mut ANativeWindow = ptr::null_mut();
            let status = AImageReader_getWindow(partial.reader, &mut window);
            if status != AMEDIA_OK || window.is_null() {
                return Err(failure("reaching the image reader's window", status));
            }

            partial.codec = AMediaCodec_createDecoderByType(mime);
            if partial.codec.is_null() {
                let name = if mime.is_null() {
                    "?".into()
                } else {
                    CStr::from_ptr(mime).to_string_lossy()
                };
                return Err(ServiceError::not_supported(format!(
                    "no decoder for {name}"
                )));
            }
            let status = AMediaCodec_configure(partial.codec, format, window, ptr::null_mut(), 0);
            if status != AMEDIA_OK {
                return Err(failure("configuring the decoder", status));
            }
            let status = AMediaCodec_start(partial.codec);
            if status != AMEDIA_OK {
                return Err(failure("starting the decoder", status));
            }
            let colour = Self::colour(format, height as u32);
            let (extractor, codec, reader, file) = partial.take();
            Ok(Self {
                extractor,
                codec,
                reader,
                _listener: listener,
                arrivals,
                _file: file,
                width: width as u32,
                height: height as u32,
                colour,
                input_done: false,
                output_done: false,
                seek_target: None,
                held: None,
                pending: None,
            })
        }
    }

    /// The first video track and its format, which the caller deletes.
    unsafe fn video_track(
        extractor: *mut AMediaExtractor,
    ) -> Result<(usize, *mut AMediaFormat), ServiceError> {
        // SAFETY: the extractor is open; each format not returned is deleted.
        unsafe {
            for index in 0..AMediaExtractor_getTrackCount(extractor) {
                let format = AMediaExtractor_getTrackFormat(extractor, index);
                if format.is_null() {
                    continue;
                }
                let mut mime: *const c_char = ptr::null();
                if AMediaFormat_getString(format, key(c"mime"), &mut mime)
                    && !mime.is_null()
                    && CStr::from_ptr(mime).to_bytes().starts_with(b"video/")
                {
                    return Ok((index, format));
                }
                AMediaFormat_delete(format);
            }
        }
        Err(ServiceError::not_supported("the source has no video track"))
    }

    /// The matrix and range a format reports -- `color-standard` and
    /// `color-range`, which a decoder fills in -- or the common reading.
    unsafe fn colour(format: *mut AMediaFormat, height: u32) -> Colour {
        let mut colour = Colour::assumed(height);
        let (mut standard, mut range) = (0i32, 0i32);
        // SAFETY: the format is live for the call.
        unsafe {
            if AMediaFormat_getInt32(format, key(c"color-standard"), &mut standard) {
                colour.matrix = match standard {
                    1 => Matrix::Bt709,
                    2 | 4 => Matrix::Bt601,
                    6 => Matrix::Bt2020,
                    _ => colour.matrix,
                };
            }
            if AMediaFormat_getInt32(format, key(c"color-range"), &mut range) {
                colour.full_range = range == 1;
            }
        }
        colour
    }

    /// Hand the codec every sample it has room for.
    fn feed(&mut self) -> Result<(), ServiceError> {
        // SAFETY: the codec and extractor are live, and the input buffer is the
        // codec's for the index it gave until it is queued.
        unsafe {
            while !self.input_done {
                let index = AMediaCodec_dequeueInputBuffer(self.codec, 0);
                if index < 0 {
                    return Ok(());
                }
                let mut capacity = 0usize;
                let buffer = AMediaCodec_getInputBuffer(self.codec, index as usize, &mut capacity);
                let size = if buffer.is_null() {
                    -1
                } else {
                    AMediaExtractor_readSampleData(self.extractor, buffer, capacity)
                };
                let status = if size < 0 {
                    self.input_done = true;
                    AMediaCodec_queueInputBuffer(
                        self.codec,
                        index as usize,
                        0,
                        0,
                        0,
                        AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM,
                    )
                } else {
                    let time = AMediaExtractor_getSampleTime(self.extractor).max(0) as u64;
                    let status = AMediaCodec_queueInputBuffer(
                        self.codec,
                        index as usize,
                        0,
                        size as usize,
                        time,
                        0,
                    );
                    AMediaExtractor_advance(self.extractor);
                    status
                };
                if status != AMEDIA_OK {
                    return Err(failure("queueing a sample", status));
                }
            }
        }
        Ok(())
    }

    /// The next frame the codec renders, with its presentation time in
    /// milliseconds; `None` at the end of the stream.
    fn decode(&mut self) -> Result<Option<(Image, f64)>, ServiceError> {
        let started = Instant::now();
        loop {
            if self.output_done {
                return Ok(None);
            }
            self.feed()?;
            let mut info = AMediaCodecBufferInfo::default();
            // SAFETY: the codec is live; an output index is released exactly once.
            let index = unsafe { AMediaCodec_dequeueOutputBuffer(self.codec, &mut info, 10_000) };
            if index >= 0 {
                let end = info.flags & AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM != 0;
                let render = info.size > 0 || !end;
                // SAFETY: as above.
                let status =
                    unsafe { AMediaCodec_releaseOutputBuffer(self.codec, index as usize, render) };
                if status != AMEDIA_OK {
                    return Err(failure("releasing a frame", status));
                }
                if end {
                    self.output_done = true;
                }
                if render {
                    let image = self.image(info.presentation_time_us)?;
                    return Ok(Some((image, info.presentation_time_us as f64 / 1000.0)));
                }
            } else if index == AMEDIACODEC_INFO_OUTPUT_FORMAT_CHANGED {
                // SAFETY: the codec is live; the format it returns is deleted here.
                unsafe {
                    let format = AMediaCodec_getOutputFormat(self.codec);
                    if !format.is_null() {
                        self.colour = Self::colour(format, self.height);
                        AMediaFormat_delete(format);
                    }
                }
            } else if index != AMEDIACODEC_INFO_TRY_AGAIN_LATER
                && index != AMEDIACODEC_INFO_OUTPUT_BUFFERS_CHANGED
            {
                return Err(failure("decoding", index));
            } else if started.elapsed() > STALL {
                return Err(ServiceError::system("the decoder stopped producing frames"));
            }
        }
    }

    /// The rendered image of the buffer presented at `pts_us`; any older one
    /// still in the reader, rendered before a seek, is dropped.
    fn image(&mut self, pts_us: i64) -> Result<Image, ServiceError> {
        let deadline = Instant::now() + IMAGE_WAIT;
        loop {
            let mut raw: *mut AImage = ptr::null_mut();
            let seen = *self
                .arrivals
                .count
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            // SAFETY: the reader is live; an acquired image is owned by `Image`.
            let status = unsafe { AImageReader_acquireNextImage(self.reader, &mut raw) };
            if status == AMEDIA_OK && !raw.is_null() {
                let image = Image(raw);
                let mut timestamp = 0i64;
                // SAFETY: the image is live.
                unsafe { AImage_getTimestamp(image.0, &mut timestamp) };
                if timestamp >= pts_us.saturating_mul(1000) {
                    return Ok(image);
                }
                continue;
            }
            if status != AMEDIA_IMGREADER_NO_BUFFER_AVAILABLE {
                return Err(failure("taking a frame from the image reader", status));
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(ServiceError::system(
                    "a decoded frame never reached the image reader",
                ));
            }
            let count = self
                .arrivals
                .count
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if *count == seen {
                let _ = self.arrivals.arrived.wait_timeout(count, deadline - now);
            }
        }
    }

    /// Write `image` into `rgba` as straight RGBA8 of the source's size.
    fn convert(&self, image: &Image, rgba: &mut [u8]) -> Result<(), ServiceError> {
        let mut crop = AImageCropRect::default();
        // SAFETY: the image is live; the plane slices are read only while it is.
        unsafe {
            if AImage_getCropRect(image.0, &mut crop) != AMEDIA_OK {
                crop = AImageCropRect {
                    left: 0,
                    top: 0,
                    right: self.width as i32,
                    bottom: self.height as i32,
                };
            }
            let width = (crop.right - crop.left).clamp(0, self.width as i32) as usize;
            let height = (crop.bottom - crop.top).clamp(0, self.height as i32) as usize;
            let plane = |index: i32, sub: i32| -> Result<Plane<'_>, ServiceError> {
                let (mut data, mut length, mut row, mut pixel) =
                    (ptr::null_mut::<u8>(), 0i32, 0i32, 0i32);
                if AImage_getPlaneData(image.0, index, &mut data, &mut length) != AMEDIA_OK
                    || AImage_getPlaneRowStride(image.0, index, &mut row) != AMEDIA_OK
                    || AImage_getPlanePixelStride(image.0, index, &mut pixel) != AMEDIA_OK
                    || data.is_null()
                {
                    return Err(ServiceError::system("a frame's plane could not be read"));
                }
                let start = (crop.top / sub) as usize * row as usize
                    + (crop.left / sub) as usize * pixel as usize;
                let all = std::slice::from_raw_parts(data, length as usize);
                Ok(Plane {
                    data: all.get(start..).unwrap_or(&[]),
                    row_stride: row as usize,
                    pixel_stride: pixel as usize,
                })
            };
            let (y, cb, cr) = (plane(0, 1)?, plane(1, 2)?, plane(2, 2)?);
            // A plane shorter than the crop it is read through is no frame.
            let fits = |plane: &Plane, rows: usize, columns: usize| {
                rows == 0
                    || plane.data.len()
                        >= (rows - 1) * plane.row_stride + (columns - 1) * plane.pixel_stride + 1
            };
            if width == 0
                || height == 0
                || !fits(&y, height, width)
                || !fits(&cb, height.div_ceil(2), width.div_ceil(2))
                || !fits(&cr, height.div_ceil(2), width.div_ceil(2))
            {
                return Err(ServiceError::system("a frame is smaller than its video"));
            }
            if width != self.width as usize || height != self.height as usize {
                // A crop smaller than the video: the rest of the frame stays black.
                rgba.fill(0);
                let mut cropped = vec![0u8; width * height * 4];
                yuv::to_rgba(&y, &cb, &cr, width, height, self.colour, &mut cropped);
                let stride = self.width as usize * 4;
                for (row, line) in cropped.chunks(width * 4).enumerate() {
                    rgba[row * stride..row * stride + line.len()].copy_from_slice(line);
                }
            } else {
                yuv::to_rgba(&y, &cb, &cr, width, height, self.colour, rgba);
            }
        }
        Ok(())
    }

    fn emit(
        &self,
        (image, pts): (Image, f64),
        rgba: &mut [u8],
    ) -> Result<Option<FrameTiming>, ServiceError> {
        self.convert(&image, rgba)?;
        Ok(Some(FrameTiming {
            pts_ms: pts,
            dts_ms: pts,
        }))
    }
}

impl VideoFrameSource for Source {
    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn next_frame(&mut self, rgba: &mut [u8]) -> Result<Option<FrameTiming>, ServiceError> {
        if let Some(pending) = self.pending.take() {
            return self.emit(pending, rgba);
        }
        loop {
            let Some((image, pts)) = self.decode()? else {
                // A target past the last frame shows the last frame.
                self.seek_target = None;
                return match self.held.take() {
                    Some(held) => self.emit(held, rgba),
                    None => Ok(None),
                };
            };
            match self.seek_target {
                Some(target) if pts <= target => {
                    self.held = Some((image, pts));
                }
                Some(_) => {
                    self.seek_target = None;
                    match self.held.take() {
                        Some(held) => {
                            self.pending = Some((image, pts));
                            return self.emit(held, rgba);
                        }
                        None => return self.emit((image, pts), rgba),
                    }
                }
                None => return self.emit((image, pts), rgba),
            }
        }
    }

    fn seek(&mut self, position_ms: f64) -> Result<(), ServiceError> {
        self.held = None;
        self.pending = None;
        let position_us = (position_ms.max(0.0) * 1000.0) as i64;
        // SAFETY: the extractor and codec are live.
        unsafe {
            let status = AMediaExtractor_seekTo(
                self.extractor,
                position_us,
                AMEDIAEXTRACTOR_SEEK_PREVIOUS_SYNC,
            );
            if status != AMEDIA_OK {
                return Err(failure("seeking", status));
            }
            let status = AMediaCodec_flush(self.codec);
            if status != AMEDIA_OK {
                return Err(failure("flushing the decoder", status));
            }
        }
        self.input_done = false;
        self.output_done = false;
        self.seek_target = Some(position_ms);
        Ok(())
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        self.held = None;
        self.pending = None;
        // SAFETY: each was created by `open` and is released once, the codec
        // before the reader whose window it renders into.
        unsafe {
            AMediaCodec_stop(self.codec);
            AMediaCodec_delete(self.codec);
            AImageReader_delete(self.reader);
            AMediaExtractor_delete(self.extractor);
        }
    }
}

/// What `Source::open` has created so far, released if it gives up.
struct Partial {
    extractor: *mut AMediaExtractor,
    format: *mut AMediaFormat,
    reader: *mut AImageReader,
    codec: *mut AMediaCodec,
    file: Option<File>,
}

impl Default for Partial {
    fn default() -> Self {
        Self {
            extractor: ptr::null_mut(),
            format: ptr::null_mut(),
            reader: ptr::null_mut(),
            codec: ptr::null_mut(),
            file: None,
        }
    }
}

impl Partial {
    /// Everything but the track format, now the source's to release.
    fn take(
        &mut self,
    ) -> (
        *mut AMediaExtractor,
        *mut AMediaCodec,
        *mut AImageReader,
        Option<File>,
    ) {
        let taken = (self.extractor, self.codec, self.reader, self.file.take());
        self.extractor = ptr::null_mut();
        self.codec = ptr::null_mut();
        self.reader = ptr::null_mut();
        taken
    }
}

impl Drop for Partial {
    fn drop(&mut self) {
        // SAFETY: each non-null pointer was created by `Source::open` and not
        // handed on.
        unsafe {
            if !self.format.is_null() {
                AMediaFormat_delete(self.format);
            }
            if !self.codec.is_null() {
                AMediaCodec_delete(self.codec);
            }
            if !self.reader.is_null() {
                AImageReader_delete(self.reader);
            }
            if !self.extractor.is_null() {
                AMediaExtractor_delete(self.extractor);
            }
        }
    }
}
