cmake_minimum_required(VERSION 3.16)
project(migo_linux_host_kit_install_consumer LANGUAGES CXX)

if(NOT DEFINED MIGO_PUBLIC_INCLUDE)
    message(FATAL_ERROR "MIGO_PUBLIC_INCLUDE must point at the core SDK's public headers")
endif()

# Model the real core SDK as a separately built linkable target. Linking an
# executable (rather than another archive) proves that every dependency of the
# installed static Host Kit closes through its exported CMake targets.
add_library(migo_install_consumer_fake_capi STATIC ../fake_migo.cpp)
target_include_directories(migo_install_consumer_fake_capi PUBLIC
    "${MIGO_PUBLIC_INCLUDE}"
    "${CMAKE_CURRENT_LIST_DIR}/.."
)
add_library(migo::migo ALIAS migo_install_consumer_fake_capi)

find_package(migo-linux-host-kit CONFIG REQUIRED COMPONENTS qt6-media)

add_executable(migo-host-kit-install-consumer consumer.cpp)
set_target_properties(migo-host-kit-install-consumer PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED ON
    CXX_EXTENSIONS ON
)
target_link_libraries(migo-host-kit-install-consumer PRIVATE
    migo::qt6-x11-surface-view
    migo::qt6-managed-session
    migo::qt6-media
)
target_compile_definitions(migo-host-kit-install-consumer PRIVATE
    QT_NO_KEYWORDS
)
target_compile_options(migo-host-kit-install-consumer PRIVATE
    -Wall -Wextra -Werror
)
target_compile_options(migo_install_consumer_fake_capi PRIVATE
    -Wall -Wextra -Werror
)
