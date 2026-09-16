use std::path::{Path, PathBuf};

use steamlocate::SteamDir;

use crate::models::{Game, GameSource};

/// Executable filename patterns to skip when auto-detecting the main game exe.
const SKIP_PATTERNS: &[&str] = &[
    "setup",
    "install",
    "unins",
    "uninst",
    "uninstall",
    "redist",
    "redistributable",
    "crash",
    "report",
    "launch",
    "updater",
    "update",
    "vc_redist",
    "vcredist",
    "dxsetup",
    "dxwebsetup",
    "dotnet",
    "7z",
    "ue4prereq",
    "easyanticheat",
    "battleye",
    "beclient",
    "beservice",
];

/// Steam CDN base URL for app images.
const STEAM_CDN: &str = "https://cdn.cloudflare.steamstatic.com/steam/apps";

/// Outcome of a Steam scan.
///
/// `complete` is the load-bearing part: a scan that could not reach Steam or
/// could not read one of its libraries still returns a plausible-looking list,
/// and callers must not treat that as an authoritative view of what is
/// installed -- overwriting the stored library with it destroys playtime for
/// every game on the drive that happened to be unavailable.
pub(crate) struct ScanOutcome {
    pub games: Vec<Game>,
    pub complete: bool,
}

/// Discover installed Steam games.
///
/// Returns an alphabetically sorted list of games found across all Steam
/// libraries, plus whether every library was read successfully.
/// Exe detection is deferred to launch time for speed.
pub(crate) fn discover_steam_games() -> ScanOutcome {
    let steam_dir = match SteamDir::locate() {
        Ok(dir) => dir,
        Err(e) => {
            tracing::warn!("Failed to locate Steam: {e}");
            return ScanOutcome {
                games: Vec::new(),
                complete: false,
            };
        }
    };

    tracing::info!("Steam located at {}", steam_dir.path().display());

    let libraries = match steam_dir.libraries() {
        Ok(iter) => iter,
        Err(e) => {
            tracing::warn!("Failed to read Steam libraries: {e}");
            return ScanOutcome {
                games: Vec::new(),
                complete: false,
            };
        }
    };

    let mut games: Vec<Game> = Vec::new();
    let mut complete = true;

    for library_result in libraries {
        let library = match library_result {
            Ok(lib) => lib,
            Err(e) => {
                // A library on an unmounted or slow drive. Anything installed
                // there is missing from `games`, so this scan is partial.
                tracing::warn!("Failed to read Steam library: {e}");
                complete = false;
                continue;
            }
        };

        tracing::info!("Scanning library: {}", library.path().display());

        for app_result in library.apps() {
            let Ok(app) = app_result else { continue };

            let name = match &app.name {
                Some(n) if !n.is_empty() => n.clone(),
                _ => continue,
            };

            let install_dir = library.resolve_app_dir(&app);
            let install_exists = install_dir.exists();
            let app_id = app.app_id;

            let cover_art_path = Some(format!("{STEAM_CDN}/{app_id}/library_600x900_2x.jpg"));
            let install_path = if install_exists {
                Some(install_dir.to_string_lossy().into_owned())
            } else {
                None
            };

            games.push(Game {
                id: format!("steam_{app_id}"),
                name,
                source: GameSource::Steam,
                source_id: Some(app_id.to_string()),
                exe_name: String::new(),
                exe_path: None,
                install_dir: install_path,
                cover_art_path,
                last_played: None,
                play_time_minutes: 0,
            });
        }
    }

    tracing::info!(
        "Discovery finished: {} games found (complete: {complete})",
        games.len()
    );
    games.sort_by_key(|g| g.name.to_lowercase());
    ScanOutcome { games, complete }
}

/// Resolve the main exe for a game on demand (called at launch time).
pub(crate) fn resolve_game_exe(install_dir: &Path) -> (String, Option<String>) {
    find_main_exe(install_dir)
}

/// Find the main executable in a game's install directory.
///
/// Scans recursively for `.exe` files, skips known non-game executables
/// (setup, uninstall, redist, crash reporters, etc.), and returns the
/// largest remaining exe as a heuristic for the main game binary.
fn find_main_exe(install_dir: &Path) -> (String, Option<String>) {
    let mut candidates: Vec<(PathBuf, u64)> = Vec::new();
    collect_exes(install_dir, &mut candidates, 0);

    // Sort by size descending -- largest exe is most likely the game
    candidates.sort_by_key(|c| std::cmp::Reverse(c.1));

    if let Some((path, _)) = candidates.first() {
        let exe_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let exe_path = Some(path.to_string_lossy().into_owned());
        (exe_name, exe_path)
    } else {
        (String::new(), None)
    }
}

/// Max exe candidates to collect per game before stopping early.
const MAX_EXE_CANDIDATES: usize = 64;

/// Max directory depth to search for the game executable.
///
/// Must reach 3: the standard Unreal layout puts the real binary at
/// `<install>/<Game>/Binaries/Win64/<Game>-Win64-Shipping.exe`. At the previous
/// limit of 2 that file was never seen, so the only candidate was the root
/// launcher stub -- which spawns the real process and exits within seconds, so
/// the watcher booked a zero-minute session and every such game recorded no
/// playtime at all.
const MAX_EXE_DEPTH: u32 = 4;

/// Recursively collect `.exe` files, filtering out known non-game executables.
fn collect_exes(dir: &Path, out: &mut Vec<(PathBuf, u64)>, depth: u32) {
    // Limit recursion depth to avoid traversing massive directory trees
    if depth > MAX_EXE_DEPTH || out.len() >= MAX_EXE_CANDIDATES {
        return;
    }

    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        if out.len() >= MAX_EXE_CANDIDATES {
            return;
        }
        let path = entry.path();
        if path.is_dir() {
            collect_exes(&path, out, depth + 1);
        } else if let Some(ext) = path.extension() {
            if ext.eq_ignore_ascii_case("exe") {
                let file_name_lower = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_lowercase())
                    .unwrap_or_default();

                let is_skip = SKIP_PATTERNS
                    .iter()
                    .any(|pat| file_name_lower.contains(pat));

                if !is_skip {
                    let size = entry.metadata().map_or(0, |m| m.len());
                    out.push((path, size));
                }
            }
        }
    }
}
