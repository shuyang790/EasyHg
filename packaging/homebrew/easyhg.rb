class Easyhg < Formula
  desc "Lazygit-style terminal UI for Mercurial"
  homepage "https://github.com/shuyang790/EasyHg"
  url "https://github.com/shuyang790/EasyHg/archive/refs/tags/v0.2.0.tar.gz"
  sha256 "5c2f771110b215071948167df14ec52a8927fdbb65eee9617743a5571fb8c236"
  license "MIT"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args
  end

  test do
    assert_match "0.2.0", shell_output("#{bin}/easyhg --version")
  end
end
