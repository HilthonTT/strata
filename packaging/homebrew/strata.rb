# Homebrew formula for strata, built from the source tarball of a release tag.
#
# At release time: set `url` to the new tag, then replace the sha256 with
#   curl -sL https://github.com/HilthonTT/strata/archive/refs/tags/vX.Y.Z.tar.gz | shasum -a 256
# and test with `brew install --build-from-source ./packaging/homebrew/strata.rb`.
class Strata < Formula
  desc "Fast, extensible terminal file explorer"
  homepage "https://github.com/HilthonTT/strata"
  url "https://github.com/HilthonTT/strata/archive/refs/tags/v0.1.0.tar.gz"
  # FILL IN AT RELEASE TIME: sha256 of the tarball above.
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"
  license "MIT"
  head "https://github.com/HilthonTT/strata.git", branch: "main"

  depends_on "pkgconf" => :build
  depends_on "rust" => :build
  depends_on "openssl@3"

  uses_from_macos "zlib"

  def install
    # Link against Homebrew's OpenSSL rather than building one.
    ENV["OPENSSL_NO_VENDOR"] = "1"
    ENV["OPENSSL_DIR"] = Formula["openssl@3"].opt_prefix
    system "cargo", "install", *std_cargo_args(path: "crates/strata")
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/strata --version")
    assert_match "[general]", shell_output("#{bin}/strata --dump-config")
  end
end
