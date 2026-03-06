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

/// Cover art filename suffixes to try, in priority order.
const COVER_ART_SUFFIXES: &[&str] = &[
    "_library_600x900.jpg",
    "_library_600x900_2x.jpg",
    "_header.jpg",
];

/// Discover installed Steam games.
///
/// Returns an alphabetically sorted list of games found across all Steam libraries.
pub fn discover_steam_games() -> Vec<Game> {
    let steam_dir = match SteamDir::locate() {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!("Failed to locate Steam: {e}");
            return Vec::new();
        }
    };

    let steam_path = steam_dir.path().to_path_buf();

    let libraries = match steam_dir.libraries() {
        Ok(iter) => iter,
        Err(e) => {
            eprintln!("Failed to read Steam libraries: {e}");
            return Vec::new();
        }
    };

    let mut games: Vec<Game> = Vec::new();

    for library_result in libraries {
        let library = match library_result {
            Ok(lib) => lib,
            Err(e) => {
                eprintln!("Failed to read Steam library: {e}");
                continue;
            }
        };

        for app_result in library.apps() {
            let app = match app_result {
                Ok(a) => a,
                Err(_) => continue,
            };

            let name = match &app.name {
                Some(n) if !n.is_empty() => n.clone(),
                _ => continue,
            };

            let install_dir = library.resolve_app_dir(&app);
            if !install_dir.exists() {
                continue;
            }

            let app_id = app.app_id;

            let cover_art_path = find_cover_art(&steam_path, app_id);
            let (exe_name, exe_path) = find_main_exe(&install_dir);

            games.push(Game {
                id: format!("steam_{app_id}"),
                name,
                source: GameSource::Steam,
                source_id: Some(app_id.to_string()),
                exe_name,
                exe_path,
                cover_art_path,
                last_played: None,
                play_time_minutes: 0,
            });
        }
    }

    games.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    games
}

/// Look for cover art in Steam's library cache.
fn find_cover_art(steam_path: &Path, app_id: u32) -> Option<String> {
    let cache_dir = steam_path.join("appcache").join("librarycache");
    for suffix in COVER_ART_SUFFIXES {
        let path = cache_dir.join(format!("{app_id}{suffix}"));
        if path.exists() {
            return Some(path.to_string_lossy().into_owned());
        }
    }
    None
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
    candidates.sort_by(|a, b| b.1.cmp(&a.1));

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

/// Recursively collect `.exe` files, filtering out known non-game executables.
fn collect_exes(dir: &Path, out: &mut Vec<(PathBuf, u64)>, depth: u32) {
    // Limit recursion depth to avoid traversing massive directory trees
    if depth > 4 {
        return;
    }

    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
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
                    let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                    out.push((path, size));
                }
            }
        }
    }
}
