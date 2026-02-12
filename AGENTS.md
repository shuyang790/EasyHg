# AGENTS.md

This file defines repo-specific guidance for coding agents and contributors working on `easyHg`.

## Project Goal

Build a fast, safe, `lazygit`-style terminal UI for Mercurial with strong defaults for daily use:

- inspect working tree and history quickly
- run common local/remote actions safely
- keep risky operations explicit and confirmable

## Stack

- Language: Rust
- UI: `ratatui` + `crossterm`
- Async runtime: `tokio`
- Serialization/parsing: `serde`, `serde_json`, `toml`
- Error handling: `anyhow`

## Code Layout

- `src/main.rs`: startup, config load, app launch
- `src/app.rs`: app state, key handling, background jobs, event loop
- `src/ui.rs`: terminal layout and rendering
- `src/hg/mod.rs`: Mercurial command adapter + parsing + capability detection
- `src/config.rs`: config schema and load logic
- `src/domain.rs`: shared domain model types

## Engineering Rules

- Prefer typed domain structs over passing raw JSON/text across modules.
- Keep Mercurial-specific behavior in `src/hg/mod.rs`; UI layer should stay command-agnostic.
- Always gate extension-dependent features via capability detection.
- Preserve safety confirmations for high-risk actions.
- Do not silently ignore command failures; surface errors in UI log/status.
- Avoid blocking the UI thread; command execution should stay async.

## Parsing and Command Strategy

- Use `-Tjson` when Mercurial supports machine-readable output.
- For commands without stable JSON output, keep text parsing narrow and defensive.
- Add unit tests for every new parser and parser edge case.
- Include command preview strings for all actions to improve operator trust.

## Testing Expectations

Before finishing a change:

1. Run `cargo fmt`.
2. Run `cargo test`.
3. If parser behavior changed, add or update tests in module-local test blocks.

When adding new `HgAction` variants:

1. Add action preview text.
2. Add execution mapping in `run_action`.
3. Wire key handling or invocation path in `app`.
4. Add at least one parser/behavior test where applicable.

## UX Principles

- Keep keyboard-first operation.
- Keep focused-panel behavior obvious (visual highlight + predictable navigation).
- Keep command log useful and concise.
- Prefer explicit confirmations over surprising side effects.
- Never hide dangerous behavior behind a single accidental keypress.

## Out of Scope for MVP

- Multi-repo dashboard
- Background daemon
- Full lazygit config parity
- Advanced patch queue UX (`mq`)

## Suggested Next Work

- Add integration tests with temporary Mercurial repositories.
- Implement custom command execution path from config.
- Add staged/interactive commit UX.
- Add CLI flags for non-interactive diagnostics and version/help output.
