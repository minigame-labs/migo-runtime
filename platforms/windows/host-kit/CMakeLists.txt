cmake_minimum_required(VERSION 3.16)

if(CMAKE_SOURCE_DIR STREQUAL CMAKE_CURRENT_SOURCE_DIR)
    project(migo_windows_host_kit LANGUAGES CXX)
endif()

include(GNUInstallDirs)

if(NOT CMAKE_SYSTEM_NAME STREQUAL "Windows")
    message(FATAL_ERROR "The Migo Windows Host Kit can only be built for Windows")
endif()

if(CMAKE_SOURCE_DIR STREQUAL CMAKE_CURRENT_SOURCE_DIR)
    set(_migo_windows_host_kit_install_default ON)
else()
    set(_migo_windows_host_kit_install_default OFF)
endif()
option(MIGO_WINDOWS_HOST_KIT_ENABLE_INSTALL
       "Generate install rules for the Windows Host Kit"
       ${_migo_windows_host_kit_install_default})
unset(_migo_windows_host_kit_install_default)

if(NOT TARGET migo::migo)
    find_package(migo CONFIG REQUIRED)
endif()
find_package(Threads REQUIRED)

# The Surface lifecycle controller: the C ABI's attach, update and asynchronous
# release, owned on one thread. Its own target, which links nothing of the
# window system, as the Linux Host Kit's migo::linux-surface-host does.
add_library(migo_windows_surface_host STATIC
    include/migo/windows/surface_host.hpp
    src/surface_host.cpp
)
add_library(migo::windows-surface-host ALIAS migo_windows_surface_host)
set_target_properties(migo_windows_surface_host PROPERTIES
    EXPORT_NAME windows-surface-host
    POSITION_INDEPENDENT_CODE ON
)
target_compile_features(migo_windows_surface_host PUBLIC cxx_std_17)
target_include_directories(migo_windows_surface_host
    PUBLIC
        $<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/include>
        $<INSTALL_INTERFACE:${CMAKE_INSTALL_INCLUDEDIR}>
)
target_link_libraries(migo_windows_surface_host PUBLIC migo::migo Threads::Threads)

set(_migo_windows_host_kit_targets migo_windows_surface_host)

# The game's view: a child window the engine draws in, and the player's input.
add_library(migo_win32_surface_view STATIC
    include/migo/windows/win32_surface_view.hpp
    src/win32_surface_view.cpp
    # Internal, not installed: the DOM's names for the keys Windows reports.
    src/dom_keys.hpp
    src/dom_keys.cpp
)
add_library(migo::win32-surface-view ALIAS migo_win32_surface_view)
set_target_properties(migo_win32_surface_view PROPERTIES
    EXPORT_NAME win32-surface-view
    POSITION_INDEPENDENT_CODE ON
)
target_compile_features(migo_win32_surface_view PUBLIC cxx_std_17)
target_include_directories(migo_win32_surface_view
    PUBLIC
        $<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/include>
        $<INSTALL_INTERFACE:${CMAKE_INSTALL_INCLUDEDIR}>
)
target_link_libraries(migo_win32_surface_view
    PUBLIC migo::windows-surface-host
    PRIVATE user32 dwmapi imm32
)
list(APPEND _migo_windows_host_kit_targets migo_win32_surface_view)

# The host-service channel -- answers and events back to one Session -- and the
# JSON its requests and answers are made of.
add_library(migo_windows_host_services STATIC
    include/migo/windows/json.hpp
    include/migo/windows/host_services.hpp
    src/json.cpp
    src/host_services.cpp
)
add_library(migo::windows-host-services ALIAS migo_windows_host_services)
set_target_properties(migo_windows_host_services PROPERTIES
    EXPORT_NAME windows-host-services
    POSITION_INDEPENDENT_CODE ON
)
target_compile_features(migo_windows_host_services PUBLIC cxx_std_17)
target_include_directories(migo_windows_host_services
    PUBLIC
        $<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/include>
        $<INSTALL_INTERFACE:${CMAKE_INSTALL_INCLUDEDIR}>
)
target_link_libraries(migo_windows_host_services PUBLIC migo::migo)
list(APPEND _migo_windows_host_kit_targets migo_windows_host_services)

