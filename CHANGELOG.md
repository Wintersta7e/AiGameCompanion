# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## 2.1.0 - 2026-10-07

### Added

- **The panel opens on the game's monitor, along the right edge of the game,
  and remembers where you drag or resize it.**
- **Hints first, on by default -- including after an upgrade.** Questions about
  where to go or how to get past something first get a nudge; **Another hint**
  goes one step further and **Full answer** gives the solution. Switch it off
  from the panel.
- **Formatted answers and a Copy button.** Lists, bold text, code and tables
  display as such; Copy copies the answer's text.

### Security

- **The overlay window can use only the commands its own controls need.** Links
  open only as https web addresses, in your default browser.
- **The launcher is built with Control Flow Guard and opts in to CET shadow
  stacks,** Windows protections that make memory-corruption exploits harder.

### Fixed

- **A Steam scan no longer erases games from other sources.** A manually added
  game, with its playtime, was removed by the next startup scan.
- **Closing the panel with Alt+F4 now hands focus back to the game, as the
  hotkey does.**
- **Claude errors show the Claude CLI's own message** instead of a generic one.
- **A game without cover art shows the first letter or number of its name.** A
  name that starts with an emoji showed a broken character instead, and a name
  in a non-Latin script showed its second letter.
- **Store apps link as themselves.** The overlay showed and linked the window
  host that runs them, so linking one linked them all; each now shows its own
  name and is linked on its own.

### Changed

- **Sage now tells the AI a linked game's Steam app id, or a linked program's
  product name.**
- **Sage no longer volunteers story spoilers and skips preamble.**

### Infrastructure

- **CI now checks commit messages, PR titles and branch names against the
  repository's commit rules, and scans tracked text files for local paths,
  private email addresses and planning ids.** The checks read shapes, not
  meaning, and run after a branch is pushed.
- **Security problems can be reported privately.** `SECURITY.md` explains how,
  and that only the latest release is supported.
- **Dependencies refreshed; the project builds with Rust 1.99.0.**

## 2.0.2 - 2026-09-26

### Security

- **A window title could run commands inside WSL.** When Claude or Codex ran
  through WSL, the launcher started it with `wsl.exe --`, which hands the command
  line to the user's default shell to parse before the CLI's own shell does. Text
  in the system prompt -- including the title of whatever window was in front --
  could therefore expand `$(...)` and run commands. WSL commands now start with
  `wsl.exe --exec`, so the prompt is parsed exactly once. The same double parsing
  is what the 2.0.1 entry described as an environment variable arriving empty.
- **Window titles no longer reach any provider.** The prompt names a game by its
  library name, or by the program's file name for a window you linked yourself,
  and only a linked window is ever named or captured.
- **Codex no longer saves Sage prompts to its session files.** Each Codex
  request -- Sage's instructions, the game's name and the conversation so far --
  was kept in Codex's own session files on disk, and a screenshot could have
  been too. Codex now runs without saving a session, as Claude already did.
- **Chats are no longer copied into the launcher log.** Codex repeats the whole
  prompt on its error output, and the launcher logged every line of it, so each
  conversation ended up in `launcher.log`. That output is now only counted; its
  last few lines are kept for the error message when the CLI fails.

### Fixed

- **A library file the launcher cannot fully read is never overwritten.** If the
  file stays locked (for example by a virus scanner) for more than about a
  second, cannot be read, or is corrupt with no backup possible, the launcher now
  runs read-only for that session: it shows a banner saying so, skips the startup
  scan and leaves launch-on-startup alone. Before, it started from an empty
  library and the next save replaced the file. A backup never replaces an older
  backup, and a file with unreadable game entries is backed up before anything
  writes to it.
- The chat screenshot now rechecks that its target window is still there right
  before capturing, as translation does; a window that closed is not captured.
- **A new Gemini key works on the first question.** The default model was one
  Google no longer opens to newly created keys, so a fresh key failed at once.
  The default is now `gemini-3.6-flash`, and the example `config.toml` no
  longer names the old model.
- **Gemini errors give Google's reason.** A refused request shows the HTTP
  status, Google's own message (with the key blanked out) and what to try next,
  instead of a generic line that pointed at `config.toml`.
- **Codex answers keep their line breaks.** Lists and paragraphs no longer run
  together into a single line.
- **A stopped or failed answer is no longer resent.** Only questions whose answer
  finished go back to the provider as history with the next question; a stopped,
  empty or failed exchange is left out.
