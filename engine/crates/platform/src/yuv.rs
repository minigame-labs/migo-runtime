//! Decoded video, YUV 4:2:0, as the straight RGBA8 content draws.
//!
//! A platform decoder hands out three planes with their own strides -- a
//! separate Cb and Cr (I420), interleaved ones (NV12, NV21: a pixel stride of
//! two), whatever its codec produced -- so the planes are read through their
//! strides, never assumed packed. The matrix and range are the stream's own,
//! as its decoder reports them.

/// Which YUV-to-RGB matrix a stream was encoded with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Matrix {
    Bt601,
    Bt709,
    Bt2020,
}

/// What a stream says of itself: its matrix, and whether luma spans 0..255
/// (full) or 16..235 (limited, the broadcast default).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Colour {
    pub matrix: Matrix,
    pub full_range: bool,
}

impl Colour {
    /// The common reading of a stream that says nothing: limited range, BT.709
    /// from 720 lines up and BT.601 below.
    pub(crate) fn assumed(height: u32) -> Self {
        Self {
            matrix: if height >= 720 {
                Matrix::Bt709
            } else {
                Matrix::Bt601
            },
            full_range: false,
        }
    }
}

/// One plane: its bytes from the crop's origin, the distance between rows and
/// between neighbouring samples.
pub(crate) struct Plane<'a> {
    pub data: &'a [u8],
    pub row_stride: usize,
    pub pixel_stride: usize,
}

/// The conversion in 16.16 fixed point.
struct Coefficients {
    y_offset: i32,
    y_scale: i32,
    r_cr: i32,
    g_cb: i32,
    g_cr: i32,
    b_cb: i32,
}

impl Coefficients {
    fn of(colour: Colour) -> Self {
        let (kr, kb) = match colour.matrix {
            Matrix::Bt601 => (0.299, 0.114),
            Matrix::Bt709 => (0.2126, 0.0722),
            Matrix::Bt2020 => (0.2627, 0.0593),
        };
        let kg = 1.0 - kr - kb;
        let (y_offset, y_scale, c_scale) = if colour.full_range {
            (0, 1.0, 1.0)
        } else {
            (16, 255.0 / 219.0, 255.0 / 224.0)
        };
        let fixed = |value: f64| (value * 65536.0).round() as i32;
        Self {
            y_offset,
            y_scale: fixed(y_scale),
            r_cr: fixed(2.0 * (1.0 - kr) * c_scale),
            g_cb: fixed(2.0 * kb * (1.0 - kb) / kg * c_scale),
            g_cr: fixed(2.0 * kr * (1.0 - kr) / kg * c_scale),
            b_cb: fixed(2.0 * (1.0 - kb) * c_scale),
        }
    }
}

/// Write `width` x `height` pixels of 4:2:0 `y`, `cb`, `cr` into `rgba`, row
/// by row, top first, alpha opaque. The chroma planes hold one sample per two
/// by two pixels.
pub(crate) fn to_rgba(
    y: &Plane,
    cb: &Plane,
    cr: &Plane,
    width: usize,
    height: usize,
    colour: Colour,
    rgba: &mut [u8],
) {
    let c = Coefficients::of(colour);
    let clamp = |value: i32| ((value + 32768) >> 16).clamp(0, 255) as u8;
    for row in 0..height {
        let y_row = row * y.row_stride;
        let c_row = (row / 2) * cb.row_stride;
        let r_row = (row / 2) * cr.row_stride;
        let out = &mut rgba[row * width * 4..(row + 1) * width * 4];
        for column in 0..width {
            let luma =
                (i32::from(y.data[y_row + column * y.pixel_stride]) - c.y_offset) * c.y_scale;
            let blue = i32::from(cb.data[c_row + (column / 2) * cb.pixel_stride]) - 128;
            let red = i32::from(cr.data[r_row + (column / 2) * cr.pixel_stride]) - 128;
            let pixel = &mut out[column * 4..column * 4 + 4];
            pixel[0] = clamp(luma + c.r_cr * red);
            pixel[1] = clamp(luma - c.g_cb * blue - c.g_cr * red);
            pixel[2] = clamp(luma + c.b_cb * blue);
            pixel[3] = 255;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One 2x2 block of `luma` with the given chroma, in I420 and in NV12.
    fn convert(luma: u8, cb: u8, cr: u8, colour: Colour) -> [u8; 4] {
        let y = [luma; 4];
        let mut i420 = [0u8; 16];
        to_rgba(
            &Plane {
                data: &y,
                row_stride: 2,
                pixel_stride: 1,
            },
            &Plane {
                data: &[cb],
                row_stride: 1,
                pixel_stride: 1,
            },
            &Plane {
                data: &[cr],
                row_stride: 1,
                pixel_stride: 1,
            },
            2,
            2,
            colour,
            &mut i420,
        );
        // NV12: Cb and Cr interleaved, read through a pixel stride of two.
        let interleaved = [cb, cr];
        let mut nv12 = [0u8; 16];
        to_rgba(
            &Plane {
                data: &y,
                row_stride: 2,
                pixel_stride: 1,
            },
            &Plane {
                data: &interleaved,
                row_stride: 2,
                pixel_stride: 2,
            },
            &Plane {
                data: &interleaved[1..],
                row_stride: 2,
                pixel_stride: 2,
            },
            2,
            2,
            colour,
            &mut nv12,
        );
        assert_eq!(i420, nv12);
        assert!(i420.chunks(4).all(|pixel| pixel == &i420[..4]));
        [i420[0], i420[1], i420[2], i420[3]]
    }

    const LIMITED_601: Colour = Colour {
        matrix: Matrix::Bt601,
        full_range: false,
    };
    const FULL_709: Colour = Colour {
        matrix: Matrix::Bt709,
        full_range: true,
    };

    #[test]
    fn black_white_and_grey_are_themselves() {
        assert_eq!(convert(16, 128, 128, LIMITED_601), [0, 0, 0, 255]);
        assert_eq!(convert(235, 128, 128, LIMITED_601), [255, 255, 255, 255]);
        assert_eq!(convert(0, 128, 128, FULL_709), [0, 0, 0, 255]);
        assert_eq!(convert(128, 128, 128, FULL_709), [128, 128, 128, 255]);
    }

    #[test]
    fn the_primaries_come_back_from_their_published_yuv() {
        // BT.601 limited-range red, green and blue (ITU-R BT.601 Table 3).
        let near = |a: [u8; 4], b: [u8; 3]| a[..3].iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 2);
        assert!(near(convert(81, 90, 240, LIMITED_601), [255, 0, 0]));
        assert!(near(convert(145, 54, 34, LIMITED_601), [0, 255, 0]));
        assert!(near(convert(41, 240, 110, LIMITED_601), [0, 0, 255]));
        // BT.709 full-range red.
        assert!(near(convert(54, 99, 255, FULL_709), [255, 0, 0]));
    }

    #[test]
    fn a_stream_that_says_nothing_is_read_by_its_size() {
        assert_eq!(Colour::assumed(480).matrix, Matrix::Bt601);
        assert_eq!(Colour::assumed(1080).matrix, Matrix::Bt709);
        assert!(!Colour::assumed(1080).full_range);
    }
}
