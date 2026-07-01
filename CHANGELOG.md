# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## 2.0.0 - 2026-07-01

A ground-up rewrite. The DLL-injection overlay is replaced by an external,
transparent companion window: Sage no longer injects into games or hooks a
graphics API -- it runs as its own window composited over the game, so it works
with any renderer and cannot crash the game.

### Added

- **External overlay** -- a transparent, always-on-top companion window toggled
  with `Ctrl+Shift+G`. Draggable, and takes keyboard focus only while interactive.
- **Global hotkeys** -- `Ctrl+Shift+T` to translate on-screen text and
  `Ctrl+Shift+A` to quick-ask a preset question with the current frame attached.
- **In-app provider setup** -- enter your Gemini API key in Settings; it is stored
  in the Windows Credential Manager, never in plaintext. `config.toml` becomes an
  optional fallback.
- **Redesigned Settings** -- Providers / Hotkeys / Launcher / About, with live CLI
  detection, a re-check button, and a persisted default provider.
- **Screenshot capture** via Windows.Graphics.Capture, with no injection.
- Launcher and overlay screenshots in the README.

### Changed

- **In-process AI** -- Gemini (direct API) and Claude / Codex (spawned CLIs) now
  run inside the launcher and stream over a Tauri channel; the localhost proxy is
  gone. A new request cancels and replaces the previous one.
- **Playtime** is tracked by an external process watcher. Steam sessions are
  detected via Steam's own registry running-flag (keyed by app id) rather than
  guessing the game executable.
- Game detection uses the foreground window instead of an injected hook.

### Removed

- DLL injection, the CLI injector, and the vendored/patched hudhook (~14,500
  lines). The launcher is now the only crate.

### Fixed

- Settings no longer reverts a newly-picked default provider on save.
- Codex answers that are a bare JSON value are no longer dropped.
- The launch button is disabled while a game is running (previously a no-op
  "Relaunch").

### Infrastructure

- Strict CI: `cargo fmt` / clippy / test, prettier / svelte-check / build,
  gitleaks, and cargo-deny, all required to merge.
- Expanded the unit-test suite and added `scripts/ci-check.sh` to mirror CI
  locally before pushing.
- Batched dependency updates (Tauri, Svelte, Vite, Tokio, and others).

## 1.2.1 - 2026-05-06

- Cancel-flow fix, audit cleanups, first unit tests, and dependency bumps. See
  the GitHub release notes for detail.
