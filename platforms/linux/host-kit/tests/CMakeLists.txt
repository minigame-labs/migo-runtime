cmake_minimum_required(VERSION 3.16)
project(migo_linux_host_kit_tests LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

if(NOT DEFINED MIGO_REPOSITORY_ROOT)
    get_filename_component(MIGO_REPOSITORY_ROOT
        "${CMAKE_CURRENT_LIST_DIR}/../../../.." ABSOLUTE)
endif()

get_filename_component(HOST_KIT_ROOT "${CMAKE_CURRENT_LIST_DIR}/.." ABSOLUTE)
set(MIGO_PUBLIC_INCLUDE "${MIGO_REPOSITORY_ROOT}/include")

add_library(migo_host_kit_fake_capi STATIC fake_migo.cpp)
target_include_directories(migo_host_kit_fake_capi PUBLIC
    "${MIGO_PUBLIC_INCLUDE}"
    "${CMAKE_CURRENT_LIST_DIR}"
)

# Model a separately installed core SDK: the Host Kit must see only the public
# include contract and must not accidentally inherit the test fake as a build
# dependency. Test executables link the fake explicitly to resolve migo_*.
add_library(migo::migo INTERFACE IMPORTED GLOBAL)
set_target_properties(migo::migo PROPERTIES
    INTERFACE_INCLUDE_DIRECTORIES "${MIGO_PUBLIC_INCLUDE}"
)

set(MIGO_LINUX_HOST_KIT_BUILD_QT6_X11 ON CACHE BOOL "" FORCE)
if(NOT DEFINED MIGO_LINUX_HOST_KIT_ENABLE_INSTALL)
    set(MIGO_LINUX_HOST_KIT_ENABLE_INSTALL OFF CACHE BOOL "")
endif()
add_subdirectory("${HOST_KIT_ROOT}" "${CMAKE_CURRENT_BINARY_DIR}/host-kit")

find_package(Qt6 6.4 REQUIRED COMPONENTS Gui Test)
find_package(X11 REQUIRED)
if(NOT TARGET X11::Xtst)
    message(FATAL_ERROR "the xcb input tests drive the X server through XTest (libxtst-dev)")
endif()

# The player's hands on the X server, through XTest: its own unit, because
# Xlib's macros break Qt's names wherever both are seen.
add_library(migo_host_kit_x11_player STATIC x11_player.cpp)
target_link_libraries(migo_host_kit_x11_player PUBLIC Qt6::Gui PRIVATE X11::X11 X11::Xtst)

add_executable(migo-surface-host-test surface_host_test.cpp)
target_link_libraries(migo-surface-host-test PRIVATE
    migo::linux-surface-host
    migo_host_kit_fake_capi
)

add_executable(migo-qt-x11-view-test qt_x11_surface_view_test.cpp)
set_target_properties(migo-qt-x11-view-test PROPERTIES AUTOMOC ON)
target_link_libraries(migo-qt-x11-view-test PRIVATE
    migo::qt6-x11-surface-view
    migo_host_kit_fake_capi
    Qt6::Test
)

add_executable(migo-qt-x11-input-test qt_x11_input_test.cpp)
set_target_properties(migo-qt-x11-input-test PROPERTIES AUTOMOC ON)
# The raw-motion reader is internal to the adapter; this test reads it directly.
target_include_directories(migo-qt-x11-input-test PRIVATE "${HOST_KIT_ROOT}/src/qt6")
target_link_libraries(migo-qt-x11-input-test PRIVATE
    migo::qt6-x11-surface-view
    migo_host_kit_fake_capi
    migo_host_kit_x11_player
    Qt6::Test
)

add_executable(migo-qt-host-services-test qt_host_services_test.cpp)
set_target_properties(migo-qt-host-services-test PROPERTIES AUTOMOC ON)
target_link_libraries(migo-qt-host-services-test PRIVATE
    migo::qt6-managed-session
    migo_host_kit_fake_capi
    Qt6::Test
)

add_executable(migo-qt-managed-session-test qt_managed_session_test.cpp)
set_target_properties(migo-qt-managed-session-test PROPERTIES AUTOMOC ON)
target_link_libraries(migo-qt-managed-session-test PRIVATE
    migo::qt6-managed-session
    migo_host_kit_fake_capi
    Qt6::Test
)

add_library(migo-gnu-extension-consumer-test STATIC
    gnu_extension_consumer_test.cpp
)
set_target_properties(migo-gnu-extension-consumer-test PROPERTIES
    CXX_EXTENSIONS ON
)
target_link_libraries(migo-gnu-extension-consumer-test PRIVATE
    migo::qt6-x11-surface-view
)
target_compile_definitions(migo-gnu-extension-consumer-test PRIVATE
    QT_NO_KEYWORDS
)

foreach(target
        migo_host_kit_fake_capi
        migo_host_kit_x11_player
        migo-surface-host-test
        migo-qt-x11-view-test
        migo-qt-x11-input-test
        migo-qt-managed-session-test
        migo-gnu-extension-consumer-test)
    target_compile_options(${target} PRIVATE -Wall -Wextra -Werror)
endforeach()

target_compile_options(migo_linux_surface_host PRIVATE -Wall -Wextra -Werror)
target_compile_options(migo_qt6_x11_surface_view PRIVATE -Wall -Wextra -Werror)
target_compile_options(migo_qt6_managed_session PRIVATE -Wall -Wextra -Werror)

enable_testing()
add_test(NAME migo-surface-host COMMAND migo-surface-host-test)
add_test(NAME migo-qt-x11-offscreen COMMAND migo-qt-x11-view-test)
set_tests_properties(migo-qt-x11-offscreen PROPERTIES
    ENVIRONMENT "QT_QPA_PLATFORM=offscreen")
add_test(NAME migo-qt-x11-input-offscreen COMMAND migo-qt-x11-input-test)
set_tests_properties(migo-qt-x11-input-offscreen PROPERTIES
    ENVIRONMENT "QT_QPA_PLATFORM=offscreen")
add_test(NAME migo-qt-managed-session-offscreen COMMAND migo-qt-managed-session-test)
set_tests_properties(migo-qt-managed-session-offscreen PROPERTIES
    ENVIRONMENT "QT_QPA_PLATFORM=offscreen")
