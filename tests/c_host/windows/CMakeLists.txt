# The minimal Windows C ABI host (main.c), built as a third-party host builds
# against the Migo Windows SDK package: find_package(migo) + migo::migo.
#
#   cmake -S tests/c_host/windows -B build -G Ninja -DCMAKE_PREFIX_PATH=<package prefix>
#   cmake --build build
#
# migo-conformance's Windows lane (runners/run-windows-capi.sh) runs it.
cmake_minimum_required(VERSION 3.16)
project(migo_c_host_windows C)

find_package(migo REQUIRED)

add_executable(migo-c-host main.c)
target_link_libraries(migo-c-host PRIVATE migo::migo user32)
# wmain: the arguments arrive as UTF-16, converted to the ABI's UTF-8 here.
if(MSVC)
    target_compile_options(migo-c-host PRIVATE /W4 /WX /utf-8)
else()
    target_compile_options(migo-c-host PRIVATE -Wall -Wextra -Werror)
    target_link_options(migo-c-host PRIVATE -municode)
endif()
