# easyHg

`easyHg` is a terminal UI for Mercurial inspired by `lazygit`.

It focuses on the daily edit -> review -> commit -> sync loop and keeps core operations one keypress away.

## Status

This repository currently contains an MVP implementation:

- Multi-panel terminal UI (`ratatui` + `crossterm`)
- Async `hg` command execution
- Live repository snapshot refresh
- File diff and revision patch detail views
- Confirmation gates for risky actions
- Extension-aware actions for `rebase` and `histedit`

## Requirements

- Rust toolchain (stable; tested with modern cargo/rustc)
- Mercurial installed and available as `hg` in `PATH`
- Run inside an existing Mercurial repository
- TTY terminal environment (raw mode is required)

## Quick Start

```bash
cargo run
```

### CLI Options

```bash
easyhg --help
easyhg --version
```

## Keybindings

- `q`: quit
- `Tab` / `Shift+Tab`: cycle focused panel
- `j` / `k`: move selection in focused panel
- `r`: refresh repository snapshot
- `d`: reload details for selected file/revision
- `?`: append help text into command log

### Actions

- `c`: commit (opens commit message input)
- `b`: create bookmark (opens bookmark name input)
- `u`: update to selected revision/bookmark (with confirmation)
- `p`: push (with confirmation)
- `P`: pull (`hg pull -u`)
- `i`: incoming
- `o`: outgoing
- `s`: create shelf (if `shelve` is available)
- `U`: unshelve selected shelf (with confirmation)
- `m`: mark selected conflict as resolved
- `M`: mark selected conflict as unresolved
- `R`: rebase selected revision onto `.` (if `rebase` is available, with confirmation)
- `H`: start `histedit` from selected revision (if `histedit` is available, with confirmation)

## Optional Config

Config path:

`~/.config/easyhg/config.toml`

Supported fields in the current implementation:

- `theme`: string (currently informational; default `"auto"`)
- `[keybinds]`: key override map (loaded and surfaced in status/log)
- `[[custom_commands]]`: loaded and logged at startup

Example:

```toml
theme = "auto"

[keybinds]
commit = "c"

[[custom_commands]]
id = "lint"
title = "Run Lint"
context = "repo" # repo | file | revision
command = "cargo clippy"
needs_confirmation = true
```

## Architecture

- `src/main.rs`: app entrypoint
- `src/app.rs`: event loop, state machine, key handling, async job wiring
- `src/ui.rs`: panel rendering and modal rendering
- `src/hg/mod.rs`: Mercurial client, capability detection, parser layer, action runner
- `src/config.rs`: config parsing/loading
- `src/domain.rs`: core domain models

## Development

Run tests:

```bash
cargo test
```

Format and test:

```bash
cargo fmt
cargo test
```

## Homebrew (Custom Tap)

`easyhg` should be distributed via a custom tap repo named `homebrew-easyhg`.

### 1. Tag and push a release

```bash
git tag v0.1.0
git push origin v0.1.0
```

### 2. Generate formula with correct SHA

```bash
./scripts/generate-homebrew-formula.sh v0.1.0 shuyang790 EasyHg
```

This writes `packaging/homebrew/easyhg.rb`.

### 3. Publish formula in your tap repo

Create `https://github.com/shuyang790/homebrew-easyhg` and copy formula to:

`Formula/easyhg.rb`

Then commit and push in that tap repo.

### 4. Install from tap

```bash
brew tap shuyang790/easyhg
brew install easyhg
```

## Current Limitations

- No staged-hunk UI yet (Mercurial interactive commit flow is not embedded)
- Config values are loaded, but only part of them currently influence behavior
- No integration test harness yet (only parser/config unit tests)
