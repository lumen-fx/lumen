# Lumen C++ SDK - examples.
#
# A set of small, self-contained programs that each compile + link against
# the built `lumen-ffi` cdylib and run headless. Build them all with:
#
#     cmake -B build
#     cmake --build build            # builds the cdylib (if stale) + all examples
#
# then run e.g. `./build/counter`. See README.md in this directory.
#
# CMake resolves the workspace cargo target dir via `cargo metadata` (so a
# custom CARGO_TARGET_DIR is honoured) and drives `cargo build -p lumen-ffi`
# through a custom target - mirroring apps/sysmon. Headers come from the
# header-only interface target `lumen::cpp` (../, i.e. sdk/cpp).

cmake_minimum_required(VERSION 3.20)
project(lumen_cpp_examples LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

set(LUMEN_REPO "${CMAKE_CURRENT_SOURCE_DIR}/../../..")

# Resolve the workspace's cargo target directory (honours CARGO_TARGET_DIR).
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

set(LUMEN_LIB "${LUMEN_CARGO_TARGET_DIR}/${_LUMEN_PROFILE_DIR}/liblumen_ffi.so")

add_custom_command(
    OUTPUT  ${LUMEN_LIB}
    COMMAND cargo build -p lumen-ffi ${_LUMEN_CARGO_FLAGS}
    WORKING_DIRECTORY ${LUMEN_REPO}
    VERBATIM
    USES_TERMINAL)
add_custom_target(lumen_ffi DEPENDS ${LUMEN_LIB})

# Header-only SDK interface target (lumen::cpp): <lumen.hpp> + <lumen.h>.
add_subdirectory(${LUMEN_REPO}/sdk/cpp ${CMAKE_BINARY_DIR}/lumen_cpp)

# Each example: name -> (source, default app dir baked in as LUMEN_APP_DIR).
function(lumen_example name src appdir)
    add_executable(${name} ${src})
    add_dependencies(${name} lumen_ffi)
    target_link_libraries(${name} PRIVATE lumen::cpp ${LUMEN_LIB} pthread)
    target_compile_definitions(${name} PRIVATE LUMEN_APP_DIR="${appdir}")
    # Bake the cargo target dir as RPATH so the binary finds liblumen_ffi.so
    # without LD_LIBRARY_PATH.
    set_target_properties(${name} PROPERTIES
        BUILD_RPATH   "${LUMEN_CARGO_TARGET_DIR}/${_LUMEN_PROFILE_DIR}"
        INSTALL_RPATH "${LUMEN_CARGO_TARGET_DIR}/${_LUMEN_PROFILE_DIR}")
endfunction()

set(_APPS "${CMAKE_CURRENT_SOURCE_DIR}/apps")

lumen_example(hello    hello.cpp    "${_APPS}/hello")
lumen_example(counter  counter.cpp  "${_APPS}/counter")
lumen_example(signals  signals.cpp  "${_APPS}/counter")
lumen_example(handlers handlers.cpp "${_APPS}/counter")
lumen_example(expose   expose.cpp   "${_APPS}/expose")
lumen_example(list     list.cpp     "${_APPS}/list")
lumen_example(dom      dom.cpp      "${_APPS}/counter")

# `cmake --build build --target examples` builds them all.
add_custom_target(examples DEPENDS hello counter signals handlers expose list dom)
