#include "fake_migo.hpp"

#include <migo/windows/surface_host.hpp>

#include <cmath>
#include <cstdint>
#include <cstdio>
#include <exception>
#include <limits>
#include <thread>
#include <type_traits>

#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>

#include <cstdlib>
#include <string>

namespace {

int g_failures = 0;

#define CHECK(condition)                                                                    \
    do {                                                                                    \
        if (!(condition)) {                                                                 \
            std::fprintf(stderr, "%s:%d: CHECK failed: %s\n", __FILE__, __LINE__, #condition); \
            ++g_failures;                                                                   \
        }                                                                                   \
    } while (false)

using migo::windows_host::SurfaceHost;
using migo::windows_host::SurfaceMetrics;
using migo::windows_host::SurfaceState;
using migo::windows_host::Win32Target;

static_assert(!std::is_copy_constructible_v<SurfaceHost>);
static_assert(!std::is_move_constructible_v<SurfaceHost>,
              "a Session-scoped SurfaceHost must keep a stable address");

SurfaceMetrics metrics(uint32_t width = 640, uint32_t height = 360, float scale = 1.0F) {
    SurfaceMetrics value;
    value.width_pixels = width;
    value.height_pixels = height;
    value.scale_factor = scale;
    return value;
}

void finish_release(SurfaceHost &host) {
    bool released = false;
    fake_migo::set_release_status(host.generation(), MIGO_SURFACE_RELEASE_RELEASED);
    CHECK(host.poll_release(&released) == MIGO_OK);
    CHECK(released);
}

/// A child window's handle, for the descriptor: the fake reads it, never uses it.
Win32Target window(std::uintptr_t value) { return Win32Target{reinterpret_cast<void *>(value)}; }

/// The scenarios that must end the process, run each in a process of its own:
/// this executable again, told which one.
void attach_success_without_handle_scenario();
void detach_success_without_observer_scenario();
void attach_error_with_handle_scenario();
void detach_error_with_release_handle_scenario();
void (*const kTerminating[])() = {
    attach_success_without_handle_scenario,
    detach_success_without_observer_scenario,
    attach_error_with_handle_scenario,
    detach_error_with_release_handle_scenario,
};

/// Scenario `index` run in a child process, which must end through
/// std::terminate (exit code 86).
void expect_termination(int index) {
    wchar_t self[MAX_PATH];
    CHECK(GetModuleFileNameW(nullptr, self, MAX_PATH) > 0);
    std::wstring command = L"\"" + std::wstring(self) + L"\" --scenario " + std::to_wstring(index);
    STARTUPINFOW startup{};
    startup.cb = sizeof startup;
    PROCESS_INFORMATION process{};
    if (!CreateProcessW(nullptr, command.data(), nullptr, nullptr, FALSE, 0, nullptr, nullptr, &startup, &process)) {
        CHECK(false);
        return;
    }
    CHECK(WaitForSingleObject(process.hProcess, 30000) == WAIT_OBJECT_0);
    DWORD code = 0;
    CHECK(GetExitCodeProcess(process.hProcess, &code));
    CHECK(code == 86);
    CloseHandle(process.hThread);
    CloseHandle(process.hProcess);
}

void attach_success_without_handle_scenario() {
    fake_migo::reset();
    SurfaceHost host(fake_migo::session());
    fake_migo::set_attach_returns_null(true);
    if (host.attach(window(2), metrics()) !=
            MIGO_ERROR_INTERNAL ||
        host.state() != SurfaceState::Faulted || host.generation() != 1) {
        std::_Exit(87);
    }
}

void detach_success_without_observer_scenario() {
    fake_migo::reset();
    SurfaceHost host(fake_migo::session());
    if (host.attach(window(2), metrics()) != MIGO_OK) {
        std::_Exit(87);
    }
    fake_migo::set_begin_detach_returns_null(true);
    if (host.begin_detach() != MIGO_ERROR_INTERNAL ||
        host.state() != SurfaceState::Faulted) {
        std::_Exit(87);
    }
}

void attach_error_with_handle_scenario() {
    fake_migo::reset();
    SurfaceHost host(fake_migo::session());
    fake_migo::set_attach_result(MIGO_ERROR_INTERNAL);
    fake_migo::set_attach_writes_handle_on_error(true);
    if (host.attach(window(2), metrics()) !=
            MIGO_ERROR_INTERNAL ||
        host.state() != SurfaceState::Faulted || host.generation() != 0) {
        std::_Exit(87);
    }
}

void detach_error_with_release_handle_scenario() {
    fake_migo::reset();
    SurfaceHost host(fake_migo::session());
    if (host.attach(window(2), metrics()) != MIGO_OK) {
        std::_Exit(87);
    }
    fake_migo::set_begin_detach_result(MIGO_ERROR_INTERNAL);
    fake_migo::set_begin_detach_writes_handle_on_error(true);
    if (host.begin_detach() != MIGO_ERROR_INTERNAL ||
        host.state() != SurfaceState::Faulted) {
        std::_Exit(87);
    }
}

void win32_attach_update_and_release_are_exact() {
    fake_migo::reset();
    SurfaceHost host(fake_migo::session());

    CHECK(host.attach(window(0xABCD), metrics()) == MIGO_OK);
    CHECK(host.state() == SurfaceState::Attached);
    CHECK(host.generation() == 1);
    CHECK(fake_migo::calls().attach == 1);
    CHECK(fake_migo::last_surface().struct_size == sizeof(MigoSurfaceDescriptor));
    CHECK(fake_migo::last_surface().abi_version == MIGO_ABI_VERSION_CURRENT);
    CHECK(fake_migo::last_surface().generation == 1);
    CHECK(fake_migo::last_surface().platform_kind == MIGO_PLATFORM_WIN32_HWND);
    CHECK(fake_migo::last_surface().flags == MIGO_SURFACE_DESCRIPTOR_FLAG_NONE);
    CHECK(fake_migo::last_surface().width_pixels == 640);
    CHECK(fake_migo::last_surface().height_pixels == 360);
    CHECK(fake_migo::last_surface().scale_factor == 1.0F);
    CHECK(fake_migo::last_surface().color_space == MIGO_COLOR_SPACE_SRGB);
    CHECK(fake_migo::last_surface().alpha_mode == MIGO_ALPHA_MODE_OPAQUE);
    CHECK(fake_migo::last_surface().preferred_presentation_mode ==
          MIGO_PRESENTATION_MODE_DEFAULT);
    CHECK(fake_migo::last_surface().capability_flags == MIGO_SURFACE_CAPABILITY_NONE);
    CHECK(fake_migo::last_surface().platform_descriptor_size ==
          sizeof(MigoWin32HwndDescriptor));
    CHECK(fake_migo::last_surface().reserved0 == 0);
    CHECK(fake_migo::last_win32().struct_size == sizeof(MigoWin32HwndDescriptor));
    CHECK(fake_migo::last_win32().abi_version == MIGO_ABI_VERSION_CURRENT);
    CHECK(fake_migo::last_win32().platform_kind == MIGO_PLATFORM_WIN32_HWND);
    CHECK(fake_migo::last_win32().flags == MIGO_PLATFORM_DESCRIPTOR_FLAG_NONE);
    CHECK(fake_migo::last_win32().hwnd == reinterpret_cast<void *>(std::uintptr_t{0xABCD}));

    const auto resized = metrics(1280, 720, 2.0F);
    CHECK(host.update(resized) == MIGO_OK);
    CHECK(fake_migo::calls().update == 1);
    CHECK(fake_migo::last_metrics().generation == 1);
    CHECK(fake_migo::last_metrics().struct_size == sizeof(MigoSurfaceMetrics));
    CHECK(fake_migo::last_metrics().abi_version == MIGO_ABI_VERSION_CURRENT);
    CHECK(fake_migo::last_metrics().width_pixels == 1280);
    CHECK(fake_migo::last_metrics().height_pixels == 720);
    CHECK(fake_migo::last_metrics().scale_factor == 2.0F);
    CHECK(fake_migo::last_metrics().color_space == MIGO_COLOR_SPACE_SRGB);
    CHECK(fake_migo::last_metrics().alpha_mode == MIGO_ALPHA_MODE_OPAQUE);
    CHECK(fake_migo::last_metrics().preferred_presentation_mode ==
          MIGO_PRESENTATION_MODE_DEFAULT);
    CHECK(fake_migo::last_metrics().flags == MIGO_SURFACE_DESCRIPTOR_FLAG_NONE);
    CHECK(fake_migo::last_metrics().reserved0 == 0);

    auto changed_requirements = resized;
    changed_requirements.required_capabilities = MIGO_SURFACE_CAPABILITY_WIDE_COLOR;
    const int updates_before_requirement_change = fake_migo::calls().update;
    CHECK(host.update(changed_requirements) == MIGO_ERROR_INVALID_ARGUMENT);
    CHECK(fake_migo::calls().update == updates_before_requirement_change);

    CHECK(host.begin_detach() == MIGO_OK);
    CHECK(host.state() == SurfaceState::Retiring);
    bool released = true;
    fake_migo::set_release_status(host.generation(), MIGO_SURFACE_RELEASE_PENDING);
    CHECK(host.poll_release(&released) == MIGO_OK);
    CHECK(!released);
    CHECK(fake_migo::last_release_query().struct_size ==
          sizeof(MigoSurfaceReleaseStatus));
    CHECK(fake_migo::last_release_query().abi_version == MIGO_ABI_VERSION_CURRENT);
    CHECK(fake_migo::last_release_query().generation == 0);
    CHECK(fake_migo::last_release_query().state == MIGO_SURFACE_RELEASE_PENDING);
    CHECK(fake_migo::last_release_query().reserved0 == 0);
    CHECK(fake_migo::calls().destroy_release == 0);
    finish_release(host);
    CHECK(host.state() == SurfaceState::Detached);
}

void a_controller_adopting_a_session_starts_at_its_generation() {
    fake_migo::reset();
    SurfaceHost host(fake_migo::session(), 9);
    CHECK(host.attach(window(0x6666), metrics(800, 600, 1.25F)) == MIGO_OK);
    CHECK(host.generation() == 9);
    CHECK(fake_migo::last_surface().generation == 9);
    CHECK(fake_migo::last_surface().scale_factor == 1.25F);
    CHECK(host.begin_detach() == MIGO_OK);
    finish_release(host);
}

void invalid_inputs_do_not_enter_the_c_abi() {
    fake_migo::reset();
    SurfaceHost null_session(nullptr);
    CHECK(null_session.attach(window(2), metrics()) ==
          MIGO_ERROR_INVALID_ARGUMENT);

    SurfaceHost host(fake_migo::session());
    CHECK(host.attach(Win32Target{nullptr}, metrics()) == MIGO_ERROR_INVALID_ARGUMENT);
    CHECK(host.attach(window(2), metrics(0, 10)) ==
          MIGO_ERROR_INVALID_ARGUMENT);
    CHECK(host.attach(window(2), metrics(10, 0)) ==
          MIGO_ERROR_INVALID_ARGUMENT);
    CHECK(host.attach(window(2), metrics(10, 10, 0.0F)) ==
          MIGO_ERROR_INVALID_ARGUMENT);
    CHECK(host.attach(window(2),
                      metrics(10, 10, std::numeric_limits<float>::infinity())) ==
          MIGO_ERROR_INVALID_ARGUMENT);
    const int calls_before_oversized_metrics = fake_migo::calls().attach;
    fake_migo::set_attach_result(MIGO_ERROR_INVALID_ARGUMENT);
    CHECK(host.attach(window(2),
                      metrics(std::numeric_limits<uint32_t>::max(), 10)) ==
          MIGO_ERROR_INVALID_ARGUMENT);
    CHECK(host.attach(window(2),
                      metrics(10, std::numeric_limits<uint32_t>::max())) ==
          MIGO_ERROR_INVALID_ARGUMENT);
    CHECK(fake_migo::calls().attach == calls_before_oversized_metrics);
    fake_migo::set_attach_result(MIGO_OK);
    CHECK(fake_migo::calls().attach == 0);
    CHECK(host.update(metrics()) == MIGO_ERROR_INVALID_STATE);
    CHECK(host.begin_detach() == MIGO_ERROR_INVALID_STATE);
}

void calls_from_a_foreign_thread_are_rejected_before_the_c_abi() {
    fake_migo::reset();
    SurfaceHost host(fake_migo::session());
    MigoResult attach_result = MIGO_OK;
    std::thread foreign([&] {
        attach_result = host.attach(window(2), metrics());
    });
    foreign.join();

    CHECK(attach_result == MIGO_ERROR_WRONG_THREAD);
    CHECK(fake_migo::calls().attach == 0);

    CHECK(host.attach(window(2), metrics()) == MIGO_OK);
    MigoResult update_result = MIGO_OK;
    MigoResult detach_result = MIGO_OK;
    MigoResult poll_result = MIGO_OK;
    bool released = true;
    std::thread foreign_attached([&] {
        update_result = host.update(metrics(800, 450));
        detach_result = host.begin_detach();
        poll_result = host.poll_release(&released);
    });
    foreign_attached.join();

    CHECK(update_result == MIGO_ERROR_WRONG_THREAD);
    CHECK(detach_result == MIGO_ERROR_WRONG_THREAD);
    CHECK(poll_result == MIGO_ERROR_WRONG_THREAD);
    CHECK(released);
    CHECK(fake_migo::calls().update == 0);
    CHECK(fake_migo::calls().begin_detach == 0);
    CHECK(fake_migo::calls().query == 0);
    CHECK(host.state() == SurfaceState::Attached);
    CHECK(host.begin_detach() == MIGO_OK);
    finish_release(host);
}

void null_success_handles_and_generation_exhaustion_fail_closed() {
    for (int index = 0; index < 4; ++index) expect_termination(index);

    fake_migo::reset();
    SurfaceHost zero(fake_migo::session(), 0);
    CHECK(zero.attach(window(2), metrics()) ==
          MIGO_ERROR_INVALID_STATE);
    CHECK(fake_migo::calls().attach == 0);

    SurfaceHost exhausted(fake_migo::session(), std::numeric_limits<uint64_t>::max());
    CHECK(exhausted.attach(window(2), metrics()) == MIGO_OK);
    CHECK(exhausted.generation() == std::numeric_limits<uint64_t>::max());
    CHECK(exhausted.begin_detach() == MIGO_OK);
    finish_release(exhausted);
    const int attach_calls = fake_migo::calls().attach;
    CHECK(exhausted.attach(window(3), metrics()) ==
          MIGO_ERROR_INVALID_STATE);
    CHECK(fake_migo::calls().attach == attach_calls);
}

void transient_update_and_query_failures_preserve_ownership() {
    fake_migo::reset();
    SurfaceHost host(fake_migo::session());
    CHECK(host.attach(window(2), metrics()) == MIGO_OK);

    fake_migo::set_update_result(MIGO_ERROR_WOULD_BLOCK);
    CHECK(host.update(metrics(800, 450)) == MIGO_ERROR_WOULD_BLOCK);
    CHECK(host.state() == SurfaceState::Attached);
    fake_migo::set_update_result(MIGO_OK);

    CHECK(host.begin_detach() == MIGO_OK);
    bool released = true;
    fake_migo::set_query_result(MIGO_ERROR_INTERNAL);
    CHECK(host.poll_release(&released) == MIGO_ERROR_INTERNAL);
    CHECK(!released);
    CHECK(host.state() == SurfaceState::Retiring);
    CHECK(fake_migo::calls().destroy_release == 0);
    fake_migo::set_query_result(MIGO_OK);
    finish_release(host);
}

void failures_preserve_owned_handles_and_generation() {
    fake_migo::reset();
    SurfaceHost host(fake_migo::session());
    fake_migo::set_attach_result(MIGO_ERROR_INTERNAL);
    CHECK(host.attach(window(2), metrics()) ==
          MIGO_ERROR_INTERNAL);
    CHECK(host.state() == SurfaceState::Detached);
    CHECK(host.generation() == 0);

    fake_migo::set_attach_result(MIGO_OK);
    CHECK(host.attach(window(2), metrics()) == MIGO_OK);
    CHECK(host.generation() == 1);
    fake_migo::set_begin_detach_result(MIGO_ERROR_INVALID_STATE);
    CHECK(host.begin_detach() == MIGO_ERROR_INVALID_STATE);
    CHECK(host.state() == SurfaceState::Attached);
    CHECK(host.update(metrics(20, 20)) == MIGO_OK);

    fake_migo::set_begin_detach_result(MIGO_OK);
    CHECK(host.begin_detach() == MIGO_OK);
    finish_release(host);

    CHECK(host.attach(window(3), metrics()) == MIGO_OK);
    CHECK(host.generation() == 2);
    CHECK(host.begin_detach() == MIGO_OK);
    finish_release(host);
}

void release_identity_and_destroy_fail_closed() {
    fake_migo::reset();
    SurfaceHost host(fake_migo::session());
    CHECK(host.attach(window(2), metrics()) == MIGO_OK);
    CHECK(host.begin_detach() == MIGO_OK);

    bool released = true;
    fake_migo::set_release_status(host.generation(), MIGO_SURFACE_RELEASE_RELEASED);
    fake_migo::set_release_status_metadata(0, MIGO_ABI_VERSION_CURRENT, 0);
    CHECK(host.poll_release(&released) == MIGO_ERROR_INTERNAL);
    CHECK(!released);
    CHECK(fake_migo::calls().destroy_release == 0);
    fake_migo::set_release_status_metadata(
        static_cast<uint32_t>(sizeof(MigoSurfaceReleaseStatus)),
        MIGO_ABI_VERSION_CURRENT + 1, 0);
    CHECK(host.poll_release(&released) == MIGO_ERROR_INTERNAL);
    CHECK(!released);
    CHECK(fake_migo::calls().destroy_release == 0);
    fake_migo::set_release_status_metadata(
        static_cast<uint32_t>(sizeof(MigoSurfaceReleaseStatus)),
        MIGO_ABI_VERSION_CURRENT, 1);
    CHECK(host.poll_release(&released) == MIGO_ERROR_INTERNAL);
    CHECK(!released);
    CHECK(fake_migo::calls().destroy_release == 0);
    fake_migo::set_release_status_metadata(
        static_cast<uint32_t>(sizeof(MigoSurfaceReleaseStatus)),
        MIGO_ABI_VERSION_CURRENT, 0);

    fake_migo::set_release_status(host.generation() + 1, MIGO_SURFACE_RELEASE_RELEASED);
    CHECK(host.poll_release(&released) == MIGO_ERROR_INTERNAL);
    CHECK(!released);
    CHECK(host.state() == SurfaceState::Retiring);
    CHECK(fake_migo::calls().destroy_release == 0);

    fake_migo::set_release_status(host.generation(), MIGO_SURFACE_RELEASE_RELEASED);
    fake_migo::set_destroy_result(MIGO_ERROR_INVALID_STATE);
    CHECK(host.poll_release(&released) == MIGO_ERROR_INVALID_STATE);
    CHECK(!released);
    CHECK(host.state() == SurfaceState::Retiring);

    fake_migo::set_destroy_result(MIGO_OK);
    CHECK(host.poll_release(&released) == MIGO_OK);
    CHECK(released);
    CHECK(host.state() == SurfaceState::Detached);
}

}  // namespace

int main(int argc, char **argv) {
    if (argc == 3 && std::string(argv[1]) == "--scenario") {
        std::set_terminate([] { std::_Exit(86); });
        const int index = std::atoi(argv[2]);
        if (index >= 0 && index < 4) kTerminating[index]();
        return 0;
    }
    win32_attach_update_and_release_are_exact();
    a_controller_adopting_a_session_starts_at_its_generation();
    invalid_inputs_do_not_enter_the_c_abi();
    calls_from_a_foreign_thread_are_rejected_before_the_c_abi();
    null_success_handles_and_generation_exhaustion_fail_closed();
    transient_update_and_query_failures_preserve_ownership();
    failures_preserve_owned_handles_and_generation();
    release_identity_and_destroy_fail_closed();
    if (g_failures != 0) {
        std::fprintf(stderr, "%d SurfaceHost assertion(s) failed\n", g_failures);
        return 1;
    }
    std::puts("SurfaceHost contract: PASS");
    return 0;
}
