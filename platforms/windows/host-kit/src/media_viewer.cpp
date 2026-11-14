#include "media_viewer.hpp"

#include "image_codec.hpp"
#include "videos.hpp"

#include <commctrl.h>
#include <d2d1.h>

#include <algorithm>
#include <cmath>
#include <cstdio>
#include <exception>
#include <mutex>
#include <string>
#include <utility>

namespace migo::windows_host {

namespace {

using codec::ComPtr;

constexpr wchar_t kViewerClass[] = L"MigoMediaViewer";
constexpr UINT_PTR kFollowId = 0x4D69676F + 8;
// The arrows' and the close box's reach, in device-independent pixels.
constexpr float kArrowReach = 72;
constexpr float kCloseReach = 56;

[[noreturn]] void fatal(const char *why) {
    std::fprintf(stderr, "migo windows host kit: %s\n", why);
    std::fflush(stderr);
    std::terminate();
}

}  // namespace

class MediaViewer::Impl final {
public:
    Impl(HWND owner, std::vector<Item> items, int current, std::function<void()> closed, VideoMaker videos)
        : owner_(owner), items_(std::move(items)), closed_(std::move(closed)), videos_(std::move(videos)) {
        static std::once_flag registered;
        std::call_once(registered, [] {
            WNDCLASSEXW viewer{};
            viewer.cbSize = sizeof viewer;
            viewer.lpfnWndProc = &Impl::procedure;
            viewer.hInstance = GetModuleHandleW(nullptr);
            viewer.hCursor = LoadCursorW(nullptr, IDC_ARROW);
            viewer.lpszClassName = kViewerClass;
            if (RegisterClassExW(&viewer) == 0) fatal("RegisterClassExW failed for the media viewer");
        });
        current_ = std::clamp(current, 0, std::max(0, static_cast<int>(items_.size()) - 1));
        window_ = CreateWindowExW(WS_EX_TOOLWINDOW, kViewerClass, L"", WS_POPUP, 0, 0, 1, 1, owner_, nullptr,
                                  GetModuleHandleW(nullptr), this);
        if (window_ == nullptr) fatal("CreateWindowExW failed for the media viewer");
        if (owner_ != nullptr) {
            following_ = SetWindowSubclass(owner_, &Impl::follow, kFollowId, reinterpret_cast<DWORD_PTR>(this)) != FALSE;
            // Modal to the App's window.
            EnableWindow(owner_, FALSE);
        }
        wic_ = codec::factory();
        D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, d2d_.GetAddressOf());
        place();
        ShowWindow(window_, SW_SHOW);
        SetForegroundWindow(window_);
        SetFocus(window_);
        present(current_);
    }

    ~Impl() {
        // The App's window is enabled before the viewer goes, so the system
        // gives it back the activation.
        if (owner_ != nullptr) EnableWindow(owner_, TRUE);
        if (following_) RemoveWindowSubclass(owner_, &Impl::follow, kFollowId);
        if (window_ != nullptr) {
            SetWindowLongPtrW(window_, GWLP_USERDATA, 0);
            DestroyWindow(window_);
        }
        video_.reset();
        for (const Item &item : items_) {
            if (item.file != nullptr && item.file != INVALID_HANDLE_VALUE) CloseHandle(item.file);
        }
    }

    HWND hwnd() const noexcept { return window_; }
    int current() const noexcept { return current_; }
    int count() const noexcept { return static_cast<int>(items_.size()); }
    bool showsImage() const noexcept { return decoded_; }

