cmake_minimum_required(VERSION 3.16)

if(CMAKE_SOURCE_DIR STREQUAL CMAKE_CURRENT_SOURCE_DIR)
    project(migo_linux_host_kit LANGUAGES CXX)
endif()

include(GNUInstallDirs)

if(NOT CMAKE_SYSTEM_NAME STREQUAL "Linux")
    message(FATAL_ERROR "The Migo Linux Host Kit can only be built for Linux")
endif()

option(MIGO_LINUX_HOST_KIT_BUILD_QT6_X11
       "Build the Qt 6 Widgets X11 surface adapter" ON)
if(CMAKE_SOURCE_DIR STREQUAL CMAKE_CURRENT_SOURCE_DIR)
    set(_migo_linux_host_kit_install_default ON)
else()
    set(_migo_linux_host_kit_install_default OFF)
endif()
option(MIGO_LINUX_HOST_KIT_ENABLE_INSTALL
       "Generate install rules for the Linux Host Kit"
       ${_migo_linux_host_kit_install_default})
unset(_migo_linux_host_kit_install_default)

if(NOT TARGET migo::migo)
    find_package(migo CONFIG REQUIRED)
endif()
find_package(Threads REQUIRED)

add_library(migo_linux_surface_host STATIC
    src/surface_host.cpp
)
add_library(migo::linux-surface-host ALIAS migo_linux_surface_host)
set_target_properties(migo_linux_surface_host PROPERTIES
    EXPORT_NAME linux-surface-host
    POSITION_INDEPENDENT_CODE ON
)
target_compile_features(migo_linux_surface_host PUBLIC cxx_std_17)
target_include_directories(migo_linux_surface_host
    PUBLIC
        $<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/include>
        $<INSTALL_INTERFACE:${CMAKE_INSTALL_INCLUDEDIR}>
)
target_link_libraries(migo_linux_surface_host PUBLIC migo::migo Threads::Threads)

set(_migo_linux_host_kit_targets migo_linux_surface_host)

if(MIGO_LINUX_HOST_KIT_BUILD_QT6_X11)
    find_package(Qt6 6.4 REQUIRED COMPONENTS Core Gui Widgets DBus)
    find_package(X11 REQUIRED)
    # XInput 2 on Qt's own xcb connection: the raw motion a pointer lock reads.
    find_package(PkgConfig REQUIRED)
    pkg_check_modules(MIGO_XCB_XINPUT REQUIRED IMPORTED_TARGET xcb xcb-xinput)

    add_library(migo_qt6_x11_surface_view STATIC
        include/migo/linux/qt6/x11_surface_view.hpp
        src/qt6/x11_surface_view.cpp
        # Internal, not installed: the DOM key/code tables and the raw-motion
        # reader are implementation details of this adapter, not part of the
        # Host Kit's public contract.
        src/qt6/dom_key_names.hpp
        src/qt6/dom_key_names.cpp
        src/qt6/xi2_raw_motion.hpp
        src/qt6/xi2_raw_motion.cpp
    )
    add_library(migo::qt6-x11-surface-view ALIAS migo_qt6_x11_surface_view)
    set_target_properties(migo_qt6_x11_surface_view PROPERTIES
        AUTOMOC ON
        EXPORT_NAME qt6-x11-surface-view
        POSITION_INDEPENDENT_CODE ON
    )
    target_compile_features(migo_qt6_x11_surface_view PUBLIC cxx_std_17)
    target_include_directories(migo_qt6_x11_surface_view
        PUBLIC
            $<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/include>
            $<INSTALL_INTERFACE:${CMAKE_INSTALL_INCLUDEDIR}>
    )
    target_link_libraries(migo_qt6_x11_surface_view
        PUBLIC
            migo::linux-surface-host
            Qt6::Widgets
        PRIVATE
            X11::X11
            PkgConfig::MIGO_XCB_XINPUT
    )
    list(APPEND _migo_linux_host_kit_targets migo_qt6_x11_surface_view)

    # The host-service channel and the services Qt answers itself. Its own
    # target: the Managed wrapper answers with it, and an App that owns its
    # Session (the Bound shape) can answer with it too without linking the
    # wrapper.
    add_library(migo_qt6_host_services STATIC
        include/migo/linux/qt6/host_services.hpp
        src/qt6/host_services.cpp
        # Internal, not installed: how what stands over the game is shown, and
        # the backlight (sysfs and logind, over Qt's D-Bus).
        src/qt6/overlay.hpp
        src/qt6/backlight.hpp
        src/qt6/backlight.cpp
    )
    add_library(migo::qt6-host-services ALIAS migo_qt6_host_services)
    set_target_properties(migo_qt6_host_services PROPERTIES
        AUTOMOC ON
        EXPORT_NAME qt6-host-services
        POSITION_INDEPENDENT_CODE ON
    )
    target_compile_features(migo_qt6_host_services PUBLIC cxx_std_17)
    target_include_directories(migo_qt6_host_services
        PUBLIC
            $<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/include>
            $<INSTALL_INTERFACE:${CMAKE_INSTALL_INCLUDEDIR}>
    )
    target_link_libraries(migo_qt6_host_services
        PUBLIC
            migo::qt6-x11-surface-view
            migo::migo
            Qt6::Widgets
        PRIVATE
            Qt6::DBus
    )
    list(APPEND _migo_linux_host_kit_targets migo_qt6_host_services)

    # The Managed wrapper is a separate target, not a mode of the view: an App
    # that owns its own Session (the Bound shape) must not be made to link a
    # type that creates and destroys Sessions.
    add_library(migo_qt6_managed_session STATIC
        include/migo/linux/qt6/managed_session.hpp
        src/qt6/managed_session.cpp
    )
    add_library(migo::qt6-managed-session ALIAS migo_qt6_managed_session)
    set_target_properties(migo_qt6_managed_session PROPERTIES
        AUTOMOC ON
        EXPORT_NAME qt6-managed-session
        POSITION_INDEPENDENT_CODE ON
    )
    target_compile_features(migo_qt6_managed_session PUBLIC cxx_std_17)
    target_include_directories(migo_qt6_managed_session
        PUBLIC
            $<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/include>
            $<INSTALL_INTERFACE:${CMAKE_INSTALL_INCLUDEDIR}>
    )
    target_link_libraries(migo_qt6_managed_session
        PUBLIC
            migo::qt6-x11-surface-view
            migo::qt6-host-services
            Qt6::Widgets
    )
    list(APPEND _migo_linux_host_kit_targets migo_qt6_managed_session)
