cmake_minimum_required(VERSION 3.22.1)
# C CXX, not C alone: the host source is C, but it links Skia and V8, which are
# C++. With a C-only project the NDK toolchain does not link the C++ runtime and
# the link fails on several hundred std::__ndk1 symbols.
project(migo_c_host C CXX)

# cpp -> main -> src -> android -> c-host -> examples -> repo root
get_filename_component(MIGO_ROOT ${CMAKE_CURRENT_SOURCE_DIR}/../../../../../.. ABSOLUTE)

# The NDK ships the NativeActivity glue; use it rather than hand-rolling an
# event loop and a looper.
add_library(native_app_glue STATIC
    ${ANDROID_NDK}/sources/android/native_app_glue/android_native_app_glue.c)
target_include_directories(native_app_glue PUBLIC
    ${ANDROID_NDK}/sources/android/native_app_glue)

add_library(migo_c_host SHARED main.c api_level_compat.c)

# The engine comes from one of two places.
#
# A published package (MIGO_PACKAGE_PREFIX, set by
# scripts/build-android-c-host.sh --package): found the way a third-party host
# finds it, and its headers are the ones compiled against -- a released library
# paired with this tree's headers would be a host no one ships.
#
# Otherwise this tree: the staticlib the build script staged, and the public
# headers. The C side sees those headers and nothing else. If this example ever
# needs something outside include/migo, the ABI is incomplete -- that feedback
# is why the example exists.
if(MIGO_PACKAGE_PREFIX)
    find_package(migo REQUIRED)
    set(MIGO_ENGINE migo::migo)
else()
    add_library(migo_capi STATIC IMPORTED)
    set_target_properties(migo_capi PROPERTIES IMPORTED_LOCATION
        ${CMAKE_CURRENT_SOURCE_DIR}/../jniLibs-static/${ANDROID_ABI}/libmigo_capi.a)
    target_include_directories(migo_c_host PRIVATE ${MIGO_ROOT}/include)
    set(MIGO_ENGINE migo_capi)
endif()

# -u ANativeActivity_onCreate: the glue's entry point is referenced only by the
# Android framework, so without a root the linker collects it and the app dies
# with "Unable to find native library entry point".
#
# --gc-sections is load-bearing, not an optimisation: skia-bindings compiles one
# translation unit holding JPEG/PDF/pathops wrappers while Skia is built without
# those features, so that object references symbols that do not exist. An
# executable survives because the unreferenced wrappers are collected before
# their references have to resolve; a shared library keeps every global unless
# told otherwise.
# No --exclude-libs,ALL here. That flag belongs on a library with an ABI to
# pin, which is what the Linux SDK does; on an application it hides
# ANativeActivity_onCreate, which comes from the *static* native_app_glue, and
# NativeActivity then dies with "Unable to load native library entry point"
# before any of this code runs.
target_link_options(migo_c_host PRIVATE
    -u ANativeActivity_onCreate
    -Wl,--version-script=${CMAKE_CURRENT_SOURCE_DIR}/migo_c_host.map
    -Wl,--gc-sections)

target_link_libraries(migo_c_host
    native_app_glue
    ${MIGO_ENGINE}
    android log EGL GLESv3 OpenSLES)

target_link_libraries(migo_c_host c++_shared)

# android_get_device_api_level comes from api_level_compat.c; see the comment
# there for why linking the static libc.a instead crashes before android_main.