    void showPrevious() {
        if (current_ > 0) present(current_ - 1);
    }
    void showNext() {
        if (current_ + 1 < count()) present(current_ + 1);
    }

private:
    static LRESULT CALLBACK procedure(HWND window, UINT message, WPARAM wparam, LPARAM lparam) {
        if (message == WM_NCCREATE) {
            const auto *create = reinterpret_cast<const CREATESTRUCTW *>(lparam);
            SetWindowLongPtrW(window, GWLP_USERDATA, reinterpret_cast<LONG_PTR>(create->lpCreateParams));
        }
        auto *impl = reinterpret_cast<Impl *>(GetWindowLongPtrW(window, GWLP_USERDATA));
        if (impl == nullptr) return DefWindowProcW(window, message, wparam, lparam);
        switch (message) {
            case WM_PAINT:
                impl->paint();
                return 0;
            case WM_ERASEBKGND:
                return 1;
            case WM_SIZE:
                if (impl->target_ != nullptr) impl->target_->Resize(D2D1::SizeU(LOWORD(lparam), HIWORD(lparam)));
                impl->render();
                InvalidateRect(window, nullptr, FALSE);
                return 0;
            case WM_DPICHANGED:
                impl->target_.Reset();
                impl->place();
                impl->render();
                return 0;
            case WM_KEYDOWN:
                if (wparam == VK_ESCAPE) {
                    impl->close();
                } else if (wparam == VK_LEFT) {
                    impl->showPrevious();
                } else if (wparam == VK_RIGHT) {
                    impl->showNext();
                }
                return 0;
            case WM_LBUTTONUP:
                impl->clicked(static_cast<short>(LOWORD(lparam)), static_cast<short>(HIWORD(lparam)));
                return 0;
            case WM_CLOSE:
                impl->close();
                return 0;
            default:
                return DefWindowProcW(window, message, wparam, lparam);
        }
    }

    /// The App's window: the viewer keeps its client area's place and size.
    static LRESULT CALLBACK follow(HWND window, UINT message, WPARAM wparam, LPARAM lparam, UINT_PTR,
                                   DWORD_PTR reference) {
        const LRESULT result = DefSubclassProc(window, message, wparam, lparam);
        auto *impl = reinterpret_cast<Impl *>(reference);
        if (message == WM_WINDOWPOSCHANGED) {
            impl->place();
        } else if (message == WM_NCDESTROY) {
            impl->following_ = false;
            RemoveWindowSubclass(window, &Impl::follow, kFollowId);
        }
        return result;
    }

    void close() {
        if (closing_) return;
        closing_ = true;
        video_.reset();
        ShowWindow(window_, SW_HIDE);
        if (closed_) closed_();
    }

    float scale() const {
        return static_cast<float>(std::max<UINT>(GetDpiForWindow(window_), USER_DEFAULT_SCREEN_DPI / 4)) /
               static_cast<float>(USER_DEFAULT_SCREEN_DPI);
    }

    void place() {
        RECT area{};
        if (owner_ != nullptr && GetClientRect(owner_, &area)) {
            POINT origin{0, 0};
            ClientToScreen(owner_, &origin);
            SetWindowPos(window_, nullptr, origin.x, origin.y, std::max(1L, area.right), std::max(1L, area.bottom),
                         SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER);
        }
        placeVideo();
    }

    /// A video plays between the arrows, under the close box: they stay the viewer's.
    void placeVideo() {
        if (video_ == nullptr) return;
        RECT area{};
        GetClientRect(window_, &area);
        const float reach = scale();
        const int side = static_cast<int>(kArrowReach * reach);
        const int top = static_cast<int>(kCloseReach * reach);
        video_->place(side, top, std::max(0L, area.right - 2 * side), std::max(0L, area.bottom - top));
    }

    void clicked(int x, int y) {
        RECT area{};
        GetClientRect(window_, &area);
        const float reach = scale();
        if (x >= area.right - static_cast<int>(kCloseReach * reach) && y < static_cast<int>(kCloseReach * reach)) {
            close();
        } else if (x < static_cast<int>(kArrowReach * reach)) {
            showPrevious();
        } else if (x >= area.right - static_cast<int>(kArrowReach * reach)) {
            showNext();
        }
    }

    void present(int index) {
        current_ = index;
        video_.reset();
        const Item &item = items_[static_cast<std::size_t>(current_)];
        if (item.video && videos_) {
            // Its own handle to the file the viewer holds: it may be shown again.
            HANDLE file = INVALID_HANDLE_VALUE;
            if (item.file != nullptr && item.file != INVALID_HANDLE_VALUE) {
                DuplicateHandle(GetCurrentProcess(), item.file, GetCurrentProcess(), &file, 0, FALSE,
                                DUPLICATE_SAME_ACCESS);
            }
            video_ = videos_(window_, file == INVALID_HANDLE_VALUE ? nullptr : file, item.url, item.poster);
            placeVideo();
        }
        render();
        InvalidateRect(window_, nullptr, FALSE);
    }