# The services Windows already offers an App, answered with its own: the
# clipboard, the window, the screen, the scopes a desktop decides.
add_library(migo_windows_builtin_host_services STATIC
    include/migo/windows/builtin_host_services.hpp
    src/builtin_host_services.cpp
    # Internal, not installed: toasts, loading, modals and action sheets; the
    # image service, its codec, its viewer and where its files wait.
    src/interaction.hpp
    src/interaction.cpp
    src/images.hpp
    src/images.cpp
    src/image_codec.hpp
    src/image_codec.cpp
    src/media_viewer.hpp
    src/media_viewer.cpp
    src/staging.hpp
    src/staging.cpp
    src/gui_queue.hpp
    src/gui_queue.cpp
    src/win32_text.hpp
    src/win32_text.cpp
    src/geolocation.hpp
    src/geolocation.cpp
)
add_library(migo::windows-builtin-host-services ALIAS migo_windows_builtin_host_services)
set_target_properties(migo_windows_builtin_host_services PROPERTIES
    EXPORT_NAME windows-builtin-host-services
    POSITION_INDEPENDENT_CODE ON
)
target_compile_features(migo_windows_builtin_host_services PUBLIC cxx_std_17)
# C++/WinRT (the location service) awaits through C++20's coroutines; the
# experimental ones it falls back to before C++20 are gone from current MSVC.
target_compile_features(migo_windows_builtin_host_services PRIVATE cxx_std_20)
target_include_directories(migo_windows_builtin_host_services
    PUBLIC
        $<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/include>
        $<INSTALL_INTERFACE:${CMAKE_INSTALL_INCLUDEDIR}>
)
target_link_libraries(migo_windows_builtin_host_services
    PUBLIC migo::win32-surface-view migo::windows-host-services
    PRIVATE user32 gdi32 comctl32 ole32 shell32 shlwapi uuid d2d1 dwrite windowscodecs
            mfplat mfreadwrite mfuuid winhttp windowsapp
)
list(APPEND _migo_windows_host_kit_targets migo_windows_builtin_host_services)

# The Managed shape: one Session owned with its callbacks, its view and its teardown.
add_library(migo_windows_managed_session STATIC
    include/migo/windows/managed_session.hpp
    src/managed_session.cpp
    # Internal, not installed: the panel the debugging aids show, the field
    # content's showKeyboard opens, and what Windows says about the machine.
    src/debug_panel.hpp
    src/debug_panel.cpp
    src/keyboard_field.hpp
    src/keyboard_field.cpp
    src/system_reports.hpp
    src/system_reports.cpp
)
add_library(migo::windows-managed-session ALIAS migo_windows_managed_session)
set_target_properties(migo_windows_managed_session PROPERTIES
    EXPORT_NAME windows-managed-session
    POSITION_INDEPENDENT_CODE ON
)
target_compile_features(migo_windows_managed_session PUBLIC cxx_std_17)
target_include_directories(migo_windows_managed_session
    PUBLIC
        $<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/include>
        $<INSTALL_INTERFACE:${CMAKE_INSTALL_INCLUDEDIR}>
)
target_link_libraries(migo_windows_managed_session
    PUBLIC migo::win32-surface-view migo::windows-host-services migo::windows-builtin-host-services
    PRIVATE user32 gdi32 comctl32 shell32 ole32 iphlpapi
)
list(APPEND _migo_windows_host_kit_targets migo_windows_managed_session)

if(MIGO_WINDOWS_HOST_KIT_ENABLE_INSTALL)
    set(_migo_windows_host_kit_cmake_dir
        "${CMAKE_INSTALL_LIBDIR}/cmake/migo-windows-host-kit")

    install(TARGETS ${_migo_windows_host_kit_targets}
        EXPORT migo-windows-host-kit-targets
        ARCHIVE DESTINATION "${CMAKE_INSTALL_LIBDIR}"
        LIBRARY DESTINATION "${CMAKE_INSTALL_LIBDIR}"
        RUNTIME DESTINATION "${CMAKE_INSTALL_BINDIR}"
    )
    install(FILES include/migo/windows/surface_host.hpp
                  include/migo/windows/win32_surface_view.hpp
                  include/migo/windows/json.hpp
                  include/migo/windows/host_services.hpp
                  include/migo/windows/builtin_host_services.hpp
                  include/migo/windows/managed_session.hpp
        DESTINATION "${CMAKE_INSTALL_INCLUDEDIR}/migo/windows")
    install(FILES README.md
        DESTINATION "${CMAKE_INSTALL_DATAROOTDIR}/doc/migo-windows-host-kit")
    install(EXPORT migo-windows-host-kit-targets
        FILE migo-windows-host-kit-targets.cmake
        NAMESPACE migo::
        DESTINATION "${_migo_windows_host_kit_cmake_dir}"
    )
    configure_file(
        cmake/migo-windows-host-kit-config.cmake.in
        "${CMAKE_CURRENT_BINARY_DIR}/migo-windows-host-kit-config.cmake"
        @ONLY
    )
    install(FILES
        "${CMAKE_CURRENT_BINARY_DIR}/migo-windows-host-kit-config.cmake"
        DESTINATION "${_migo_windows_host_kit_cmake_dir}"
    )
endif()
