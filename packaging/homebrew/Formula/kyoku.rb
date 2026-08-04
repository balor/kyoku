# Homebrew binary formula for kyoku (tap: balor/kyoku, repo: balor/homebrew-kyoku).
# Installs the prebuilt tarballs produced by .github/workflows/release.yml:
#   kyoku-<version>-<target>.tar.gz  (tarball root dir contains just `kyoku`)
#
# The sha256 placeholders MUST be filled after each release is published:
#   ./update-formula.sh <version>
# (downloads the assets from GitHub Releases, hashes them, rewrites this file)
class Kyoku < Formula
  desc "TUI-first music library manager"
  homepage "https://github.com/balor/kyoku"
  version "0.2.2"
  license "MIT"

  # Binaries are not Apple-notarized / code-signed, but files downloaded by
  # Homebrew itself are never quarantined, so installs work without the
  # Gatekeeper/SmartScreen dance described in the main README.
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/balor/kyoku/releases/download/v0.2.2/kyoku-0.2.2-aarch64-apple-darwin.tar.gz"
      sha256 "76b01d09015316f7bdbc3b753051e4323719b1b467011a3eb67f377e9bf40fb8"
    else
      url "https://github.com/balor/kyoku/releases/download/v0.2.2/kyoku-0.2.2-x86_64-apple-darwin.tar.gz"
      sha256 "8fd687812c995b8a56f03810ad05296b1aceff2ff013d292735c90a78d9dbcdb"
    end
  end

  on_linux do
    # Only x86_64 Linux tarballs are published for now. aarch64 Linux users
    # should `cargo install kyoku` (takes a couple of minutes, no system deps).
    if Hardware::CPU.arm?
      odie "kyoku has no prebuilt aarch64 Linux binary; use: cargo install kyoku"
    end
    url "https://github.com/balor/kyoku/releases/download/v0.2.2/kyoku-0.2.2-x86_64-unknown-linux-gnu.tar.gz"
    sha256 "34fc078fac7bce7039c2f192e7dddf79815f3e06b54ce31456f2f1937ee56af8"
  end

  def install
    bin.install "kyoku"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/kyoku --version")
  end
end
