<div align="center">

# AI Game Companion

**Ask an AI while you play -- without alt-tabbing.**

A transparent, always-on-top companion window that composites over your game and
lets you ask Google Gemini, Claude, or OpenAI questions in-game. Bring your own
provider, attach a screenshot for context, or translate on-screen text -- all
without ever leaving the game, and **without injecting anything into it**.

[![Rust](https://img.shields.io/badge/rust-2021-DEA584?logo=rust&logoColor=white)](https://www.rust-lang.org)
[![Platform](https://img.shields.io/badge/platform-Windows%20x86__64-0078D4?logo=windows&logoColor=white)](#)
[![Tauri](https://img.shields.io/badge/Tauri-2-24C8DB?logo=tauri&logoColor=white)](https://tauri.app)
[![Gemini](https://img.shields.io/badge/Google_Gemini-free_tier-4285F4?logo=googlegemini&logoColor=white)](https://aistudio.google.com)
[![Claude](https://img.shields.io/badge/Claude-subscription-D97757?logo=anthropic&logoColor=white)](https://claude.ai/code)
[![OpenAI](https://img.shields.io/badge/OpenAI-subscription-412991?logo=openai&logoColor=white)](https://openai.com/codex/)
[![License](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![Status](https://img.shields.io/badge/status-personal%20%C2%B7%20actively%20developed-brightgreen)](#status)

</div>

---

## Why

I built this for myself -- a way to ask an AI for a hint, a build, or a
translation while playing, without alt-tabbing to a browser and losing the
moment. Responses come from **Sage**, an in-game advisor drawn in a panel over
the running game.

Earlier versions injected a DLL and hooked the game's renderer. That was fragile
and looked like the thing anti-cheats flag, so it was **dropped entirely**. Sage
now runs as its **own transparent window** that Windows composites over the game
-- it never injects, never hooks a graphics API, and **cannot crash the game**.

It's deliberately **bring-your-own-AI**: Gemini talks to its own free API, while
Claude and OpenAI run through your *existing* CLI subscriptions -- no middleman
service, no shared keys. There's no telemetry, no analytics, and no account.

It's a personal tool, not a product -- open source under MIT. No adoption goal
and no support guarantees, but issues and PRs are read.

## Status

Actively developed personal tool, shipped as tagged Windows releases. It targets
**borderless / fullscreen-optimized** games (effectively all modern single-player
titles); genuine legacy exclusive-fullscreen games are out of scope. Treat it as
a working tool you can build and run, not a polished consumer app.

**Implemented:**
- External transparent overlay window -- topmost, click-through when idle, takes
  focus on demand; toggled with **Ctrl+Shift+G**. No injection, any graphics API.
- Multi-provider AI -- Gemini (direct API), Claude & OpenAI (through your own
  `claude` / `codex` CLIs), switchable from an in-panel dropdown that shows only
  available providers and persists your choice.
- Streaming "Sage" replies over a Tauri channel, multi-turn chat, Stop / New chat.
- Screenshot vision (Gemini + Claude) via **Windows.Graphics.Capture** -- capture
  the game frame with no injection; skipped for OpenAI (upstream Codex limitation).
- Screen translation (**Ctrl+Shift+T**) and quick-ask (**Ctrl+Shift+A**) hotkeys.
- Desktop launcher (Tauri 2 + Svelte 5) -- Steam library discovery, cover art,
  one-click launch, tray, launch-on-startup, and play-time via an external process
  watcher.
- In-app Settings: your Gemini key is stored in the **Windows Credential Manager**
  (not plaintext), plus CLI detection, a persisted default provider, and launcher
  toggles.

**Not done yet / out of scope:**
- OpenAI screenshots -- blocked on an upstream Codex CLI fix.
- Rebindable hotkeys -- the chords are fixed this build (Settings shows them).
- Positioning the panel over the game's specific monitor (it opens centered).
- Offline / local-model translation -- translation currently runs through Gemini.
- Genuine legacy exclusive-fullscreen games -- an external window can't composite
  over those; borderless / FSO windowed is covered.
- Competitive / kernel-anti-cheat titles are a non-goal.

## Features

### The overlay
- A transparent, frameless, always-on-top panel Windows composites over the game
  -- no DLL, no swapchain hook, works regardless of graphics API.
- **Ctrl+Shift+G** toggles it; while interactive it takes keyboard focus so your
  typing doesn't reach the game, then hands focus back on hide.

### Multi-provider AI
Sage can talk through **Gemini**, **Claude**, or **OpenAI** -- pick one from the
in-panel dropdown (only available providers are shown; the choice persists).

- **Gemini** -- direct API with a free key ([Google AI Studio](https://aistudio.google.com/apikey)),
  entered in Settings and stored in the OS Credential Manager.
- **Claude** -- your existing [Claude Code](https://claude.ai/code) CLI, no separate key.
- **OpenAI** -- your existing [Codex CLI](https://openai.com/codex/), no separate key.

The CLIs are spawned directly as subprocesses -- the same pattern documented for
[headless Claude Code](https://code.claude.com/docs/en/headless). No OAuth tokens
are extracted or shared; each user authenticates their own CLIs. Providers never
fall back to one another silently.

### Screenshot vision & translation
Attach the current frame to a question (Gemini / Claude) -- captured via
Windows.Graphics.Capture, no injection. Press **Ctrl+Shift+T** to translate
on-screen text through Gemini, or **Ctrl+Shift+A** to fire a preset question with
a screenshot attached.

### Desktop launcher
A Tauri 2 + Svelte 5 GUI for your library: Steam auto-discovery, Steam-CDN cover
art, one-click launch, play-time tracking via an external process watcher, tray,
launch-on-startup, and an in-app Settings panel (provider key + detection,
default provider, hotkey reference, launcher toggles).

## Stack

| Layer | Choice | Notes |
|---|---|---|
| App | [Tauri 2][tauri] + [Svelte 5][svelte] + Tailwind 4 | One process, two windows: library + transparent overlay |
| Capture | [windows][windows] crate (WinRT) | Windows.Graphics.Capture -> D3D11 -> PNG; foreground-HWND game detection |
| Async / HTTP | [tokio][tokio] + [reqwest][reqwest] | Gemini streaming + CLI subprocesses off the UI thread |
| Shared state | [parking_lot][parking_lot] | Fast mutex for the shared app state |
| Secrets | [keyring][keyring] | Gemini key in the Windows Credential Manager |
| Build | [cargo-xwin][cargo-xwin] | MSVC cross-compile from WSL2 |
| Logging | [tracing][tracing] | Structured logs to `launcher.log` |

## Quick start

1. **Get a free Gemini key** at [Google AI Studio](https://aistudio.google.com/apikey)
   -- no billing required. (Or skip it and use your existing Claude / OpenAI CLI.)
2. **Run** `launcher.exe`, open **Settings -> Providers**, and paste your Gemini
   key (stored in the Windows Credential Manager). Claude / OpenAI are detected
   automatically if their CLIs are installed.
3. **Play.** With a game in the foreground, press **Ctrl+Shift+G** to open Sage,
   type a question, and optionally attach the current frame. **Ctrl+Shift+T**
   translates on-screen text; **Ctrl+Shift+A** asks a preset question.

## Layout

```
├── Cargo.toml                  # workspace root
├── crates/
│   └── launcher/               # the whole app (Tauri 2 + Svelte 5)
│       ├── src/                # Svelte frontend (runes): library + overlay UI
│       └── src-tauri/          # Rust backend: overlay window, WGC capture,
│                               #   in-process AI dispatch, Steam discovery, secrets
├── config.example.toml         # optional legacy key fallback (no real key)
└── scripts/build.sh            # release build -> release/
```

## Building from source

Built from WSL2 with [`cargo-xwin`][cargo-xwin] -- no native Visual Studio needed.

```bash
rustup target add x86_64-pc-windows-msvc
cargo install cargo-xwin
sudo apt install clang lld llvm

cargo xwin build -p launcher --target x86_64-pc-windows-msvc   # debug
./scripts/build.sh                                              # release -> release/
```

Lint + test gates:

```bash
cargo xwin clippy -p launcher --target x86_64-pc-windows-msvc -- -D warnings
cargo test -p launcher    # pure-logic tests; run on the Linux host
```

## Troubleshooting

- **Overlay doesn't appear over the game** -- use borderless / fullscreen-windowed
  mode; a topmost window can't composite over true exclusive fullscreen.
- **Claude / OpenAI missing from the dropdown** -- their CLIs may still be
  detecting (especially under WSL); open Settings and press **Re-check CLIs**.
- **Screenshot is black** -- some protected/DRM content or minimized windows can't
  be captured via Windows.Graphics.Capture; try borderless.
- **Gemini errors** -- "invalid key" re-enter it in Settings; "rate limited" the
  free tier allows roughly 250 requests/day.

## Design principles

1. **Can't crash the game.** Sage is a separate window, not injected code -- a bug
   in the companion never takes the game down with it.
2. **Bring your own AI, no middleman.** Gemini talks direct; Claude / OpenAI spawn
   your own authenticated CLIs. No tokens extracted, no shared service.
3. **No silent fallback.** Providers never switch behind your back -- a failed
   request fails loudly rather than leaking your prompt to a different vendor.
4. **Secrets stay secret.** The Gemini key lives in the OS Credential Manager,
   never plaintext; no telemetry or analytics anywhere.

## License

[MIT](LICENSE).

---

<sub>AI Game Companion is a personal tool -- built for my own use, with no
telemetry or analytics. It runs as a separate transparent window composited over
the game: it does not inject code, read or modify game memory, or touch network
traffic or game logic. You are responsible for complying with each AI provider's
terms of service. Not chasing adoption, but if it looks useful, you're welcome to
try it.</sub>

[tauri]:       https://tauri.app
[svelte]:      https://svelte.dev
[windows]:     https://github.com/microsoft/windows-rs
[tokio]:       https://tokio.rs
[reqwest]:     https://docs.rs/reqwest
[parking_lot]: https://docs.rs/parking_lot
[keyring]:     https://docs.rs/keyring
[cargo-xwin]:  https://github.com/rust-cross/cargo-xwin
[tracing]:     https://docs.rs/tracing
