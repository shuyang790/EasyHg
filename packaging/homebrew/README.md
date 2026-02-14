# Homebrew Bottles (Tap-Managed)

This directory contains tooling and workflow templates for running a tap-owned bottle pipeline.

## What lives where

- Source repo (`easyHg`):
  - `.github/workflows/update-homebrew-tap-formula.yml`
  - `scripts/update-homebrew-tap-formula.sh`
  - Responsibility: bump `Formula/easyhg.rb` in tap repo on each `main` push.
- Tap repo (`homebrew-easyhg`):
  - `Formula/easyhg.rb` (canonical formula)
  - `.github/workflows/publish-bottles.yml` (copy from template below)
  - Responsibility: build/merge/publish macOS bottles.

## Setup Steps

1. In source repo actions secrets, add `TAP_REPO_TOKEN` with access to the tap repo.
2. In source repo workflow, adjust `TAP_REPO` if your tap path differs.
3. Copy `packaging/homebrew/tap-workflows/publish-bottles.yml` into tap repo at `.github/workflows/publish-bottles.yml`.
4. In tap repo, ensure `Formula/easyhg.rb` already exists.
5. Merge a formula bump PR and confirm bottle workflow creates a release with `.bottle.tar.gz` files.

## Notes

- Snapshot formula versions are generated as:
  - `<cargo_version>.dev.<YYYYMMDD>.<shortsha>`
- Formula test assertion matches Cargo version, not snapshot suffix.
- Existing `scripts/generate-homebrew-formula.sh` remains useful for source-tag bootstrap but is not the bottle pipeline path.
