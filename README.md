<div align="center">

# AI Game Companion

**Ask an AI while you play -- without alt-tabbing.**

A fullscreen game overlay that hooks a game's DX12/DX11 renderer and lets you ask
Google Gemini, Claude, or OpenAI questions in-game. Bring your own provider,
attach a screenshot for context, or translate on-screen text -- all without ever
leaving the game.

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
translation while playing fullscreen, without alt-tabbing to a browser and
losing the moment. Responses come from **Sage**, an in-game advisor that renders
right on top of the running game.

It's deliberately **bring-your-own-AI**: Gemini talks to its own free API, while
Claude and OpenAI run through your *existing* CLI subscriptions -- no middleman
service, no shared keys. Translation can run **fully offline** on a local vision
model. There's no telemetry, no analytics, and no account.

It's a personal tool, not a product -- but it's open source under MIT, and if it
looks useful you're welcome to clone it and try it. No adoption goal and no
support guarantees, but issues and PRs are read.

## Status

Actively developed personal tool. The overlay, injector, and launcher are
implemented and ship as tagged Windows releases. It's still rough in places, and
some games need a little config tuning to hook cleanly -- treat it as a working
tool you can build and run, not a polished consumer app.

**Implemented:**
- DX12 + DX11 overlay with automatic API detection (vendored, patched hudhook +
  Dear ImGui), DPI-aware scaling, F9 toggle, full input capture
- Multi-provider AI -- Gemini (direct API), Claude & OpenAI (through your own CLI
  subscriptions via a localhost proxy), switchable mid-game from a dropdown
- SSE streaming, multi-turn chat, cancel / clear, Google Search grounding
- Screenshot vision (Gemini + Claude) with hide-capture-show and auto-downscale
- Screen translation (F10) via Gemini *or* a local vision model (Ollama / LM Studio)
- CLI injector -- watch mode (auto-inject configured games) + direct mode, with
  3-tier game-name detection
- Desktop launcher (Tauri 2 + Svelte 5) -- Steam library discovery, one-click
  launch+inject, tray, launch-on-startup, play-time tracking
- TOML config, conversation transcript logging

**Not done yet / out of scope:**
- OpenAI screenshots -- blocked on an upstream Codex CLI fix
- DX9 and OpenGL -- detected but not hooked; Vulkan is out of scope (most Vulkan
  games offer a DX11/DX12 path)
- Screen-DC capture (not swapchain backbuffer) -- some exclusive-fullscreen games
  may capture black; borderless windowed is more reliable
- Planned: quick-ask hotkey, per-game profiles + personality modes, voice I/O,
  region-select translation

## Features

### Overlay & rendering
- DX12 + DX11 with automatic detection (vendored hudhook, patched for broad
  engine compatibility)
- DPI-aware UI -- scales proportionally on 1080p, 1440p, 4K, and ultrawide
- F9 toggle; while open, the overlay consumes keyboard/mouse so the game ignores
  your typing

### Multi-provider AI
Sage can talk through **Gemini**, **Claude**, or **OpenAI** -- switch mid-game
from a dropdown.