- **Saving Settings no longer reverts the provider.** A provider picked in the
  overlay or the top bar while Settings was open stays picked after Save.
- **The overlay no longer switches provider on its own.** When the chosen
  provider is not available yet, it stays selected and the overlay says so; you
  switch from the dropdown.
- **Hotkey conflicts are shown.** If Ctrl+Shift+G, T or A cannot be registered,
  the top bar, the status bar and Settings say which one, in place of a "Watcher
  active" indicator that reflected nothing.
- **Indicators and a Config button that did nothing useful are gone.** The
  detail panel's Translation and Screenshot vision rows described settings that
  do not exist, its Config button opened a file a new install does not have, and
  the translate tab showed a capture box that never held an image. The top bar
  no longer offers providers that are not available.
- **Overlay and Settings text is corrected.** The overlay no longer says it can
  see your screen when no screenshot is attached, neither window sends you to
  `config.toml`, Settings no longer says changes apply without Save, and the
  model names shown are the ones that answer.
- **The Translate tab names Gemini.** It showed the provider picked for chat,
  although every translation is sent to Gemini; it now reads "Translates with
  Gemini".
- **Large screenshots are scaled down.** A frame from a 4K monitor could encode
  to a picture larger than Gemini and Claude accept. Frames larger than
  1920x1080 are now scaled down to fit, keeping their shape, before they are
  sent to any provider.
- **A fresh install picks an available provider.** A new install started with
  Gemini chosen, so without a Gemini key the overlay said Gemini was not
  available even when the Claude or Codex CLI worked. Until you pick one, the
  overlay and the launcher use the first provider that can answer; a provider
  you chose before is kept.
- **The provider switch stays current in both windows.** The launcher showed
  Claude and Codex as unavailable after startup until you pointed at them, and
  neither window followed a provider chosen in the other. Both now update as
  soon as detection finishes or a choice, key or model changes.
- **The Gemini model list is readable.** Its options showed light text on a
  light background.

### Changed

- **Gemini no longer searches the web.** Gemini requests included Google Search,
  whose results must be shown with Google's search suggestions and which paid
  keys are billed for. Gemini now answers from the model alone.
- **The Gemini model is chosen in Settings.** Settings -> Providers -> Gemini
  offers Default, three named models or a custom model id. Default is
  `gemini-3.6-flash`, or a model still named in a legacy `config.toml`.
- **Codex gets screenshots.** With the image button on, Codex sees the game frame
  as Gemini and Claude do. The picture is written to the app's own data folder
  only while the request runs and deleted afterwards, and a file left behind by a
  crash is removed at the next start.
- **Sage's Claude and Codex helpers no longer load your CLI setup.** Sage starts
  the Claude CLI without your tools, plugins, hooks, MCP servers or CLAUDE.md
  instructions, and Codex without your Codex config or its command-running
  tool, so text in a screenshot cannot steer them into your own setup. Codex
  therefore answers with its built-in default model and effort, not the ones in
  your config. A CLI too old for these options fails with its own error message.
- **Windows outside your library are linked with one click.** The overlay opens
  unlinked over any other window and sends nothing about it until you click
  **Link** for that session. Library games link automatically.
- **Ctrl+Shift+A and Ctrl+Shift+T wait for Enter.** Quick-ask puts the preset
  question in the input with the screenshot on; translate opens its tab with the
  button focused. Nothing is captured or sent until you press Enter or click.

## 2.0.1 - 2026-09-16

### Fixed

- **Claude and Codex are reachable again.** Both CLI providers are launched
  through WSL, and the launcher probed them with a shell that sources neither
  the login profile nor, in one code path, anything usable: Claude was reported
  "Not found" on machines that have it, and Codex failed on every request with
  "No such file or directory". Codex under WSL had never worked. Probing now
  uses a login + interactive shell, and its working directory is prepared from
  an absolute path rather than one built from an environment variable that
  arrives empty.
- A provider whose working directory cannot be prepared is reported
  unavailable, instead of being offered and then failing on the first question.
- The launcher no longer warns on every start about disabling an autostart
  entry that was never created.

### Changed

- Dependencies refreshed, and the project now builds under a much stricter lint
  set (clippy pedantic/nursery/cargo plus type-aware linting for the UI). This
  fixed several latent defects, including ignored errors that had been hiding
  exactly the failures above.

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
