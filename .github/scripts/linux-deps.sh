#!/usr/bin/env bash
# System libraries the workspace links against on Linux. The authoritative
# list lives in docs/src/contributing/building-lumen.md; keep the two in sync.
#
#   pkg-config          resolves the rest
#   libgtk-3-dev        rfd's GTK3 file dialog (lumen-os-filedialog)
#   libasound2-dev      ALSA, via cpal under rodio (lumen-audio)
#   libxkbcommon-dev    keyboard handling under winit
#   libwayland-dev      wayland session support under winit
#   libvulkan1          Vulkan loader; wgpu builds the Vulkan backend on Linux
#   libvulkan-dev       Vulkan headers and link library, for the C reference
#                       producer the lumen-surface tests compile
#   mesa-vulkan-drivers lavapipe software ICD, the only Vulkan device a runner
#                       has. Without it wgpu finds no adapter, the GPU render
#                       tests skip, and the golden images compare on the CPU
#                       backend alone.
#   xvfb and the X11    a private X server, and the client libraries winit
#   client libraries    loads at run time, for the test that drags across
#                       real windows (public/lumenc/tests/dock_x11.rs). The
#                       test starts its own server; without one it skips.
set -euxo pipefail

sudo apt-get update
sudo apt-get install -y --no-install-recommends \
  pkg-config \
  libgtk-3-dev \
  libasound2-dev \
  libxkbcommon-dev \
  libwayland-dev \
  libvulkan1 \
  libvulkan-dev \
  mesa-vulkan-drivers \
  xvfb \
  libxkbcommon-x11-0 \
  libx11-xcb1 \
  libxcursor1 \
  libxrandr2 \
  libxi6
