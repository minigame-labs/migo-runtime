# Native bridge for the OpenHarmony host.
#
# This links migo's C ABI static library into the module's own .so, which is
# how an OpenHarmony native dependency is consumed -- the same arrangement the
# Android C SDK uses, and the reason neither ships a versioned shared object.

cmake_minimum_required(VERSION 3.5.0)
project(migo_ohos_host)

# Signed content is the production default. The only opt-in is wired by the
# explicit Debug build profile below; reject an accidental release/toolchain
# override instead of silently shipping an unsigned-content host.
option(MIGO_OHOS_ALLOW_UNSIGNED_CONTENT
    "Allow unsigned content for the explicitly selected Debug profile" OFF)
if(MIGO_OHOS_ALLOW_UNSIGNED_CONTENT AND NOT CMAKE_BUILD_TYPE STREQUAL "Debug")
    message(FATAL_ERROR
        "MIGO_OHOS_ALLOW_UNSIGNED_CONTENT is permitted only for CMAKE_BUILD_TYPE=Debug")
endif()

set(NATIVERENDER_ROOT_PATH ${CMAKE_CURRENT_SOURCE_DIR})

# Staged by scripts/build-ohos-host.sh next to this file, one directory per ABI.
# Not committed: it is a build output, exactly like the Android host's
# jniLibs-static.
set(MIGO_LIB "${NATIVERENDER_ROOT_PATH}/libs/${OHOS_ARCH}/libmigo_capi.a")
if(NOT EXISTS "${MIGO_LIB}")
    message(FATAL_ERROR
        "missing ${MIGO_LIB}\n"
        "build it first: scripts/build-ohos-host.sh --arch <arch>")
endif()

include_directories(
    ${NATIVERENDER_ROOT_PATH}
    ${NATIVERENDER_ROOT_PATH}/include
    ${NATIVERENDER_ROOT_PATH}/migo-include
)

add_library(migohost SHARED napi_init.cpp)

if(MIGO_OHOS_ALLOW_UNSIGNED_CONTENT)
    target_compile_definitions(migohost PRIVATE MIGO_OHOS_ALLOW_UNSIGNED_CONTENT=1)
endif()

# Two link options that are requirements, not tuning. Both were established by
# a failing link, and both fail in ways that name neither Skia nor this file.
#
# --gc-sections: skia-bindings carries a translation unit referencing
# JPEG/PDF/pathops symbols for features this Skia build disables. Without
# section garbage collection the link fails with about twenty undefined
# symbols.
#
# --allow-multiple-definition: Skia and V8 each embed parts of a C++ runtime, so
# std::runtime_error and std::logic_error are defined more than once. The linker takes the
# first definition -- and "because both were built against the same libc++ ABI", which this
# comment used to say, is false: on Android the same duplicates were measured as coming from
# two *different* libc++ implementations (Chromium's inside V8, and the NDK sysroot's), and
# OpenHarmony's arrangement is the same shape with the SDK's libc++ in that second role.
# It is safe for a narrower reason: no std exception object crosses the V8/Skia boundary,
# since each side throws and catches internally and the surface between them is a C ABI.
target_link_options(migohost PRIVATE
    -Wl,--gc-sections
    -Wl,--allow-multiple-definition
    # Without this --gc-sections cannot collect anything: in a shared library
    # every global symbol is an entry point, so skia-bindings' JPEG/PDF/pathops
    # wrappers stay live and their undefined references fail the link. The
    # module is reached through a constructor, not an exported symbol, so a
    # fully local map costs nothing.
    "-Wl,--version-script=${NATIVERENDER_ROOT_PATH}/migohost.map"
)

# libc++ is deliberately absent from this list: it arrives with the archive and
# with the platform, and naming it again is what the duplicate-symbol errors
# were. This mirrors the Android C SDK's -DANDROID_STL=c++_shared requirement.
target_link_libraries(migohost PUBLIC
    ${MIGO_LIB}
    libace_napi.z.so
    libace_ndk.z.so
    libnative_window.so
    libEGL.so
    libGLESv3.so
    libhilog_ndk.z.so
    dl
    m
    pthread
)
