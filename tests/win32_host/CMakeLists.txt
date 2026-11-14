# A Windows App on the Windows Host Kit's Managed shape (main.cpp), built as an
# App builds: against the Migo Windows SDK package (find_package(migo)) and the
# Host Kit from this checkout.
#
#   cmake -S tests/win32_host -B build -G Ninja -DCMAKE_PREFIX_PATH=<package prefix>
#   cmake --build build
#
# migo-conformance's Windows Host Kit lane (runners/run-windows-hostkit.sh) runs it.
cmake_minimum_required(VERSION 3.24)
project(migo_win32_host LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

find_package(migo REQUIRED)
add_subdirectory("${CMAKE_CURRENT_LIST_DIR}/../../platforms/windows/host-kit" host-kit)

add_executable(migo-win32-host main.cpp)
target_link_libraries(migo-win32-host PRIVATE migo::windows-managed-session user32 shell32 shlwapi)
# Every warning an error, in the compiler's own spelling.
set_target_properties(migo-win32-host PROPERTIES COMPILE_WARNING_AS_ERROR ON)
if(MSVC)
    target_compile_options(migo-win32-host PRIVATE /W4 /utf-8 /permissive-)
else()
    target_compile_options(migo-win32-host PRIVATE -Wall -Wextra)
    target_link_options(migo-win32-host PRIVATE -municode)
endif()
