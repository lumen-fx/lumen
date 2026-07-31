#!/usr/bin/env bash
# System libraries the workspace links against on Linux. The authoritative
# list lives in docs/src/getting-started/install.md; keep the two in sync.
#
#   pkg-config          resolves the rest
#   libgtk-3-dev        rfd's GTK3 file dialog (lumen-os-filedialog)
#   libasound2-dev      ALSA, via cpal under rodio (lumen-audio)
#   libxkbcommon-dev    keyboard handling under winit
#   libwayland-dev      wayland session support under winit
#   libvulkan1          Vulkan loader; wgpu builds the Vulkan backend on Linux
#   mesa-vulkan-drivers lavapipe software ICD, the only Vulkan device a runner
#                       has. Without it wgpu finds no adapter and the render
#                       and golden-image tests skip instead of running.
set -euxo pipefail

sudo apt-get update
sudo apt-get install -y --no-install-recommends \
  pkg-config \
  libgtk-3-dev \
  libasound2-dev \
  libxkbcommon-dev \
  libwayland-dev \
  libvulkan1 \
  mesa-vulkan-drivers
