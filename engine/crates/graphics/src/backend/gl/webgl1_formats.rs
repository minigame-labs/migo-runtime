//! WebGL 1's texture formats that OpenGL ES 3.0 does not take as they are: the unsized float and half-float uploads of
//! OES_texture_float and OES_texture_half_float, the unsized depth ones of WEBGL_depth_texture and the sRGB ones of
//! EXT_sRGB. The facade takes them only in a WebGL 1 context with the extension enabled -- WebGL 2 refuses every one --
//! so the renderer maps them wherever they arrive: each is uploaded as the sized format ES 3.0 has for it, and a
//! luminance or alpha float as a red or red-green one read through a swizzle, as browsers upload them on ES 3.0. A
//! depth image is read through one too: OES_depth_texture samples depth as luminance, (d, d, d, 1), where ES 3.0 has
//! (d, 0, 0, 1).

/// `HALF_FLOAT_OES`, OES_texture_half_float's type; ES 3.0's `HALF_FLOAT` has another value.
pub(crate) const HALF_FLOAT_OES: u32 = 0x8D61;
/// EXT_sRGB's unsized formats.
pub(crate) const SRGB_EXT: u32 = 0x8C40;
pub(crate) const SRGB_ALPHA_EXT: u32 = 0x8C42;
/// WEBGL_depth_texture's packed depth-stencil type, ES 3.0's UNSIGNED_INT_24_8.
const UNSIGNED_INT_24_8: u32 = 0x84FA;

const RED: u32 = 0x1903;
const RG: u32 = 0x8227;
const R16F: u32 = 0x822D;
const RG16F: u32 = 0x822F;
const R32F: u32 = 0x822E;
const RG32F: u32 = 0x8230;
const RGB16F: u32 = 0x881B;
const RGB32F: u32 = 0x8815;
const RGBA16F: u32 = 0x881A;
const RGBA32F: u32 = 0x8814;
const SRGB8: u32 = 0x8C41;
const SRGB8_ALPHA8: u32 = 0x8C43;
const DEPTH_COMPONENT16: u32 = 0x81A5;
const DEPTH_COMPONENT24: u32 = 0x81A6;
const DEPTH24_STENCIL8: u32 = 0x88F0;
const ZERO: u32 = 0;
const ONE: u32 = 1;

/// The swizzle a texture reads its channels through: TEXTURE_SWIZZLE_R, _G, _B, _A.
pub(crate) type Swizzle = [u32; 4];

/// The identity swizzle.
pub(crate) const IDENTITY: Swizzle = [glow::RED, glow::GREEN, glow::BLUE, glow::ALPHA];

/// Red read as luminance: (r, r, r, 1).
const LUMINANCE: Swizzle = [glow::RED, glow::RED, glow::RED, ONE];

/// A WebGL 1 upload as the driver takes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DriverFormat {
    pub internalformat: i32,
    pub format: u32,
    pub type_: u32,
    /// The swizzle the texture reads the upload through; `None` for the identity.
    pub swizzle: Option<Swizzle>,
}

/// The driver's form of a full upload of (`internalformat`, `format`, `type_`), or `None` when ES 3.0 takes it as it is.
pub(crate) fn driver_format(internalformat: i32, format: u32, type_: u32) -> Option<DriverFormat> {
    let float = type_ == glow::FLOAT;
    let half = type_ == HALF_FLOAT_OES;
    let mapped = |internalformat: u32, format: u32, type_: u32, swizzle: Option<Swizzle>| {
        Some(DriverFormat {
            internalformat: internalformat as i32,
            format,
            type_,
            swizzle,
        })
    };
    let driver_type = if half { glow::HALF_FLOAT } else { type_ };
    if internalformat as u32 != format {
        return None;
    }
    match format {
        glow::RGBA if float || half => mapped(
            if float { RGBA32F } else { RGBA16F },
            glow::RGBA,
            driver_type,
            None,
        ),
        glow::RGB if float || half => mapped(
            if float { RGB32F } else { RGB16F },
            glow::RGB,
            driver_type,
            None,
        ),
        glow::LUMINANCE if float || half => mapped(
            if float { R32F } else { R16F },
            RED,
            driver_type,
            Some(LUMINANCE),
        ),
        glow::ALPHA if float || half => mapped(
            if float { R32F } else { R16F },
            RED,
            driver_type,
            Some([ZERO, ZERO, ZERO, glow::RED]),
        ),
        glow::LUMINANCE_ALPHA if float || half => mapped(
            if float { RG32F } else { RG16F },
            RG,
            driver_type,
            Some([glow::RED, glow::RED, glow::RED, glow::GREEN]),
        ),
        glow::DEPTH_COMPONENT if type_ == glow::UNSIGNED_SHORT => mapped(
            DEPTH_COMPONENT16,
            glow::DEPTH_COMPONENT,
            type_,
            Some(LUMINANCE),
        ),
        glow::DEPTH_COMPONENT if type_ == glow::UNSIGNED_INT => mapped(
            DEPTH_COMPONENT24,
            glow::DEPTH_COMPONENT,
            type_,
            Some(LUMINANCE),
        ),
        glow::DEPTH_STENCIL if type_ == UNSIGNED_INT_24_8 => mapped(
            DEPTH24_STENCIL8,
            glow::DEPTH_STENCIL,
            type_,
            Some(LUMINANCE),
        ),
        SRGB_EXT if type_ == glow::UNSIGNED_BYTE => mapped(SRGB8, glow::RGB, type_, None),
        SRGB_ALPHA_EXT if type_ == glow::UNSIGNED_BYTE => {
            mapped(SRGB8_ALPHA8, glow::RGBA, type_, None)
        }
        _ => None,
    }
}

