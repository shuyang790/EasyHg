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

## Current Limitations

- No command-line flags yet (launches interactive TUI directly)
- No staged-hunk UI yet (Mercurial interactive commit flow is not embedded)
- Config values are loaded, but only part of them currently influence behavior
- No integration test harness yet (only parser/config unit tests)
