# A third-party integrator's view of migo: find_package, link one target.
#
# Deliberately standalone. This file is never pulled in with add_subdirectory
# from the repo, because that would let it see build state a real consumer does
# not have -- and the point of this example is to fail if the package is
# incomplete.
#
# Configure it against a staged package:
#   cmake -S tests/c_host/linux -B <build> -DCMAKE_PREFIX_PATH=<prefix>
cmake_minimum_required(VERSION 3.16)
project(migo_c_host C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)

find_package(migo REQUIRED)

# The example owns its window, so it -- not the engine -- links Xlib.
find_package(X11 REQUIRED)

add_executable(c-host-cmake main.c)
target_link_libraries(c-host-cmake PRIVATE migo::migo ${X11_LIBRARIES})
target_compile_options(c-host-cmake PRIVATE -Wall -Wextra)

# X11 only -- see build-with-pkgconfig.sh for why the Wayland host is not part of
# what a packaged-SDK consumer builds.
target_compile_definitions(c-host-cmake PRIVATE MIGO_C_HOST_NO_WAYLAND)