endif()

if(MIGO_LINUX_HOST_KIT_ENABLE_INSTALL)
    set(_migo_linux_host_kit_cmake_dir
        "${CMAKE_INSTALL_LIBDIR}/cmake/migo-linux-host-kit")

    install(TARGETS ${_migo_linux_host_kit_targets}
        EXPORT migo-linux-host-kit-targets
        ARCHIVE DESTINATION "${CMAKE_INSTALL_LIBDIR}"
        LIBRARY DESTINATION "${CMAKE_INSTALL_LIBDIR}"
        RUNTIME DESTINATION "${CMAKE_INSTALL_BINDIR}"
    )
    install(FILES include/migo/linux/surface_host.hpp
        DESTINATION "${CMAKE_INSTALL_INCLUDEDIR}/migo/linux")
    if(MIGO_LINUX_HOST_KIT_BUILD_QT6_X11)
        install(FILES include/migo/linux/qt6/x11_surface_view.hpp
                      include/migo/linux/qt6/host_services.hpp
                      include/migo/linux/qt6/managed_session.hpp
            DESTINATION "${CMAKE_INSTALL_INCLUDEDIR}/migo/linux/qt6")
    endif()
    install(FILES README.md
        DESTINATION "${CMAKE_INSTALL_DATAROOTDIR}/doc/migo-linux-host-kit")
    install(EXPORT migo-linux-host-kit-targets
        FILE migo-linux-host-kit-targets.cmake
        NAMESPACE migo::
        DESTINATION "${_migo_linux_host_kit_cmake_dir}"
    )

    configure_file(
        cmake/migo-linux-host-kit-config.cmake.in
        "${CMAKE_CURRENT_BINARY_DIR}/migo-linux-host-kit-config.cmake"
        @ONLY
    )
    install(FILES
        "${CMAKE_CURRENT_BINARY_DIR}/migo-linux-host-kit-config.cmake"
        DESTINATION "${_migo_linux_host_kit_cmake_dir}"
    )
endif()
