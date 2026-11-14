cmake_minimum_required(VERSION 3.24)
project(migo_windows_host_kit_tests LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

if(NOT DEFINED MIGO_REPOSITORY_ROOT)
    get_filename_component(MIGO_REPOSITORY_ROOT
        "${CMAKE_CURRENT_LIST_DIR}/../../../.." ABSOLUTE)
endif()

get_filename_component(HOST_KIT_ROOT "${CMAKE_CURRENT_LIST_DIR}/.." ABSOLUTE)
set(MIGO_PUBLIC_INCLUDE "${MIGO_REPOSITORY_ROOT}/include")

# The strict fake of the public C ABI every Host Kit's tests link (tests/host_kit_fake).
set(MIGO_HOST_KIT_FAKE "${MIGO_REPOSITORY_ROOT}/tests/host_kit_fake")
add_library(migo_host_kit_fake_capi STATIC "${MIGO_HOST_KIT_FAKE}/fake_migo.cpp")
target_include_directories(migo_host_kit_fake_capi PUBLIC
    "${MIGO_PUBLIC_INCLUDE}"
    "${MIGO_HOST_KIT_FAKE}"
)

# Model a separately installed core SDK: the Host Kit sees only the public
# include contract; test executables link the fake explicitly to resolve migo_*.
add_library(migo::migo INTERFACE IMPORTED GLOBAL)
set_target_properties(migo::migo PROPERTIES
    INTERFACE_INCLUDE_DIRECTORIES "${MIGO_PUBLIC_INCLUDE}"
)

add_subdirectory("${HOST_KIT_ROOT}" host-kit)

add_executable(migo-windows-surface-host-test surface_host_test.cpp)
target_link_libraries(migo-windows-surface-host-test PRIVATE
    migo::windows-surface-host
    migo_host_kit_fake_capi
)

add_executable(migo-win32-surface-view-test win32_surface_view_test.cpp)
target_link_libraries(migo-win32-surface-view-test PRIVATE
    migo::win32-surface-view
    migo_host_kit_fake_capi
    user32
)

add_executable(migo-windows-json-test json_test.cpp)
target_link_libraries(migo-windows-json-test PRIVATE migo::windows-host-services migo_host_kit_fake_capi)

add_executable(migo-windows-managed-session-test managed_session_test.cpp)
target_link_libraries(migo-windows-managed-session-test PRIVATE
    migo::windows-managed-session
    migo_host_kit_fake_capi
    user32
)

add_executable(migo-windows-builtin-host-services-test builtin_host_services_test.cpp)
target_link_libraries(migo-windows-builtin-host-services-test PRIVATE
    migo::windows-managed-session
    migo_host_kit_fake_capi
    user32
    ole32
    windowscodecs
)

add_executable(migo-windows-recorder-test recorder_test.cpp)
target_link_libraries(migo-windows-recorder-test PRIVATE
    migo::windows-builtin-host-services
    migo_host_kit_fake_capi
    user32
    ole32
)

add_executable(migo-windows-gamepad-mapping-test gamepad_mapping_test.cpp)
target_link_libraries(migo-windows-gamepad-mapping-test PRIVATE migo::migo)

# The Host Kit lane's App (tests/win32_host), built here against the fake so a
# change to the Host Kit that breaks it fails here, not in the lane. Not run:
# it needs the engine.
add_executable(migo-win32-host-builds "${MIGO_REPOSITORY_ROOT}/tests/win32_host/main.cpp")
target_link_libraries(migo-win32-host-builds PRIVATE migo::windows-managed-session migo_host_kit_fake_capi user32 shell32 shlwapi)

foreach(target migo_host_kit_fake_capi migo-windows-surface-host-test migo_windows_surface_host
               migo-win32-surface-view-test migo_win32_surface_view
               migo-windows-json-test migo_windows_host_services
               migo-windows-managed-session-test migo_windows_managed_session
               migo-windows-builtin-host-services-test migo_windows_builtin_host_services
               migo-windows-gamepad-mapping-test migo-win32-host-builds migo-windows-recorder-test)
    # Every warning an error, in the compiler's own spelling.
    set_target_properties(${target} PROPERTIES COMPILE_WARNING_AS_ERROR ON)
    if(MSVC)
        target_compile_options(${target} PRIVATE /W4 /utf-8 /permissive-)
    else()
        target_compile_options(${target} PRIVATE -Wall -Wextra)
    endif()
endforeach()

enable_testing()
add_test(NAME migo-windows-surface-host COMMAND migo-windows-surface-host-test)
add_test(NAME migo-win32-surface-view COMMAND migo-win32-surface-view-test)
add_test(NAME migo-windows-json COMMAND migo-windows-json-test)
add_test(NAME migo-windows-managed-session COMMAND migo-windows-managed-session-test)
add_test(NAME migo-windows-builtin-host-services COMMAND migo-windows-builtin-host-services-test)
add_test(NAME migo-windows-gamepad-mapping COMMAND migo-windows-gamepad-mapping-test)
add_test(NAME migo-windows-recorder COMMAND migo-windows-recorder-test)