    bool ensureTarget() {
        if (target_ != nullptr) return true;
        if (d2d_ == nullptr) return false;
        RECT area{};
        GetClientRect(window_, &area);
        const float dpi = 96.0F * scale();
        const D2D1_RENDER_TARGET_PROPERTIES properties =
            D2D1::RenderTargetProperties(D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1::PixelFormat(), dpi, dpi);
        const D2D1_HWND_RENDER_TARGET_PROPERTIES surface = D2D1::HwndRenderTargetProperties(
            window_, D2D1::SizeU(static_cast<UINT32>(std::max(1L, area.right)), static_cast<UINT32>(std::max(1L, area.bottom))));
        return SUCCEEDED(d2d_->CreateHwndRenderTarget(properties, surface, target_.GetAddressOf()));
    }

    /// The image shown, decoded upright at the size it fits the viewer at.
    void render() {
        bitmap_.Reset();
        decoded_ = false;
        if (current_ < 0 || current_ >= count() || wic_ == nullptr || !ensureTarget()) return;
        const Item &item = items_[static_cast<std::size_t>(current_)];
        // A video is the player's to draw.
        if (item.video) return;
        std::string error;
        const std::optional<codec::Opened> image = codec::open(*wic_.Get(), item.file, error);
        if (!image.has_value()) return;
        RECT area{};
        GetClientRect(window_, &area);
        const double fit = std::min(static_cast<double>(std::max(1L, area.right)) / image->uprightWidth(),
                                    static_cast<double>(std::max(1L, area.bottom)) / image->uprightHeight());
        // Never larger than it is: a small image is shown at its own size.
        const double shown = std::min(1.0, fit);
        const auto width = static_cast<std::uint32_t>(std::max(1.0, std::round(image->uprightWidth() * shown)));
        const auto height = static_cast<std::uint32_t>(std::max(1.0, std::round(image->uprightHeight() * shown)));
        ComPtr<IWICBitmapSource> source = codec::upright(*wic_.Get(), *image, width, height);
        ComPtr<IWICFormatConverter> converted;
        if (source == nullptr || FAILED(wic_->CreateFormatConverter(converted.GetAddressOf())) ||
            FAILED(converted->Initialize(source.Get(), GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, nullptr,
                                         0.0, WICBitmapPaletteTypeCustom)) ||
            FAILED(target_->CreateBitmapFromWicBitmap(converted.Get(), nullptr, bitmap_.GetAddressOf()))) {
            bitmap_.Reset();
            return;
        }
        decoded_ = true;
    }

    void paint() {
        PAINTSTRUCT painting;
        BeginPaint(window_, &painting);
        if (ensureTarget()) {
            target_->BeginDraw();
            target_->Clear(D2D1::ColorF(0, 0, 0, 1));
            const D2D1_SIZE_F size = target_->GetSize();
            ComPtr<ID2D1SolidColorBrush> ink;
            target_->CreateSolidColorBrush(D2D1::ColorF(1, 1, 1, 0.85F), ink.GetAddressOf());
            if (bitmap_ != nullptr) {
                // In pixels, centred: the bitmap was decoded at the size it is shown.
                const D2D1_SIZE_F shown = bitmap_->GetSize();
                const float left = std::round((size.width - shown.width) / 2);
                const float top = std::round((size.height - shown.height) / 2);
                target_->DrawBitmap(bitmap_.Get(), D2D1::RectF(left, top, left + shown.width, top + shown.height));
            } else if (ink != nullptr && !items_[static_cast<std::size_t>(current_)].video) {
                drawWarning(size, ink.Get());
            } else if (ink != nullptr && video_ == nullptr) {
                // A video that cannot be played here.
                drawWarning(size, ink.Get());
            }
            if (ink != nullptr) drawControls(size, ink.Get());
            if (target_->EndDraw() == static_cast<HRESULT>(D2DERR_RECREATE_TARGET)) {
                target_.Reset();
                render();
            }
        }
        EndPaint(window_, &painting);
    }

