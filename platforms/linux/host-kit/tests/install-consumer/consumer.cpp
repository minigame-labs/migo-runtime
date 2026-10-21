#include <migo/linux/qt6/managed_session.hpp>
#include <migo/linux/qt6/media.hpp>
#include <migo/linux/qt6/x11_surface_view.hpp>

#include <memory>
#include <type_traits>

static_assert(!std::is_move_constructible_v<migo::linux_host::SurfaceHost>);

// Both ownership shapes must be reachable from the installed package. The Bound
// one takes an App-owned controller; the Managed one takes an App-owned engine
// and owns the Session itself.
void accepts_installed_types(migo::linux_host::SurfaceHost &host, QWidget &parent) {
    migo::linux_host::qt6::MigoQtX11SurfaceView view(host, parent);
}

void accepts_the_managed_shape(MigoEngine &engine, QWidget &parent) {
    migo::linux_host::qt6::MigoManagedSession managed(engine, parent);
    (void)managed.isValid();
}

// The optional component, its GStreamer closing through the exported targets.
void accepts_the_videos(MigoEngine &engine, QWidget &parent) {
    migo::linux_host::qt6::MigoManagedSession::Options options;
    options.videos = std::make_shared<migo::linux_host::qt6::MigoQtMedia>();
    migo::linux_host::qt6::MigoManagedSession managed(engine, parent, options);
    (void)managed.isValid();
}

int main() {
    // This binary is a link-closure test, not a GUI smoke test. The external
    // function above keeps the Qt adapter constructor/destructor referenced;
    // executing it would require a QApplication and an active display.
    return 0;
}