/// The driver's (`format`, `type_`) for a sub-image upload into an image a full upload of WebGL 1's made: the format
/// and type of [`driver_format`]'s for the same pair. Anything else is as it came.
pub(crate) fn driver_sub_format(format: u32, type_: u32) -> (u32, u32) {
    match driver_format(format as i32, format, type_) {
        Some(mapped) => (mapped.format, mapped.type_),
        None => (format, type_),
    }
}

/// The format a TexImageSource is packed as for (`format`, `type_`): EXT_sRGB's unsized formats are packed as the RGB
/// or RGBA bytes they hold; every other format packs as itself (`unpack_convert::pack_source` knows WebGL 1's
/// luminance and alpha, and `HALF_FLOAT_OES`).
pub(crate) fn source_pack_format(format: u32) -> u32 {
    match format {
        SRGB_EXT => glow::RGB,
        SRGB_ALPHA_EXT => glow::RGBA,
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsized_floats_and_half_floats_are_uploaded_sized() {
        assert_eq!(
            driver_format(glow::RGBA as i32, glow::RGBA, glow::FLOAT),
            Some(DriverFormat {
                internalformat: RGBA32F as i32,
                format: glow::RGBA,
                type_: glow::FLOAT,
                swizzle: None
            })
        );
        assert_eq!(
            driver_format(glow::RGB as i32, glow::RGB, HALF_FLOAT_OES),
            Some(DriverFormat {
                internalformat: RGB16F as i32,
                format: glow::RGB,
                type_: glow::HALF_FLOAT,
                swizzle: None
            })
        );
        let alpha = driver_format(glow::ALPHA as i32, glow::ALPHA, glow::FLOAT).unwrap();
        assert_eq!((alpha.internalformat as u32, alpha.format), (R32F, RED));
        assert_eq!(
            alpha.swizzle,
            Some([ZERO, ZERO, ZERO, glow::RED]),
            "alpha reads as (0, 0, 0, a)"
        );
        let luminance_alpha = driver_format(
            glow::LUMINANCE_ALPHA as i32,
            glow::LUMINANCE_ALPHA,
            HALF_FLOAT_OES,
        )
        .unwrap();
        assert_eq!(
            (
                luminance_alpha.internalformat as u32,
                luminance_alpha.format
            ),
            (RG16F, RG)
        );
        assert_eq!(
            luminance_alpha.swizzle,
            Some([glow::RED, glow::RED, glow::RED, glow::GREEN])
        );
    }

    #[test]
    fn depth_and_srgb_are_uploaded_sized() {
        let depth = driver_format(
            glow::DEPTH_COMPONENT as i32,
            glow::DEPTH_COMPONENT,
            glow::UNSIGNED_INT,
        )
        .unwrap();
        assert_eq!(depth.internalformat as u32, DEPTH_COMPONENT24);
        assert_eq!(
            depth.swizzle,
            Some([glow::RED, glow::RED, glow::RED, ONE]),
            "depth samples as luminance"
        );
        assert_eq!(
            driver_format(
                glow::DEPTH_STENCIL as i32,
                glow::DEPTH_STENCIL,
                UNSIGNED_INT_24_8
            )
            .map(|f| f.internalformat as u32),
            Some(DEPTH24_STENCIL8)
        );
        let srgb =
            driver_format(SRGB_ALPHA_EXT as i32, SRGB_ALPHA_EXT, glow::UNSIGNED_BYTE).unwrap();
        assert_eq!(
            (srgb.internalformat as u32, srgb.format),
            (SRGB8_ALPHA8, glow::RGBA)
        );
    }

    #[test]
    fn what_es_3_takes_is_not_mapped() {
        assert_eq!(
            driver_format(glow::RGBA as i32, glow::RGBA, glow::UNSIGNED_BYTE),
            None
        );
        assert_eq!(
            driver_format(glow::LUMINANCE as i32, glow::LUMINANCE, glow::UNSIGNED_BYTE),
            None
        );
        assert_eq!(
            driver_format(RGBA32F as i32, glow::RGBA, glow::FLOAT),
            None,
            "WebGL 2's sized float"
        );
        assert_eq!(
            driver_format(
                DEPTH_COMPONENT16 as i32,
                glow::DEPTH_COMPONENT,
                glow::UNSIGNED_SHORT
            ),
            None
        );
        assert_eq!(
            driver_sub_format(glow::LUMINANCE, glow::FLOAT),
            (RED, glow::FLOAT)
        );
        assert_eq!(
            driver_sub_format(glow::RGBA, HALF_FLOAT_OES),
            (glow::RGBA, glow::HALF_FLOAT)
        );
        assert_eq!(
            driver_sub_format(glow::RGBA, glow::UNSIGNED_BYTE),
            (glow::RGBA, glow::UNSIGNED_BYTE)
        );
        assert_eq!(source_pack_format(SRGB_EXT), glow::RGB);
    }
}
