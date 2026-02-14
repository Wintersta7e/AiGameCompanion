# Game Companion

A lightweight in-game overlay that lets you ask an AI questions while playing games in fullscreen. Press a hotkey, type your question, optionally capture the current game screen, and get a response — all without leaving the game.

Powered by **Google Gemini** (free tier — no credit card needed). Responses appear as **Sage**, your in-game advisor.

## Features

- **In-game overlay** via Dear ImGui, rendered directly in the game's rendering pipeline
- **DX12 + DX11 support** with auto-detection (vendored hudhook, patched for broad compatibility)
- **Screenshot capture** — attach the current game frame to your message (screen DC capture)
- **Multi-turn conversation** — full chat history within each session
- **DPI-aware scaling** — UI scales proportionally on 1440p, 4K, and ultrawide displays
- **Configurable** — API model, hotkey, panel size, opacity, font size, screenshot resolution
- **Cancel and Clear** — cancel in-flight requests, clear conversation history
- **Completely free** — uses Gemini 2.5 Flash free tier (~250 requests/day)

## Prerequisites (WSL2 Cross-Compilation)

Building is done from WSL2 using `cargo-xwin`. No native Visual Studio required.

Install in WSL:

```bash
# Rust toolchain
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup target add x86_64-pc-windows-msvc

# Build tools
cargo install cargo-xwin
sudo apt install clang lld llvm
```

## Building

```bash
# Debug build
cargo xwin build --target x86_64-pc-windows-msvc

# Release build (recommended)
./scripts/build.sh
```

The release build script copies `overlay.dll`, `injector.exe`, and `config.example.toml` into a `release/` folder.

## Usage

1. **Get a free API key** — Go to https://aistudio.google.com, sign in with your Google account, click "Get API Key", and create one. No billing setup required.

2. **Configure** — Copy `config.example.toml` to `config.toml` next to `overlay.dll` and add your key:
   ```toml
   [api]
   key = "your-gemini-api-key-here"
   ```

3. **Inject** — Run the injector from PowerShell, pointing it at the game process:
   ```
   .\injector.exe -p "Game.exe"
   ```
   Use `--timeout 30` if the game takes a while to start, or `--list` to see running processes.

4. **Use** — Press **F9** in-game to toggle the companion panel. Type your question, optionally check "Attach Screenshot", and press Send (or Enter).

## Configuration Reference

All fields have sensible defaults. Only `api.key` is required.

```toml
[api]
key = ""                    # Gemini API key (required, free from aistudio.google.com)
model = "gemini-2.5-flash"  # Model to use
max_tokens = 1024           # Max response length
system_prompt = "You are a helpful game companion. Be concise and direct."

[overlay]
# graphics_api = "dx11"     # Force a specific API (auto-detects if omitted). Options: dx12, dx11, dx9, opengl
hotkey = "F9"               # Toggle key
width = 500                 # Initial panel width (scales with display resolution)
height = 400                # Initial panel height (scales with display resolution)
opacity = 0.85              # Panel background opacity
font_size = 16              # Base font size in pixels (scales with display resolution)

[capture]
enabled = true              # Allow screenshot capture
max_width = 1920            # Downscale screenshots wider than this
quality = 85                # PNG compression quality
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
```

## Troubleshooting

**Overlay doesn't appear**
- Verify the process name matches exactly (use `--list` to check)
- The game must use DX12 or DX11 (DX9 and OpenGL are not yet supported)
- If auto-detection picks the wrong API, add `graphics_api = "dx11"` (or `"dx12"`) to `[overlay]` in config.toml
- Some games with anti-cheat (EAC, BattlEye) may block DLL injection
- Check `companion.log` next to the DLL for diagnostic info

**Screenshot is black**
- Screen DC capture works for most games running under DWM composition
- Try borderless windowed mode if exclusive fullscreen doesn't work

**API errors**
- "Invalid API key" — verify `api.key` in `config.toml` (get yours at https://aistudio.google.com)
- "Rate limited" — free tier allows ~250 requests/day; wait and retry
- "Bad request" — try a shorter message or remove the screenshot
- "Network error" — check internet connection
- Timeout after 30 seconds — the API may be slow; try again

**Game crashes on injection**
- Anti-cheat software may cause crashes — try a single-player game first
- Borderless windowed mode is more stable than exclusive fullscreen

## Known Limitations

- Screenshot includes the overlay panel (hide-capture-show planned for v0.2)
- Screen DC capture instead of swapchain backbuffer (some exclusive fullscreen games may show black)
- No streaming responses (full response appears at once)
- DX9 and OpenGL not yet supported (detected but stubbed)
- Vulkan out of scope (hudhook doesn't support it; most Vulkan games offer a DX11/DX12 option)
- Conversation history is per-session only (not saved to disk)
- Free tier rate limit: ~250 requests/day, 10-15 RPM

## Roadmap

**v0.2 — Polish & Compatibility**
- ~~Graphics API auto-detection + DX11 support~~ (done)
- ~~Optional `graphics_api` config override~~ (done)
- DX9 support (legacy/JRPG engines), OpenGL support (niche)
- Injector auto-inject (background watcher for configured games)
- Streaming responses
- Hide-capture-show for clean screenshots
- Conversation log saving

**v0.3 — Nice to Have**
- Game profile system (per-game system prompts)
- Quick screenshot + predefined question hotkey

## License

MIT
