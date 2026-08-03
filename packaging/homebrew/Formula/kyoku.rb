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
  version "0.2.1"
  license "MIT"

  # Binaries are not Apple-notarized / code-signed, but files downloaded by
  # Homebrew itself are never quarantined, so installs work without the
  # Gatekeeper/SmartScreen dance described in the main README.
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/balor/kyoku/releases/download/v0.2.1/kyoku-0.2.1-aarch64-apple-darwin.tar.gz"
      sha256 "b234f8867488ff0197761226e57f76686f6b21e0746d9636edc0ce7f79b05125"
    else
      url "https://github.com/balor/kyoku/releases/download/v0.2.1/kyoku-0.2.1-x86_64-apple-darwin.tar.gz"
      sha256 "03043b36d62f5031edaf4dd7623fb83427f4cb6660fa262ba706fa5e086330f5"
    end
  end

  on_linux do
    # Only x86_64 Linux tarballs are published for now. aarch64 Linux users
    # should `cargo install kyoku` (takes a couple of minutes, no system deps).
    if Hardware::CPU.arm?
      odie "kyoku has no prebuilt aarch64 Linux binary; use: cargo install kyoku"
    end
    url "https://github.com/balor/kyoku/releases/download/v0.2.1/kyoku-0.2.1-x86_64-unknown-linux-gnu.tar.gz"
    sha256 "27bb240aa274a299a09c9a2352626bc9d55759e109e819586675f30e1495d0bf"
  end

  def install
    bin.install "kyoku"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/kyoku --version")
  end
end
