# Lumen C++ SDK - header-only INTERFACE library `lumen::cpp`.
#
# Consume from a sibling build with:
#     add_subdirectory(path/to/sdks/cpp lumen_cpp)
#     target_link_libraries(myapp PRIVATE lumen::cpp)
#
# Or, once installed, from an outside project with:
#     find_package(lumen-cpp CONFIG REQUIRED)   # CMake package, or
#     pkg_check_modules(LUMEN REQUIRED lumen)    # via lumen.pc
#     target_link_libraries(myapp PRIVATE lumen::cpp)
#
# The target carries the include paths for both this SDK's <lumen.hpp>
# and the C ABI header <lumen.h> it wraps. Linking the actual Lumen
# library (liblumen_ffi) is handled two ways:
#   * if pkg-config can resolve `lumen` (installed lumen.pc), the link
#     flags are attached to the interface automatically;
#   * otherwise the consumer links the cargo-built staticlib/cdylib
#     directly (see apps/sysmon for the in-tree pattern).

cmake_minimum_required(VERSION 3.20)
project(lumen_cpp LANGUAGES CXX VERSION 0.4.0)

# Path to the committed C ABI header (<lumen.h> + lumen_simple.h) that
# the C++ SDK wraps. Overridable by a superproject if the layout moves.
set(LUMEN_C_HEADER_DIR "${CMAKE_CURRENT_SOURCE_DIR}/../../lumen/ffi/include"
    CACHE PATH "Directory containing the Lumen C ABI header lumen.h")

add_library(lumen_cpp INTERFACE)
add_library(lumen::cpp ALIAS lumen_cpp)

target_compile_features(lumen_cpp INTERFACE cxx_std_17)

target_include_directories(lumen_cpp INTERFACE
    $<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/include>
    $<BUILD_INTERFACE:${LUMEN_C_HEADER_DIR}>
    $<INSTALL_INTERFACE:include>)

# -- Optional: resolve the Lumen C library via pkg-config (lumen.pc) --
# Best-effort. When the library is installed and discoverable, its link
# flags ride along on the interface target. When it is not (typical for
# an in-tree cargo build), we stay silent and let the consumer link the
# cargo artifact themselves.
find_package(PkgConfig QUIET)
if(PkgConfig_FOUND)
    pkg_check_modules(LUMEN_FFI QUIET IMPORTED_TARGET lumen)
    if(LUMEN_FFI_FOUND)
        target_link_libraries(lumen_cpp INTERFACE PkgConfig::LUMEN_FFI)
        message(STATUS "lumen::cpp: linking C library via pkg-config (lumen ${LUMEN_FFI_VERSION})")
    else()
        message(STATUS "lumen::cpp: pkg-config could not resolve 'lumen'; "
                       "consumer must link liblumen_ffi directly")
    endif()
endif()

# -- Install rules ---------------------------------------------------
include(GNUInstallDirs)
include(CMakePackageConfigHelpers)

# Header-only: install the SDK header. (The C ABI header lumen.h is
# shipped by the lumen-ffi install step / distro package.)
install(FILES ${CMAKE_CURRENT_SOURCE_DIR}/include/lumen.hpp
        DESTINATION ${CMAKE_INSTALL_INCLUDEDIR})

# Export the INTERFACE target so downstreams can find_package it.
install(TARGETS lumen_cpp EXPORT lumen-cpp-targets)
install(EXPORT lumen-cpp-targets
        FILE lumen-cpp-targets.cmake
        NAMESPACE lumen::
        DESTINATION ${CMAKE_INSTALL_LIBDIR}/cmake/lumen-cpp)

# Generate a package config so `find_package(lumen-cpp CONFIG)` works.
file(WRITE "${CMAKE_CURRENT_BINARY_DIR}/lumen-cpp-config.cmake.in"
[=[
@PACKAGE_INIT@
include(CMakeFindDependencyMacro)
find_dependency(PkgConfig QUIET)
include("${CMAKE_CURRENT_LIST_DIR}/lumen-cpp-targets.cmake")
]=])

configure_package_config_file(
    "${CMAKE_CURRENT_BINARY_DIR}/lumen-cpp-config.cmake.in"
    "${CMAKE_CURRENT_BINARY_DIR}/lumen-cpp-config.cmake"
    INSTALL_DESTINATION ${CMAKE_INSTALL_LIBDIR}/cmake/lumen-cpp)

write_basic_package_version_file(
    "${CMAKE_CURRENT_BINARY_DIR}/lumen-cpp-config-version.cmake"
    VERSION ${PROJECT_VERSION}
    COMPATIBILITY SameMinorVersion)

install(FILES
        "${CMAKE_CURRENT_BINARY_DIR}/lumen-cpp-config.cmake"
        "${CMAKE_CURRENT_BINARY_DIR}/lumen-cpp-config-version.cmake"
        DESTINATION ${CMAKE_INSTALL_LIBDIR}/cmake/lumen-cpp)
