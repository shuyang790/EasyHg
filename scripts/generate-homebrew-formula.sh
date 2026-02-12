#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 1 ]]; then
  echo "Usage: $0 <version|tag> [owner] [repo]"
  echo "Example: $0 v0.1.0 shuyang790 EasyHg"
  exit 1
fi

raw_version="$1"
version="${raw_version#v}"
tag="v${version}"
owner="${2:-}"
repo="${3:-}"

if [[ -z "${owner}" || -z "${repo}" ]]; then
  remote_url="$(git remote get-url origin)"
  if [[ "${remote_url}" =~ github\.com[:/]([^/]+)/([^/.]+)(\.git)?$ ]]; then
    owner="${owner:-${BASH_REMATCH[1]}}"
    repo="${repo:-${BASH_REMATCH[2]}}"
  else
    echo "Could not infer owner/repo from origin remote: ${remote_url}"
    echo "Please pass owner and repo explicitly."
    exit 1
  fi
fi

archive_url="https://github.com/${owner}/${repo}/archive/refs/tags/${tag}.tar.gz"
tmp_archive="$(mktemp -t easyhg-archive-XXXXXX.tar.gz)"
trap 'rm -f "${tmp_archive}"' EXIT

echo "Downloading ${archive_url}"
curl -fsSL "${archive_url}" -o "${tmp_archive}"
sha256="$(shasum -a 256 "${tmp_archive}" | awk '{print $1}')"

mkdir -p packaging/homebrew
cat > packaging/homebrew/easyhg.rb <<EOF
class Easyhg < Formula
  desc "Lazygit-style terminal UI for Mercurial"
  homepage "https://github.com/${owner}/${repo}"
  url "${archive_url}"
  sha256 "${sha256}"
  license "MIT"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args
  end

  test do
    assert_match "${version}", shell_output("#{bin}/easyhg --version")
  end
end
EOF

echo "Generated packaging/homebrew/easyhg.rb"
echo "sha256: ${sha256}"