- **Gemini** -- direct API with a free key ([Google AI Studio](https://aistudio.google.com))
- **Claude** -- your existing [Claude Code](https://claude.ai/code) subscription,
  no separate key
- **OpenAI** -- your existing ChatGPT/OpenAI subscription via
  [Codex CLI](https://openai.com/codex/), no separate key

The launcher runs a localhost proxy that spawns the official CLI tools as
subprocesses -- the same pattern documented for
[headless Claude Code](https://code.claude.com/docs/en/headless). No OAuth tokens
are extracted or shared; each user authenticates their own CLIs. Providers never
fall back to one another silently.

> Screenshots are supported for Gemini and Claude. You are responsible for
> ensuring your use of each provider complies with that provider's terms.

### Screen translation
Press F10 to capture the screen and translate foreign text (JRPGs, untranslated
releases, etc.). Runs through Gemini, or fully offline on a local vision model
(Ollama / LM Studio) -- no internet, no content filter, no rate limits.
Configurable hotkey and target language; disabled by default.

### Injector
> Injects a UI overlay for AI assistance only. It does not read or modify game
> memory, intercept network traffic, or touch game logic. Kernel-level anti-cheat
> may block injection.

- Watch mode -- configure `[[games]]` and it auto-injects on launch, re-injecting
  on relaunch
- Direct mode -- `--process "Game.exe"` for a one-off
- 3-tier game-name detection (config name > window title > exe name) fed into
  Sage's system prompt

### Desktop launcher
A Tauri 2 + Svelte 5 GUI for your library: Steam auto-discovery, Steam-CDN cover
art, one-click launch+inject, settings (DLL path, scan-on-startup, minimize-to-
tray, launch-on-startup), quick buttons to open `config.toml` / `companion.log`,
and play-time tracking.

## Stack

| Layer | Choice | Notes |
|---|---|---|
| Overlay | [Rust][rust] cdylib + [Dear ImGui][imgui] | Injected DLL, immediate-mode UI on the game's render thread |
| Hooking | vendored [hudhook][hudhook] 0.8.3 (patched) | DX12/DX11 swapchain hook; widened command-queue scan |
| Async / HTTP | [tokio][tokio] + [reqwest][reqwest] | API calls off the render thread; deferred runtime init |
| Shared state | [parking_lot][parking_lot] | Fast mutex -- the render thread must never block |
| Proxy | [axum][axum] | Localhost bridge that spawns the Claude / Codex CLIs |
| Injector | Rust + Win32 | Watch-mode + direct DLL injection |
| Launcher | [Tauri 2][tauri] + [Svelte 5][svelte] + Tailwind 4 | Steam discovery, launch+inject, tray, autostart |
| Build | [cargo-xwin][cargo-xwin] | MSVC cross-compile from WSL2 |
| Logging | [tracing][tracing] | Structured logs to `companion.log` |

## Quick start

1. **Get a free Gemini key** at [Google AI Studio](https://aistudio.google.com)
   -- "Get API Key", no billing required. (Or skip it and use your existing
   Claude / OpenAI subscription instead.)

2. **Configure.** Copy `config.example.toml` to `config.toml` next to
   `overlay.dll` and set your key:
   ```toml
   [api.gemini]
   key = "your-gemini-api-key-here"
   ```
   Every other field has a sensible default; `config.example.toml` documents the
   full reference (providers, overlay sizing, capture, logging, translation,
   watch-mode games).

3. **Inject** from PowerShell:
   ```powershell
   .\injector.exe --process "Game.exe"   # direct
   .\injector.exe                          # watch mode (auto-inject configured games)
   ```

4. **Play.** Press **F9** in-game to toggle the panel, type a question, and
   optionally attach the current frame. Press **F10** to translate on-screen text.

## Layout

```
├── Cargo.toml                  # workspace root + [patch.crates-io] hudhook
├── crates/
│   ├── overlay/                # injected DLL (cdylib): hook, ImGui UI, AI dispatch
│   ├── injector/               # CLI exe: watch mode + DLL injection
│   └── launcher/               # desktop GUI (Tauri 2 + Svelte 5)
│       ├── src/                # Svelte frontend (runes)
│       └── src-tauri/          # Rust backend: Steam discovery, settings, CLI proxy
├── vendor/hudhook/             # vendored hudhook 0.8.3 (patched for DX12 compat)
├── config.example.toml         # config template (no real key)
└── scripts/build.sh            # release build -> release/
```

## Building from source

Built from WSL2 with [`cargo-xwin`][cargo-xwin] -- no native Visual Studio needed.

```bash
rustup target add x86_64-pc-windows-msvc
cargo install cargo-xwin
sudo apt install clang lld llvm

cargo xwin build --target x86_64-pc-windows-msvc   # debug
./scripts/build.sh                                  # release -> release/
```

Lint + test gates:

```bash
cargo xwin clippy --workspace --target x86_64-pc-windows-msvc -- -D warnings
cargo test -p launcher    # proxy helpers; runs on the Linux host
```

## Troubleshooting

- **Overlay doesn't appear** -- confirm the process name (`injector.exe --list`),
  make sure the game is DX12/DX11, and check `companion.log`. Anti-cheat (EAC,
  BattlEye) can block injection.
- **Wrong graphics API detected** -- set `graphics_api = "dx11"` (or `"dx12"`)
  under `[overlay]`.
- **Screenshot is black** -- try borderless windowed; screen-DC capture needs DWM
  composition.
- **Crash on injection, or a slow-loading game** -- add `hook_delay = 15` under
  `[overlay]` to wait out a long DX12 init; prefer single-player titles when
  testing.
- **API errors** -- "invalid key" check `api.gemini.key`; "rate limited" the free
  tier allows roughly 250 requests/day.

## Design principles

1. **Never crash the host game.** The DLL avoids panics, wraps risky FFI in
   `catch_unwind`, and degrades gracefully if the async runtime fails to start.
2. **Bring your own AI, no middleman.** Gemini talks direct; Claude/OpenAI spawn
   your own authenticated CLIs through a localhost proxy. No tokens extracted.
3. **No silent fallback.** Providers never switch behind your back -- a failed
   request fails loudly rather than leaking your prompt to a different vendor.
4. **Private by option.** Translation can run fully offline on a local model; no
   telemetry or analytics anywhere.
5. **Launcher and CLI parity.** Anything the launcher automates, the injector
   does from a terminal.

## License

[MIT](LICENSE).

---

<sub>AI Game Companion is a personal tool -- built for my own use, with no
telemetry or analytics. It injects an overlay for AI assistance only: it does not
read or modify game memory, intercept network traffic, or touch game logic. You
are responsible for complying with each AI provider's terms of service. Not
chasing adoption, but if it looks useful, you're welcome to try it.</sub>

[rust]:        https://www.rust-lang.org
[imgui]:       https://github.com/ocornut/imgui
[hudhook]:     https://github.com/veeenu/hudhook
[tokio]:       https://tokio.rs
[reqwest]:     https://docs.rs/reqwest
[parking_lot]: https://docs.rs/parking_lot
[axum]:        https://docs.rs/axum
[tauri]:       https://tauri.app
[svelte]:      https://svelte.dev
[cargo-xwin]:  https://github.com/rust-cross/cargo-xwin
[tracing]:     https://docs.rs/tracing
