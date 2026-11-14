#ifndef MIGO_WINDOWS_HOST_KIT_IMAGE_CODEC_HPP_
#define MIGO_WINDOWS_HOST_KIT_IMAGE_CODEC_HPP_

// Internal to the Host Kit, not installed: images read and written through the
// Windows Imaging Component.

#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#ifndef NOMINMAX
#define NOMINMAX
#endif
#ifndef UNICODE
#define UNICODE
#endif
#include <windows.h>
#include <wincodec.h>
#include <wrl/client.h>

#include <cstdint>
#include <optional>
#include <string>

namespace migo::windows_host::codec {

using Microsoft::WRL::ComPtr;

/// The imaging factory, for the calling thread's apartment; null when WIC
/// cannot be made.
[[nodiscard]] ComPtr<IWICImagingFactory> factory();

/// An image opened: its first frame, its container's format, its size as
/// stored and its EXIF orientation (1 to 8; 1 when it has none).
struct Opened {
    ComPtr<IWICBitmapDecoder> decoder;
    ComPtr<IWICBitmapFrameDecode> frame;
    GUID container{};
    std::uint32_t width = 0;
    std::uint32_t height = 0;
    int orientation = 1;

    /// Whether the orientation is a quarter turn, which exchanges width and height.
    [[nodiscard]] bool turned() const noexcept { return orientation >= 5 && orientation <= 8; }
    [[nodiscard]] std::uint32_t uprightWidth() const noexcept { return turned() ? height : width; }
    [[nodiscard]] std::uint32_t uprightHeight() const noexcept { return turned() ? width : height; }
};

/// The image at `path`, or at the start of `file`; none, with `error` --
/// "file not found", "invalid image" -- when there is none to read.
[[nodiscard]] std::optional<Opened> open(IWICImagingFactory &wic, const std::wstring &path, std::string &error);
[[nodiscard]] std::optional<Opened> open(IWICImagingFactory &wic, HANDLE file, std::string &error);

/// The image upright -- its orientation applied, not carried -- at `width` x
/// `height` (upright pixels): scaled first, as stored, then turned.
[[nodiscard]] ComPtr<IWICBitmapSource> upright(IWICImagingFactory &wic, const Opened &image, std::uint32_t width,
                                               std::uint32_t height);

/// Whether `source`'s pixels can be transparent.
[[nodiscard]] bool hasAlpha(IWICImagingFactory &wic, IWICBitmapSource &source);

/// Writes `source` to `path` as `container` (JPEG or PNG), a JPEG at `quality`
/// (0 to 100). False, with `error`, the file removed, when it cannot.
[[nodiscard]] bool write(IWICImagingFactory &wic, IWICBitmapSource &source, const std::wstring &path,
                         const GUID &container, int quality, std::string &error);

/// The suffix a format the engine draws is staged under -- jpg, png, gif,
/// bmp, webp -- or null for any other.
[[nodiscard]] const wchar_t *drawableSuffix(const GUID &container) noexcept;

/// Every file name suffix WIC's decoders read, without the dot.
[[nodiscard]] std::wstring decoderExtensions(IWICImagingFactory &wic);

}  // namespace migo::windows_host::codec

#endif  // MIGO_WINDOWS_HOST_KIT_IMAGE_CODEC_HPP_
