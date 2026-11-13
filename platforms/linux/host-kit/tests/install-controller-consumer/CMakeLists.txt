cmake_minimum_required(VERSION 3.16)
project(migo_linux_surface_host_install_consumer LANGUAGES CXX)

if(NOT DEFINED MIGO_PUBLIC_INCLUDE)
    message(FATAL_ERROR "MIGO_PUBLIC_INCLUDE must point at the core SDK's public headers")
endif()

# The strict fake of the public C ABI, beside the public headers in the repository.
get_filename_component(MIGO_HOST_KIT_FAKE "${MIGO_PUBLIC_INCLUDE}/../tests/host_kit_fake" ABSOLUTE)
add_library(migo_install_consumer_fake_capi STATIC "${MIGO_HOST_KIT_FAKE}/fake_migo.cpp")
target_include_directories(migo_install_consumer_fake_capi PUBLIC
    "${MIGO_PUBLIC_INCLUDE}"
    "${MIGO_HOST_KIT_FAKE}"
)
add_library(migo::migo ALIAS migo_install_consumer_fake_capi)

find_package(migo-linux-host-kit CONFIG REQUIRED)
if(TARGET Qt6::Core OR TARGET migo::qt6-x11-surface-view)
    message(FATAL_ERROR "installed controller-only package unexpectedly discovered Qt")
endif()

add_executable(migo-linux-surface-host-install-consumer consumer.cpp)
set_target_properties(migo-linux-surface-host-install-consumer PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED ON
    CXX_EXTENSIONS ON
)
target_link_libraries(migo-linux-surface-host-install-consumer PRIVATE
    migo::linux-surface-host
)
target_compile_options(migo-linux-surface-host-install-consumer PRIVATE
    -Wall -Wextra -Werror
)
target_compile_options(migo_install_consumer_fake_capi PRIVATE
    -Wall -Wextra -Werror
)
