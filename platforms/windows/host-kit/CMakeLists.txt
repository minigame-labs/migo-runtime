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
