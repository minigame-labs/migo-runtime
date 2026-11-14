#include "image_codec.hpp"

#include <propidl.h>

#include <algorithm>
#include <set>
#include <vector>

namespace migo::windows_host::codec {

namespace {

std::string reason_of(HRESULT result) {
    if (result == HRESULT_FROM_WIN32(ERROR_FILE_NOT_FOUND) || result == HRESULT_FROM_WIN32(ERROR_PATH_NOT_FOUND)) {
        return "file not found";
    }
    if (result == WINCODEC_ERR_COMPONENTNOTFOUND || result == WINCODEC_ERR_UNKNOWNIMAGEFORMAT ||
        result == WINCODEC_ERR_BADHEADER || result == WINCODEC_ERR_BADIMAGE) {
        return "invalid image";
    }
    return "decode failed";
}

std::optional<Opened> describe(ComPtr<IWICBitmapDecoder> decoder, std::string &error) {
    Opened image;
    image.decoder = std::move(decoder);
    HRESULT result = image.decoder->GetFrame(0, image.frame.GetAddressOf());
    if (SUCCEEDED(result)) result = image.decoder->GetContainerFormat(&image.container);
    if (SUCCEEDED(result)) result = image.frame->GetSize(&image.width, &image.height);
    if (FAILED(result) || image.width == 0 || image.height == 0) {
        error = FAILED(result) ? reason_of(result) : "invalid image";
        return std::nullopt;
    }
    // Where its metadata says how it is turned: the photo policy reads it from
    // whichever block the format keeps it in.
    ComPtr<IWICMetadataQueryReader> metadata;
    if (SUCCEEDED(image.frame->GetMetadataQueryReader(metadata.GetAddressOf()))) {
        PROPVARIANT value;
        PropVariantInit(&value);
        if (SUCCEEDED(metadata->GetMetadataByName(L"System.Photo.Orientation", &value)) && value.vt == VT_UI2 &&
            value.uiVal >= 1 && value.uiVal <= 8) {
            image.orientation = value.uiVal;
        }
        PropVariantClear(&value);
    }
    return image;
}

/// One flip or rotation of `source`; `source` itself where it cannot be made.
ComPtr<IWICBitmapSource> transformed(IWICImagingFactory &wic, IWICBitmapSource *source,
                                     WICBitmapTransformOptions option) {
    ComPtr<IWICBitmapFlipRotator> turn;
    if (FAILED(wic.CreateBitmapFlipRotator(turn.GetAddressOf())) || FAILED(turn->Initialize(source, option))) {
        return ComPtr<IWICBitmapSource>(source);
    }
    return turn;
}

}  // namespace

ComPtr<IWICImagingFactory> factory() {
    ComPtr<IWICImagingFactory> wic;
    if (FAILED(CoCreateInstance(CLSID_WICImagingFactory, nullptr, CLSCTX_INPROC_SERVER, IID_PPV_ARGS(wic.GetAddressOf())))) {
        return nullptr;
    }
    return wic;
}

std::optional<Opened> open(IWICImagingFactory &wic, const std::wstring &path, std::string &error) {
    ComPtr<IWICBitmapDecoder> decoder;
    const HRESULT result = wic.CreateDecoderFromFilename(path.c_str(), nullptr, GENERIC_READ,
                                                         WICDecodeMetadataCacheOnDemand, decoder.GetAddressOf());
    if (FAILED(result)) {
        error = reason_of(result);
        return std::nullopt;
    }
    return describe(std::move(decoder), error);
}

std::optional<Opened> open(IWICImagingFactory &wic, HANDLE file, std::string &error) {
    LARGE_INTEGER start{};
    if (!SetFilePointerEx(file, start, nullptr, FILE_BEGIN)) {
        error = "decode failed";
        return std::nullopt;
    }
    ComPtr<IWICBitmapDecoder> decoder;
    const HRESULT result = wic.CreateDecoderFromFileHandle(reinterpret_cast<ULONG_PTR>(file), nullptr,
                                                           WICDecodeMetadataCacheOnDemand, decoder.GetAddressOf());
    if (FAILED(result)) {
        error = reason_of(result);
        return std::nullopt;
    }
    return describe(std::move(decoder), error);
}

ComPtr<IWICBitmapSource> upright(IWICImagingFactory &wic, const Opened &image, std::uint32_t width,
                                 std::uint32_t height) {
    ComPtr<IWICBitmapSource> source = image.frame;
    // Scaled as stored: a quarter turn exchanges the sides asked for.
    const std::uint32_t stored_width = image.turned() ? height : width;
    const std::uint32_t stored_height = image.turned() ? width : height;
    if (stored_width != image.width || stored_height != image.height) {
        ComPtr<IWICBitmapScaler> scaler;
        if (FAILED(wic.CreateBitmapScaler(scaler.GetAddressOf())) ||
            FAILED(scaler->Initialize(source.Get(), stored_width, stored_height, WICBitmapInterpolationModeFant))) {
            return nullptr;
        }
        source = scaler;
    }
    // EXIF's eight orientations: what turns each upright. A rotation is
    // clockwise; the mirrored ones turn first and flip after.
    switch (image.orientation) {
        case 2:
            return transformed(wic, source.Get(), WICBitmapTransformFlipHorizontal);
        case 3:
            return transformed(wic, source.Get(), WICBitmapTransformRotate180);
        case 4:
            return transformed(wic, source.Get(), WICBitmapTransformFlipVertical);
        case 5:
            return transformed(wic, transformed(wic, source.Get(), WICBitmapTransformRotate90).Get(),
                               WICBitmapTransformFlipHorizontal);
        case 6:
            return transformed(wic, source.Get(), WICBitmapTransformRotate90);
        case 7:
            return transformed(wic, transformed(wic, source.Get(), WICBitmapTransformRotate270).Get(),
                               WICBitmapTransformFlipHorizontal);
        case 8:
            return transformed(wic, source.Get(), WICBitmapTransformRotate270);
        default:
            return source;
    }
}

bool hasAlpha(IWICImagingFactory &wic, IWICBitmapSource &source) {
    WICPixelFormatGUID format{};
    if (FAILED(source.GetPixelFormat(&format))) return false;
    ComPtr<IWICComponentInfo> info;
    ComPtr<IWICPixelFormatInfo2> pixels;
    if (FAILED(wic.CreateComponentInfo(format, info.GetAddressOf())) || FAILED(info.As(&pixels))) return false;
    BOOL transparent = FALSE;
    return SUCCEEDED(pixels->SupportsTransparency(&transparent)) && transparent != FALSE;
}

bool write(IWICImagingFactory &wic, IWICBitmapSource &source, const std::wstring &path, const GUID &container,
           int quality, std::string &error) {
    const bool jpeg = IsEqualGUID(container, GUID_ContainerFormatJpeg) != FALSE;
    UINT width = 0;
    UINT height = 0;
    ComPtr<IWICStream> stream;
    ComPtr<IWICBitmapEncoder> encoder;
    ComPtr<IWICBitmapFrameEncode> frame;
    ComPtr<IPropertyBag2> options;
    ComPtr<IWICFormatConverter> converted;
    WICPixelFormatGUID format = jpeg ? GUID_WICPixelFormat24bppBGR : GUID_WICPixelFormat32bppBGRA;
    HRESULT result = source.GetSize(&width, &height);
    if (SUCCEEDED(result)) result = wic.CreateStream(stream.GetAddressOf());
    if (SUCCEEDED(result)) result = stream->InitializeFromFilename(path.c_str(), GENERIC_WRITE);
    if (SUCCEEDED(result)) result = wic.CreateEncoder(container, nullptr, encoder.GetAddressOf());
    if (SUCCEEDED(result)) result = encoder->Initialize(stream.Get(), WICBitmapEncoderNoCache);
    if (SUCCEEDED(result)) result = encoder->CreateNewFrame(frame.GetAddressOf(), options.GetAddressOf());
    if (SUCCEEDED(result) && jpeg) {
        PROPBAG2 name{};
        name.pstrName = const_cast<LPOLESTR>(L"ImageQuality");
        VARIANT value;
        VariantInit(&value);
        value.vt = VT_R4;
        value.fltVal = static_cast<float>(std::clamp(quality, 0, 100)) / 100.0F;
        result = options->Write(1, &name, &value);
    }
    if (SUCCEEDED(result)) result = frame->Initialize(options.Get());
    if (SUCCEEDED(result)) result = frame->SetSize(width, height);
    if (SUCCEEDED(result)) result = frame->SetPixelFormat(&format);
    // What the encoder takes: BGR for a JPEG, BGRA for a PNG.
    if (SUCCEEDED(result)) result = wic.CreateFormatConverter(converted.GetAddressOf());
    if (SUCCEEDED(result)) {
        result = converted->Initialize(&source, format, WICBitmapDitherTypeNone, nullptr, 0.0,
                                       WICBitmapPaletteTypeCustom);
    }
    if (SUCCEEDED(result)) result = frame->WriteSource(converted.Get(), nullptr);
    if (SUCCEEDED(result)) result = frame->Commit();
    if (SUCCEEDED(result)) result = encoder->Commit();
    if (SUCCEEDED(result)) return true;
    error = result == HRESULT_FROM_WIN32(ERROR_DISK_FULL) ? "the disk is full" : "encode failed";
    frame.Reset();
    encoder.Reset();
    stream.Reset();
    DeleteFileW(path.c_str());
    return false;
}

const wchar_t *drawableSuffix(const GUID &container) noexcept {
    if (IsEqualGUID(container, GUID_ContainerFormatJpeg)) return L"jpg";
    if (IsEqualGUID(container, GUID_ContainerFormatPng)) return L"png";
    if (IsEqualGUID(container, GUID_ContainerFormatGif)) return L"gif";
    if (IsEqualGUID(container, GUID_ContainerFormatBmp)) return L"bmp";
    if (IsEqualGUID(container, GUID_ContainerFormatWebp)) return L"webp";
    return nullptr;
}

std::wstring decoderExtensions(IWICImagingFactory &wic) {
    std::set<std::wstring> suffixes;
    ComPtr<IEnumUnknown> decoders;
    if (SUCCEEDED(wic.CreateComponentEnumerator(WICDecoder, WICComponentEnumerateDefault, decoders.GetAddressOf()))) {
        ComPtr<IUnknown> component;
        ULONG fetched = 0;
        while (decoders->Next(1, component.ReleaseAndGetAddressOf(), &fetched) == S_OK && fetched == 1) {
            ComPtr<IWICBitmapCodecInfo> codec;
            UINT needed = 0;
            if (FAILED(component.As(&codec)) || FAILED(codec->GetFileExtensions(0, nullptr, &needed)) || needed == 0) {
                continue;
            }
            std::vector<wchar_t> listed(needed);
            if (FAILED(codec->GetFileExtensions(needed, listed.data(), &needed))) continue;
            // ".jpg,.jpeg,.jpe": each without its dot, in lower case.
            std::wstring suffix;
            for (const wchar_t *at = listed.data();; ++at) {
                if (*at == L',' || *at == L'\0') {
                    if (!suffix.empty()) suffixes.insert(suffix);
                    suffix.clear();
                    if (*at == L'\0') break;
                } else if (*at != L'.' && *at != L' ') {
                    suffix.push_back(static_cast<wchar_t>(towlower(*at)));
                }
            }
        }
    }
    std::wstring joined;
    for (const std::wstring &suffix : suffixes) {
        if (!joined.empty()) joined += L';';
        joined += suffix;
    }
    return joined;
}

}  // namespace migo::windows_host::codec
