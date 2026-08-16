# Homebrew formula for the Lumen toolchain. It lives in the tap
# lumen-fx/homebrew-lumen as Formula/lumen.rb, which is what
# `brew install lumen-fx/lumen/lumen` reads, and
# .github/workflows/publish-packages.yml pushes this file there on every
# release.
#
# The formula installs the release archive rather than building from source:
# every platform Lumen publishes for already has an archive on the release, and
# building the workspace needs a Rust toolchain the formula would otherwise
# have to pull in.
#
# The archive's files stay in one directory because lumenc dlopens the runtime
# library from beside its own executable, `lumenc package` looks for the
# launcher stub in that same directory (crates/lumenc/src/loader.rs,
# crates/lumenc/src/package_cli.rs), and the runtime library in turn finds the
# shared Rust standard library it needs beside itself. libexec takes the whole
# bin/ directory rather than a named list, and bin gets a
# script that execs the real lumenc, so the running executable is the one in
# libexec no matter how it was invoked. A plain symlink in bin would not do:
# macOS reports the path used to launch, not the resolved one, and the library
# would go missing.
#
# Nothing here installs a receipt under share/lumen. That file is what marks a
# copy as installed and turns the built-in update check on
# (crates/lumenc/src/update_check.rs); without it lumenc never checks for a
# newer release, which is what you want when brew owns the version.
#
# The Linux archives link the distribution's GTK 3, ALSA, and Wayland or X11
# libraries. They are not declared as formula dependencies because the
# binaries look for the system copies, not brewed ones.

class Lumen < Formula
  desc "Toolchain for Lumen, a markup-first UI framework for native desktop apps"
  homepage "https://lumenfx.dev"
  version "0.0.3"
  license "MPL-2.0"

  on_macos do
    on_arm do
      url "https://github.com/lumen-fx/lumen/releases/download/v0.0.3/lumen-macos-aarch64.tar.gz"
      sha256 "283d00342858d71d945c3338f4b906fdc1855cc34a5d8da8d8e5099784532bc7"
    end

    on_intel do
      url "https://github.com/lumen-fx/lumen/releases/download/v0.0.3/lumen-macos-x86_64.tar.gz"
      sha256 "facf294c5a4c1b41fc8d0836576cac8169396c4edeb5ace833d3ca53fcd8109b"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/lumen-fx/lumen/releases/download/v0.0.3/lumen-linux-aarch64.tar.gz"
      sha256 "3c0d8460af8272918fb3e619fc0fca8fe0a50f084ae43a3b0b767b817477bb8c"
    end

    on_intel do
      url "https://github.com/lumen-fx/lumen/releases/download/v0.0.3/lumen-linux-x86_64.tar.gz"
      sha256 "55a2e50220ecee29aa14b9f1469c3624aba820154b8615e89862260bfdb0a16b"
    end
  end

  livecheck do
    url :stable
    strategy :github_latest
  end

  def install
    libexec.install Dir["bin/*"]
    bin.write_exec_script libexec/"lumenc"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/lumenc --version")

    system bin/"lumenc", "new", "smoke"
    assert_predicate testpath/"smoke/main.lmn", :exist?
    assert_match "ok", shell_output("#{bin}/lumenc check smoke")
  end
end
