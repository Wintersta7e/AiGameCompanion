# AI Game Companion

Ask an AI questions while playing any game in fullscreen -- without alt-tabbing. Press a hotkey, type your question, optionally attach a screenshot, and get an answer from **Sage**, your in-game advisor.

![Rust](https://img.shields.io/badge/Rust-2021-DEA584?logo=rust)
![Windows](https://img.shields.io/badge/Windows-x86__64-0078D4?logo=windows)
![Gemini](https://img.shields.io/badge/Google_Gemini-free_tier-4285F4?logo=googlegemini)
[![License: MIT](https://img.shields.io/badge/License-MIT-green.svg)](./LICENSE)

## Overview

AI Game Companion is a lightweight DX12/DX11 overlay that hooks into a game's rendering pipeline via Dear ImGui. It talks to Google Gemini (free tier -- no credit card needed) and can search the web for walkthroughs, guides, and current info on your behalf.

- **Zero alt-tab** -- the overlay renders inside the game
- **Screenshot vision** -- attach the current game frame and Sage will analyze it
- **Streaming responses** -- answers appear word-by-word as they're generated
- **Screen translation** -- press F10 to translate foreign text in JRPGs, untranslated games, etc.
- **Completely free** -- Gemini free tier (~250 requests/day)

## Features

### Overlay & Rendering
- **DX12 + DX11 support** with automatic detection (vendored hudhook, patched for broad engine compatibility)
- **DPI-aware scaling** -- UI scales proportionally on 1080p, 1440p, 4K, and ultrawide
- **F9 hotkey toggle** -- show/hide the companion panel instantly
- **Input capture** -- overlay consumes keyboard/mouse so the game ignores your typing

### AI & Chat
- **Google Gemini** -- free tier with vision (screenshot analysis) and streaming
- **Google Search grounding** -- Sage can search the web for guides, walkthroughs, patch notes
- **Multi-turn conversation** -- full chat history within each session
- **Streaming responses** -- SSE streaming with live word-by-word display
- **Cancel & clear** -- cancel in-flight requests or clear the conversation
- **Conversation logging** -- session transcripts saved to timestamped files in `logs/`
- **Real-time translation** -- press F10 to capture the screen and translate all foreign text (configurable hotkey and target language)

### Screenshot Capture
- **Attach screenshots** with one click -- current game frame sent as context to Gemini
- **Hide-capture-show** -- overlay hides for 2 frames during capture so it doesn't appear in the screenshot
- **Automatic downscaling** -- large screenshots are resized before sending

### Injector
- **Watch mode** -- configure `[[games]]` in config and the injector auto-injects when a game launches
- **PID tracking** -- re-injects automatically on game relaunch
- **Direct mode** -- `--process "Game.exe"` for one-off injection
- **Game name detection** -- 3-tier priority: config name > window title > exe name, prepended to Sage's system prompt

## Quick Start

1. **Get a free API key** -- Go to [Google AI Studio](https://aistudio.google.com), sign in, click "Get API Key", and create one. No billing setup required.

2. **Configure** -- Copy `config.example.toml` to `config.toml` next to `overlay.dll` and add your key:
   ```toml
   [api]
   key = "your-gemini-api-key-here"
   ```

3. **Inject** -- Run the injector from PowerShell:
   ```
   # Direct injection
   .\injector.exe --process "Game.exe"

   # Or watch mode (auto-inject configured games)
   .\injector.exe
   ```

4. **Use** -- Press **F9** in-game to toggle the companion panel. Type your question, optionally check "Attach Screenshot", and press Send (or Enter). Press **F10** to instantly translate foreign text on screen.

## Configuration Reference

All fields have sensible defaults. Only `api.key` is required.

```toml
[api]
key = ""                    # Gemini API key (required, free from aistudio.google.com)
model = "gemini-2.5-flash"  # Model to use
max_tokens = 1024           # Max response length
system_prompt = "You are a helpful game companion. Be concise and direct."

[overlay]
# graphics_api = "dx11"     # Force a specific API (auto-detects if omitted)
hotkey = "F9"               # Toggle key
width = 500                 # Initial panel width (scales with display)
height = 400                # Initial panel height (scales with display)
opacity = 0.85              # Panel background opacity
font_size = 16              # Base font size in pixels (scales with display)
translate_hotkey = "F10"    # Hotkey for screen translation

[capture]
enabled = true              # Allow screenshot capture
max_width = 1920            # Downscale screenshots wider than this
quality = 85                # PNG compression quality

[logging]
enabled = true              # Save conversation transcripts
# directory = "C:\\custom\\log\\path"  # Defaults to logs/ next to the DLL

[translation]
enabled = true              # Enable screen translation hotkey
target_language = "English" # Translate foreign text to this language

# Watch mode: injector auto-injects when these games are running
# [[games]]
# name = "Horizon Zero Dawn"
# process = "HorizonZeroDawnRemastered.exe"
#
# [[games]]
# name = "Elden Ring"
# process = "eldenring.exe"
```

## Injector CLI

```
injector.exe [OPTIONS]

Options:
  -p, --process <NAME>   Target process name (e.g. "Game.exe")
  -d, --dll <PATH>       Path to overlay.dll (defaults to same directory)
  -t, --timeout <SECS>   Seconds to wait for the process to appear (0 = no retry)
      --list             List running processes and exit
  -h, --help             Print help

With no flags: enters watch mode using [[games]] from config.toml
```

## Project Structure

```
├── Cargo.toml                     # Workspace root
├── crates/
│   ├── overlay/src/               # DLL (cdylib)
│   │   ├── lib.rs                 # DllMain, hook setup, render loop, API dispatch
│   │   ├── ui.rs                  # ImGui panel, DPI scaling, streaming display
│   │   ├── api.rs                 # Gemini SSE streaming client, Google Search grounding
│   │   ├── capture.rs             # Screen DC screenshot -> PNG -> base64
│   │   ├── config.rs              # TOML config, GraphicsApi, GameEntry, TranslationConfig
│   │   ├── game_detect.rs         # 3-tier game name detection
│   │   ├── logging.rs             # Session transcript logging
│   │   └── state.rs               # AppState (parking_lot::Mutex), streaming/capture flags
│   └── injector/src/
│       └── main.rs                # CLI, watch mode, DLL injection, process finding
├── vendor/
│   └── hudhook/                   # Vendored hudhook 0.8.3 (patched for DX12 compatibility)
├── config.example.toml            # Config template (no real API key)
├── scripts/build.sh               # Release build script
└── release/                       # Build output (gitignored)
```

## Tech Stack

| Technology | Purpose |
|------------|---------|
| [Rust](https://www.rust-lang.org) | Systems language for both DLL and injector |
| [hudhook](https://github.com/veeenu/hudhook) | DX12/DX11 render hooking (vendored + patched) |
| [Dear ImGui](https://github.com/ocornut/imgui) | Immediate-mode game UI |
| [reqwest](https://docs.rs/reqwest) | HTTP client for Gemini API |
| [tokio](https://tokio.rs) | Async runtime for non-blocking API calls |
| [parking_lot](https://docs.rs/parking_lot) | Fast mutex for render thread shared state |
| [tracing](https://docs.rs/tracing) | Structured logging to `companion.log` |
| [cargo-xwin](https://github.com/rust-cross/cargo-xwin) | MSVC cross-compilation from WSL2 |

## Building from Source

### Prerequisites (WSL2 Cross-Compilation)

Building is done from WSL2 using `cargo-xwin`. No native Visual Studio required.

```bash
# Rust toolchain
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup target add x86_64-pc-windows-msvc

# Build tools
cargo install cargo-xwin
sudo apt install clang lld llvm
```

### Build

```bash
# Debug build
cargo xwin build --target x86_64-pc-windows-msvc

# Release build (copies artifacts to release/)
./scripts/build.sh
```

## Troubleshooting

**Overlay doesn't appear**
- Verify the process name matches exactly (use `--list` to check)
- The game must use DX12 or DX11 (DX9 and OpenGL are detected but not yet supported)
- If auto-detection picks the wrong API, add `graphics_api = "dx11"` (or `"dx12"`) to `[overlay]` in config.toml
- Some games with anti-cheat (EAC, BattlEye) may block DLL injection
- Check `companion.log` next to the DLL for diagnostic info

**Screenshot is black**
- Screen DC capture works for most games running under DWM composition
- Try borderless windowed mode if exclusive fullscreen doesn't work

**API errors**
- "Invalid API key" -- verify `api.key` in config.toml (get yours at [Google AI Studio](https://aistudio.google.com))
- "Rate limited" -- free tier allows ~250 requests/day; wait and retry
- "Bad request" -- try a shorter message or remove the screenshot
- "Network error" -- check internet connection

**Game crashes on injection**
- Anti-cheat software may cause crashes -- try a single-player game first
- Borderless windowed mode is more stable than exclusive fullscreen

## Known Limitations

- Screen DC capture instead of swapchain backbuffer (some exclusive fullscreen games may show black)
- DX9 and OpenGL detected but not yet supported
- Vulkan out of scope (hudhook doesn't support it; most Vulkan games offer a DX11/DX12 option)
- Free tier rate limit: ~250 requests/day, 10-15 RPM

## Roadmap

**v0.4 -- In Progress**
- Quick-ask hotkey (one-button screenshot + predefined question)
- Per-game profiles + personality modes
- Voice I/O (speech-to-text input, text-to-speech output)
- Region-select translation (draw a box to translate specific text)

## Contributing

Contributions are welcome! Please:

1. Fork the repository
2. Create a feature branch
3. Make your changes
4. Run `cargo xwin clippy --target x86_64-pc-windows-msvc -- -D warnings`
5. Submit a pull request

## License

MIT License -- see [LICENSE](./LICENSE) for details.

## Support

- Star this repository
- [Report issues](../../issues) or suggest features
- Contribute code or ideas