    /// An image that cannot be decoded: a warning sign in its place.
    void drawWarning(D2D1_SIZE_F size, ID2D1Brush *ink) {
        const float x = size.width / 2;
        const float y = size.height / 2;
        constexpr float kSide = 48;
        const D2D1_POINT_2F top = D2D1::Point2F(x, y - kSide / 2);
        const D2D1_POINT_2F right = D2D1::Point2F(x + kSide / 2, y + kSide / 2 - 6);
        const D2D1_POINT_2F left = D2D1::Point2F(x - kSide / 2, y + kSide / 2 - 6);
        target_->DrawLine(top, right, ink, 3);
        target_->DrawLine(right, left, ink, 3);
        target_->DrawLine(left, top, ink, 3);
        target_->DrawLine(D2D1::Point2F(x, y - 8), D2D1::Point2F(x, y + 6), ink, 3);
        target_->FillEllipse(D2D1::Ellipse(D2D1::Point2F(x, y + 12), 2, 2), ink);
    }

    /// The close box, and the arrows where there is somewhere to go.
    void drawControls(D2D1_SIZE_F size, ID2D1Brush *ink) {
        const float corner = size.width - kCloseReach / 2;
        constexpr float kHalf = 9;
        target_->DrawLine(D2D1::Point2F(corner - kHalf, kCloseReach / 2 - kHalf),
                          D2D1::Point2F(corner + kHalf, kCloseReach / 2 + kHalf), ink, 2.5F);
        target_->DrawLine(D2D1::Point2F(corner + kHalf, kCloseReach / 2 - kHalf),
                          D2D1::Point2F(corner - kHalf, kCloseReach / 2 + kHalf), ink, 2.5F);
        const float middle = size.height / 2;
        constexpr float kArm = 14;
        if (current_ > 0) {
            const float x = kArrowReach / 2;
            target_->DrawLine(D2D1::Point2F(x + kArm / 2, middle - kArm), D2D1::Point2F(x - kArm / 2, middle), ink, 3);
            target_->DrawLine(D2D1::Point2F(x - kArm / 2, middle), D2D1::Point2F(x + kArm / 2, middle + kArm), ink, 3);
        }
        if (current_ + 1 < count()) {
            const float x = size.width - kArrowReach / 2;
            target_->DrawLine(D2D1::Point2F(x - kArm / 2, middle - kArm), D2D1::Point2F(x + kArm / 2, middle), ink, 3);
            target_->DrawLine(D2D1::Point2F(x + kArm / 2, middle), D2D1::Point2F(x - kArm / 2, middle + kArm), ink, 3);
        }
    }

    const HWND owner_;
    std::vector<Item> items_;
    const std::function<void()> closed_;
    const VideoMaker videos_;
    std::unique_ptr<EmbeddedVideo> video_;
    HWND window_ = nullptr;
    bool following_ = false;
    bool closing_ = false;
    int current_ = 0;
    bool decoded_ = false;
    ComPtr<IWICImagingFactory> wic_;
    ComPtr<ID2D1Factory> d2d_;
    ComPtr<ID2D1HwndRenderTarget> target_;
    ComPtr<ID2D1Bitmap> bitmap_;
};

MediaViewer::MediaViewer(HWND__ *owner, std::vector<Item> items, int current, std::function<void()> closed,
                         VideoMaker videos)
    : impl_(std::make_unique<Impl>(owner, std::move(items), current, std::move(closed), std::move(videos))) {}
MediaViewer::~MediaViewer() = default;
HWND__ *MediaViewer::hwnd() const noexcept { return impl_->hwnd(); }
int MediaViewer::current() const noexcept { return impl_->current(); }
int MediaViewer::count() const noexcept { return impl_->count(); }
bool MediaViewer::showsImage() const noexcept { return impl_->showsImage(); }
void MediaViewer::showPrevious() { impl_->showPrevious(); }
void MediaViewer::showNext() { impl_->showNext(); }

}  // namespace migo::windows_host
