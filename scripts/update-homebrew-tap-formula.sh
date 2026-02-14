#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 3 ]]; then
  echo "Usage: $0 <formula_path> <source_owner/repo> <source_commit_sha> [cargo_version]"
  echo "Example: $0 /tmp/homebrew-easyhg/Formula/easyhg.rb shuyang790/EasyHg $GITHUB_SHA 0.2.1"
  exit 1
fi

formula_path="$1"
source_repo="$2"
source_sha="$3"
cargo_version="${4:-}"

if [[ ! -f "$formula_path" ]]; then
  echo "Formula file not found: $formula_path"
  exit 1
fi

if [[ -z "$cargo_version" ]]; then
  cargo_version="$(awk -F '"' '/^version = "/ { print $2; exit }' Cargo.toml)"
fi

if [[ -z "$cargo_version" ]]; then
  echo "Could not determine cargo version."
  exit 1
fi

if [[ ! "$source_sha" =~ ^[0-9a-fA-F]{40}$ ]]; then
  echo "source_commit_sha must be a full 40-character git sha."
  exit 1
fi

archive_url="https://github.com/${source_repo}/archive/${source_sha}.tar.gz"

tmp_archive="$(mktemp -t easyhg-archive-XXXXXX.tar.gz)"
tmp_formula="$(mktemp -t easyhg-formula-XXXXXX.rb)"
trap 'rm -f "${tmp_archive}" "${tmp_formula}"' EXIT

echo "Downloading ${archive_url}"
curl -fsSL "${archive_url}" -o "${tmp_archive}"
sha256="$(shasum -a 256 "${tmp_archive}" | awk '{print $1}')"

has_version_line=0
if grep -Eq '^[[:space:]]+version[[:space:]]+"' "$formula_path"; then
  has_version_line=1
fi

while IFS= read -r line; do
  if [[ "$line" =~ ^[[:space:]]+url[[:space:]]+\" ]]; then
    echo "  url \"${archive_url}\"" >> "$tmp_formula"
    continue
  fi

  if [[ "$line" =~ ^[[:space:]]+sha256[[:space:]]+\" ]]; then
    echo "  sha256 \"${sha256}\"" >> "$tmp_formula"
    if [[ "$has_version_line" -eq 0 ]]; then
      echo "  version \"${cargo_version}\"" >> "$tmp_formula"
    fi
    continue
  fi

  if [[ "$line" =~ ^[[:space:]]+version[[:space:]]+\" ]]; then
    echo "  version \"${cargo_version}\"" >> "$tmp_formula"
    continue
  fi

  if [[ "$line" =~ ^[[:space:]]+assert_match[[:space:]]+\" ]]; then
    echo "    assert_match \"${cargo_version}\", shell_output(\"#{bin}/easyhg --version\")" >> "$tmp_formula"
    continue
  fi

  echo "$line" >> "$tmp_formula"
done < "$formula_path"

mv "$tmp_formula" "$formula_path"

echo "Updated ${formula_path}"
echo "formula_version: ${cargo_version}"
echo "archive_url: ${archive_url}"
echo "sha256: ${sha256}"
