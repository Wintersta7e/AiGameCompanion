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
    scan_steam_dir(&steam_dir)
}

/// Scan every library of the Steam install at `steam_dir`.
pub(crate) fn scan_steam_dir(steam_dir: &SteamDir) -> ScanOutcome {
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

        // A manifest that cannot be read (Steam rewriting it, or truncated by
        // a crash) or has no name yet still belongs to an installed game, so
        // skipping it makes the scan partial too.
        for app_result in library.apps() {
            let app = match app_result {
                Ok(app) => app,
                Err(e) => {
                    tracing::warn!("Failed to read a Steam app manifest: {e}");
                    complete = false;
                    continue;
                }
            };

            let name = match &app.name {
                Some(n) if !n.is_empty() => n.clone(),
                _ => {
                    tracing::warn!("Steam app {} has no name yet", app.app_id);
                    complete = false;
                    continue;
                }
            };

            let install_dir = library.resolve_app_dir(&app);
            // An empty `installdir` resolves to `steamapps/common` itself, under
            // which every game's exe would match this one.
            let install_exists = !app.install_dir.trim().is_empty() && install_dir.exists();
            let app_id = app.app_id;

            let cover_art_path = Some(format!("{STEAM_CDN}/{app_id}/library_600x900_2x.jpg"));
            let install_path = if install_exists {
                Some(install_dir.to_string_lossy().into_owned())
            } else {
                None
            };

            let game = Game {
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
                unknown: crate::models::UnknownFields::new(),
            };
            // The same manifest in two libraries (a stale copy left after
            // moving the install, a library listed twice) is one game, the one
            // whose install dir exists: two games under one id stop the
            // page's keyed game list from rendering.
            match games.iter_mut().find(|g| g.id == game.id) {
                Some(seen) if seen.install_dir.is_none() && game.install_dir.is_some() => {
                    *seen = game;
                }
                Some(_) => {}
                None => games.push(game),
            }
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
            .map_or_default(|n| n.to_string_lossy().into_owned());
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
        } else if let Some(ext) = path.extension()
            && ext.eq_ignore_ascii_case("exe")
        {
            let file_name_lower = path
                .file_stem()
                .map_or_default(|s| s.to_string_lossy().to_lowercase());

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

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;
    use std::path::{Path, PathBuf};

    use super::scan_steam_dir;

    const MANIFEST_10: &str = "\"AppState\"\n{\n\t\"appid\"\t\t\"10\"\n\t\"installdir\"\t\t\"Ten\"\n\t\"name\"\t\t\"Ten\"\n}\n";

    /// A fake Steam install at `<tmp>/<label>/steam` whose libraries are the
    /// install itself and each of `others`, every one holding `manifest`.
    fn fake_libraries(label: &str, others: &[&str], manifest: &str) -> (PathBuf, PathBuf) {
        let root =
            std::env::temp_dir().join(format!("aigc_steam_scan_{}_{label}", std::process::id()));
        drop(std::fs::remove_dir_all(&root));
        let steam = root.join("steam");
        let libraries: Vec<PathBuf> = std::iter::once(steam.clone())
            .chain(others.iter().map(|name| root.join(name)))
            .collect();
        let escape = |p: &Path| p.display().to_string().replace('\\', "\\\\");
        let mut vdf = String::from("\"libraryfolders\"\n{\n");
        for (n, library) in libraries.iter().enumerate() {
            std::fs::create_dir_all(library.join("steamapps")).unwrap();
            std::fs::write(
                library.join("steamapps").join("appmanifest_10.acf"),
                manifest,
            )
            .unwrap();
            write!(
                vdf,
                "\t\"{n}\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t}}\n",
                escape(library)
            )
            .unwrap();
        }
        vdf.push_str("}\n");
        std::fs::write(steam.join("steamapps").join("libraryfolders.vdf"), vdf).unwrap();
        (root, steam)
    }

    /// The same manifest in two libraries (a stale copy after moving the
    /// install) is one game, at the library where it is installed: two games
    /// under one id stop the page's keyed game list from rendering.
    #[test]
    fn one_game_per_app_id_where_it_is_installed() {
        let (root, steam) = fake_libraries("two_libraries", &["moved"], MANIFEST_10);
        let installed = root
            .join("moved")
            .join("steamapps")
            .join("common")
            .join("Ten");
        std::fs::create_dir_all(&installed).unwrap();

        let outcome = scan_steam_dir(&steamlocate::SteamDir::from_dir(&steam).unwrap());
        drop(std::fs::remove_dir_all(&root));

        let found: Vec<(&str, Option<&str>)> = outcome
            .games
            .iter()
            .map(|g| (g.id.as_str(), g.install_dir.as_deref()))
            .collect();
        println!("scanned: {found:?}");
        let installed = installed.to_string_lossy();
        assert_eq!(found, [("steam_10", Some(&*installed))]);
        assert!(outcome.complete);
    }

    /// A manifest whose `installdir` is empty names no folder: never the
    /// library's `steamapps/common` itself, where every game's exe would match.
    #[test]
    fn an_empty_installdir_gives_no_install_dir() {
        let manifest = "\"AppState\"\n{\n\t\"appid\"\t\t\"10\"\n\t\"installdir\"\t\t\"\"\n\t\"name\"\t\t\"Ten\"\n}\n";
        let (root, steam) = fake_libraries("empty_installdir", &[], manifest);
        std::fs::create_dir_all(steam.join("steamapps").join("common")).unwrap();

        let outcome = scan_steam_dir(&steamlocate::SteamDir::from_dir(&steam).unwrap());
        drop(std::fs::remove_dir_all(&root));

        let found: Vec<(&str, Option<&str>)> = outcome
            .games
            .iter()
            .map(|g| (g.id.as_str(), g.install_dir.as_deref()))
            .collect();
        assert_eq!(found, [("steam_10", None)]);
    }
}
