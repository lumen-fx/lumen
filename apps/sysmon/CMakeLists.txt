# Lumen sysmon - C++ demo of the C ABI.
#
# Build:
#   cmake -B build
#   cmake --build build
#   ./build/sysmon
#
# CMake drives `cargo build -p lumen`; the resulting cdylib lives
# under the workspace's cargo target dir (queried via `cargo metadata`
# so a custom `CARGO_TARGET_DIR` works). Headers come from the C++ SDK
# interface target `lumen::cpp` (sdk/cpp), which carries both
# <lumen.hpp> and the C ABI <lumen.h> include paths.

cmake_minimum_required(VERSION 3.20)
project(sysmon LANGUAGES CXX)
set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

set(LUMEN_REPO "${CMAKE_CURRENT_SOURCE_DIR}/../..")

# Resolve the workspace's cargo target directory so a custom
# CARGO_TARGET_DIR (env or `~/.cargo/config.toml`) is honoured.
execute_process(
    COMMAND cargo metadata --format-version 1 --no-deps --manifest-path ${LUMEN_REPO}/Cargo.toml
    OUTPUT_VARIABLE _LUMEN_CARGO_META
    OUTPUT_STRIP_TRAILING_WHITESPACE
    COMMAND_ERROR_IS_FATAL ANY)
string(JSON LUMEN_CARGO_TARGET_DIR GET "${_LUMEN_CARGO_META}" target_directory)

option(LUMEN_RELEASE "Build the Lumen cdylib with --release" OFF)
if(LUMEN_RELEASE)
    set(_LUMEN_PROFILE_DIR "release")
    set(_LUMEN_CARGO_FLAGS "--release")
else()
    set(_LUMEN_PROFILE_DIR "debug")
    set(_LUMEN_CARGO_FLAGS "")
endif()

set(LUMEN_LIB "${LUMEN_CARGO_TARGET_DIR}/${_LUMEN_PROFILE_DIR}/liblumen.so")

add_custom_command(
    OUTPUT  ${LUMEN_LIB}
    COMMAND cargo build -p lumen ${_LUMEN_CARGO_FLAGS}
    WORKING_DIRECTORY ${LUMEN_REPO}
    VERBATIM
    USES_TERMINAL)
add_custom_target(lumen DEPENDS ${LUMEN_LIB})

# Pull in the header-only C++ SDK (provides the lumen::cpp interface
# target with the <lumen.hpp> + <lumen.h> include paths and C++17).
add_subdirectory(${LUMEN_REPO}/sdk/cpp ${CMAKE_BINARY_DIR}/lumen_cpp)

add_executable(sysmon main.cpp)
add_dependencies(sysmon lumen)
target_link_libraries(sysmon PRIVATE lumen::cpp ${LUMEN_LIB} pthread stdc++fs)
# Bake the source dir as a default app dir so `./build/sysmon` finds
# main.lmn without an argv. Override via argv[1].
target_compile_definitions(sysmon PRIVATE
    LUMEN_APP_DIR="${CMAKE_CURRENT_SOURCE_DIR}")
# Embed the cargo target dir as RPATH so the binary finds liblumen.so
# without LD_LIBRARY_PATH gymnastics.
set_target_properties(sysmon PROPERTIES
    BUILD_RPATH "${LUMEN_CARGO_TARGET_DIR}/${_LUMEN_PROFILE_DIR}"
    INSTALL_RPATH "${LUMEN_CARGO_TARGET_DIR}/${_LUMEN_PROFILE_DIR}")
