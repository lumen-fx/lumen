#!/bin/sh
# Lumen installer — https://github.com/lumen-ui/lumen
#
# Usage:
#   curl -fsSL https://lumen-ui.dev/install.sh | sh
#
# Downloads the prebuilt lumenc binary for this platform from the latest
# GitHub release and installs it to ~/.lumen/bin (no sudo required),
# then prints the PATH line to add. Set LUMEN_INSTALL_DIR to override.

set -eu

REPO="lumen-ui/lumen"
INSTALL_DIR="${LUMEN_INSTALL_DIR:-$HOME/.lumen/bin}"

say()  { printf '%s\n' "$*"; }
fail() { printf 'install.sh: %s\n' "$*" >&2; exit 1; }

# --- detect platform ---------------------------------------------------------
OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
  Linux)  os=linux ;;
  Darwin) os=macos ;;
  MINGW*|MSYS*|CYGWIN*) fail "On Windows, download lumenc.exe from https://github.com/$REPO/releases" ;;
  *) fail "unsupported OS: $OS" ;;
esac

case "$ARCH" in
  x86_64|amd64)  arch=x86_64 ;;
  aarch64|arm64) arch=aarch64 ;;
  *) fail "unsupported architecture: $ARCH" ;;
esac

ASSET="lumenc-${os}-${arch}"
URL="https://github.com/$REPO/releases/latest/download/$ASSET"

# --- download ----------------------------------------------------------------
say "Installing lumenc ($os-$arch) to $INSTALL_DIR"
mkdir -p "$INSTALL_DIR"

tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT

if command -v curl >/dev/null 2>&1; then
  curl -fSL --progress-bar -o "$tmp" "$URL" \
    || fail "download failed — no release asset for $os-$arch yet? See https://github.com/$REPO/releases"
elif command -v wget >/dev/null 2>&1; then
  wget -q --show-progress -O "$tmp" "$URL" \
    || fail "download failed — no release asset for $os-$arch yet? See https://github.com/$REPO/releases"
else
  fail "need curl or wget"
fi

install -m 755 "$tmp" "$INSTALL_DIR/lumenc"

# --- done --------------------------------------------------------------------
say ""
say "lumenc installed: $INSTALL_DIR/lumenc"
"$INSTALL_DIR/lumenc" --version 2>/dev/null || true

case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *)
    say ""
    say "Add it to your PATH (then restart your shell):"
    say "  echo 'export PATH=\"$INSTALL_DIR:\$PATH\"' >> ~/.profile"
    ;;
esac

say ""
say "Get started:"
say "  lumenc new counter my-app"
say "  lumenc run my-app"
