use tauri::Emitter;
use tauri::Manager;
use tauri::State;
use tauri_plugin_opener::OpenerExt;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::discovery;
use crate::discovery::steam::ScanOutcome;
use crate::models::{Game, GameSource};
use crate::state::AppState;

#[tauri::command]
#[expect(
    clippy::needless_pass_by_value,
    reason = "a Tauri command receives its arguments by value"
)]
pub(crate) fn get_games(state: State<'_, AppState>) -> Vec<Game> {
    let launcher = state.launcher.lock();
    launcher.games.clone()
}

/// Fold a Steam scan result into the stored library, always carrying
/// `play_time`, `last_played` and the fields this build does not know across.
///
/// The scan speaks only for Steam games, so entries from any other source are
/// always kept. For Steam games a COMPLETE scan is authoritative: entries it
/// omits really are uninstalled. A PARTIAL scan is not -- Steam was
/// unreachable, or a library on an unmounted drive failed to read -- so stored
/// Steam games it does not mention are kept. Dropping either kind permanently
/// erases its playtime, unattended, because `scan_on_startup` defaults to true.
fn merge_scan(existing: &[Game], outcome: ScanOutcome) -> Vec<Game> {
    let ScanOutcome {
        mut games,
        complete,
    } = outcome;

    for new_game in &mut games {
        if let Some(prev) = existing.iter().find(|g| g.id == new_game.id) {
            new_game.last_played.clone_from(&prev.last_played);
            new_game.play_time_minutes = prev.play_time_minutes;
            new_game.unknown.clone_from(&prev.unknown);
        }
    }

    let scanned: std::collections::HashSet<&str> = games.iter().map(|g| g.id.as_str()).collect();
    let mut kept: Vec<Game> = existing
        .iter()
        .filter(|g| !scanned.contains(g.id.as_str()))
        .filter(|g| g.source != GameSource::Steam || !complete)
        .cloned()
        .collect();
    let unreached = kept
        .iter()
        .filter(|g| g.source == GameSource::Steam)
        .count();
    if unreached > 0 {
        tracing::warn!(
            "Partial Steam scan -- keeping {unreached} stored game(s) the scan did not reach"
        );
    }
    if !kept.is_empty() {
        games.append(&mut kept);
        games.sort_by_key(|g| g.name.to_lowercase());
    }

    games
}

#[tauri::command]
pub(crate) async fn scan_games(app: tauri::AppHandle) -> Result<Vec<Game>, String> {
    tracing::info!("scan_games: starting Steam discovery");
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let result = discovery::steam::discover_steam_games();
        if tx.send(result).is_err() {
            tracing::warn!("Steam scan finished after the caller went away");
        }
    });
    let outcome = rx.await.map_err(|e| format!("Scan task failed: {e}"))?;
    tracing::info!(
        "scan_games: found {} games (complete: {})",
        outcome.games.len(),
        outcome.complete
    );
    Ok(store_scan(&app, outcome))
}

/// Fold `outcome` into the library and save it; the games the page shows.
fn store_scan<R: tauri::Runtime>(app: &tauri::AppHandle<R>, outcome: ScanOutcome) -> Vec<Game> {
    let state = app.state::<AppState>();
    let mut launcher = state.launcher.lock();
    launcher.games = merge_scan(&launcher.games, outcome);
    let games = launcher.games.clone();
    drop(launcher);
    state.save_or_notify(app, "The scanned library");
    games
}

#[tauri::command]
pub(crate) async fn launch_game(game_id: String, app: tauri::AppHandle) -> Result<String, String> {
    let token = crate::process_watch::reserve(&app.state::<AppState>(), &game_id)?;

    match do_launch(&app, &game_id, token) {
        Ok(()) => Ok("launching".to_string()),
        Err(e) => {
            // Release the reservation so the game can be launched again.
            crate::process_watch::release(&app.state::<AppState>(), &game_id, token);
            Err(e)
        }
    }
}

/// Launch the game and attach the playtime watcher. The caller has already
/// reserved `game_id` as the launch `token`; on `Err` the caller releases it,
/// and on the no-watcher path this releases it after emitting a terminal event.
fn do_launch(app: &tauri::AppHandle, game_id: &str, token: u64) -> Result<(), String> {
    let state = app.state::<AppState>();

    let game = {
        let launcher = state.launcher.lock();
        launcher
            .games
            .iter()
            .find(|g| g.id == game_id)
            .cloned()
            .ok_or_else(|| format!("Game not found: {game_id}"))?
    };

    // Launch via Steam protocol URL for Steam games
    if game.source == GameSource::Steam {
        let source_id = game
            .source_id
            .as_ref()
            .ok_or_else(|| format!("Missing Steam app ID for game: {}", game.name))?;
        if source_id.chars().all(|c| c.is_ascii_digit()) && !source_id.is_empty() {
            let url = format!("steam://rungameid/{source_id}");
            app.opener()
                .open_url(&url, None::<&str>)
                .map_err(|e| format!("Failed to launch: {e}"))?;
        } else {
            return Err(format!("Invalid source_id: {source_id}"));
        }
    } else {
        // For non-Steam games, launch via exe_path directly
        if let Some(exe_path) = &game.exe_path {
            let path = std::path::Path::new(exe_path);
            // Validate the exe path exists and has an .exe extension
            if !path.exists() {
                return Err(format!("Executable not found: {exe_path}"));
            }
            if path.extension().and_then(|e| e.to_str()) != Some("exe") {
                return Err(format!("Invalid executable: {exe_path}"));
            }
            app.opener()
                .open_path(exe_path, None::<&str>)
                .map_err(|e| format!("Failed to launch: {e}"))?;
        } else {
            return Err(format!("No executable path for game: {}", game.name));
        }
    }

    after_start(app, &game, token);
    Ok(())
}

/// The game `game` was started as the launch `token`: record the launch, then
/// attach the session watcher. The last played time goes first because the
/// page reloads the library on the `game-finished` that ends an unwatched
/// launch.
fn after_start<R: tauri::Runtime>(app: &tauri::AppHandle<R>, game: &Game, token: u64) {
    let state = app.state::<AppState>();
    let game_id = game.id.as_str();
    record_launch(app, game_id, &game.name);
    // Attach the session watcher. Steam games are watched via Steam's own
    // running-flag (keyed by appid): authoritative, and no exe guessing. The
    // launch branch above already validated the appid is present + all-digits.
    if game.source == GameSource::Steam {
        let app_id = game.source_id.clone().unwrap_or_default();
        crate::process_watch::spawn_steam_watch(app.clone(), game_id.to_owned(), app_id, token);
    } else {
        // Non-Steam: watch by executable name, resolving it on demand if needed.
        let mut exe_name = game.exe_name.clone();
        if exe_name.is_empty()
            && let Some(dir) = &game.install_dir
        {
            let (resolved_name, resolved_path) =
                discovery::steam::resolve_game_exe(std::path::Path::new(dir));
            exe_name = resolved_name;
            // Cache the resolved exe for next time.
            let mut launcher = state.launcher.lock();
            if let Some(g) = launcher.games.iter_mut().find(|g| g.id == game_id) {
                g.exe_name.clone_from(&exe_name);
                g.exe_path = resolved_path;
            }
            drop(launcher);
            state.save_or_notify(app, &format!("The executable found for {}", game.name));
        }
        // No process name to watch -- reset to idle (the game did launch).
        if exe_name.is_empty() {
            crate::util::log_if_err("emit game-finished", app.emit("game-finished", game_id));
            crate::process_watch::release(&state, game_id, token);
        } else {
            crate::process_watch::spawn_game_watch(
                app.clone(),
                game_id.to_owned(),
                exe_name,
                token,
            );
        }
    }
}

/// Set the last played time of `game_id`, named `name`, to now and save it.
/// The local offset cannot be read on a multi-threaded Unix process; UTC reads
/// the same to the frontend.
fn record_launch<R: tauri::Runtime>(app: &tauri::AppHandle<R>, game_id: &str, name: &str) {
    let state = app.state::<AppState>();
    {
        let now = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
        let mut launcher = state.launcher.lock();
        if let Some(g) = launcher.games.iter_mut().find(|g| g.id == game_id)
            && let Ok(text) = now.format(&Rfc3339)
        {
            g.last_played = Some(text);
        }
    }
    state.save_or_notify(app, &format!("The last played time of {name}"));
}

#[tauri::command]
#[expect(
    clippy::needless_pass_by_value,
    reason = "a Tauri command receives its arguments by value"
)]
pub(crate) fn open_game_logs(app: tauri::AppHandle) -> Result<(), String> {
    let log_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Cannot determine log directory: {e}"))?;
    let log_path = log_dir.join("launcher.log");
    if log_path.exists() {
        app.opener()
            .open_path(log_path.to_string_lossy().as_ref(), None::<&str>)
            .map_err(|e| format!("Failed to open log: {e}"))
    } else {
        Err(format!("launcher.log not found in {}", log_dir.display()))
    }
}

#[cfg(test)]
mod tests {
    #![allow(let_underscore_drop, reason = "test cleanup is best-effort")]

    use super::{ScanOutcome, merge_scan};
    use crate::models::{Game, GameSource};

    fn game(id: &str, name: &str, minutes: u64) -> Game {
        Game {
            id: id.to_owned(),
            name: name.to_owned(),
            source: GameSource::Steam,
            play_time_minutes: minutes,
            last_played: Some("2026-09-15".to_owned()),
            ..Game::default()
        }
    }

    #[test]
    fn complete_scan_carries_playtime_across() {
        let stored = vec![game("steam_1", "One", 300)];
        let merged = merge_scan(
            &stored,
            ScanOutcome {
                games: vec![game("steam_1", "One", 0)],
                complete: true,
            },
        );
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].play_time_minutes, 300);
        assert_eq!(merged[0].last_played.as_deref(), Some("2026-09-15"));
    }

    #[test]
    fn complete_scan_drops_uninstalled_games() {
        let stored = vec![game("steam_1", "One", 300), game("steam_2", "Two", 50)];
        let merged = merge_scan(
            &stored,
            ScanOutcome {
                games: vec![game("steam_1", "One", 0)],
                complete: true,
            },
        );
        assert_eq!(merged.len(), 1, "a complete scan is authoritative");
        assert_eq!(merged[0].id, "steam_1");
    }

    /// A Steam scan knows nothing of other launchers: a complete one drops the
    /// Steam games it omits, never a game from another source.
    #[test]
    fn complete_scan_keeps_other_sources() {
        let mut manual = game("manual_1", "Alpha", 40);
        manual.source = GameSource::Manual;
        let mut epic = game("epic_1", "Zeta", 20);
        epic.source = GameSource::Epic;
        let stored = vec![
            manual,
            game("steam_1", "One", 300),
            game("steam_2", "Two", 50),
            epic,
        ];
        let merged = merge_scan(
            &stored,
            ScanOutcome {
                games: vec![game("steam_1", "One", 0)],
                complete: true,
            },
        );
        let ids: Vec<&str> = merged.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(ids, ["manual_1", "steam_1", "epic_1"], "sorted by name");
        let minutes: Vec<u64> = merged.iter().map(|g| g.play_time_minutes).collect();
        assert_eq!(minutes, [40, 300, 20]);
    }

    /// The regression that matters: a library on an unavailable drive must not
    /// erase those games or their playtime.
    #[test]
    fn partial_scan_keeps_games_it_did_not_reach() {
        let stored = vec![game("steam_1", "One", 300), game("steam_2", "Two", 50)];
        let merged = merge_scan(
            &stored,
            ScanOutcome {
                games: vec![game("steam_1", "One", 0)],
                complete: false,
            },
        );
        assert_eq!(merged.len(), 2, "partial scan must not drop stored games");
        let two = merged.iter().find(|g| g.id == "steam_2").unwrap();
        assert_eq!(two.play_time_minutes, 50);
    }

    /// Steam entirely unreachable: the scan is empty AND partial, which is the
    /// startup case that silently wiped the library.
    #[test]
    fn empty_partial_scan_keeps_the_whole_library() {
        let stored = vec![game("steam_1", "One", 300), game("steam_2", "Two", 50)];
        let merged = merge_scan(
            &stored,
            ScanOutcome {
                games: Vec::new(),
                complete: false,
            },
        );
        assert_eq!(merged.len(), 2);
        assert_eq!(merged.iter().map(|g| g.play_time_minutes).sum::<u64>(), 350);
    }

    #[test]
    fn empty_complete_scan_clears_the_library() {
        let stored = vec![game("steam_1", "One", 300)];
        let merged = merge_scan(
            &stored,
            ScanOutcome {
                games: Vec::new(),
                complete: true,
            },
        );
        assert!(merged.is_empty(), "a clean scan finding nothing is truth");
    }

    /// A Steam install under the temp dir with one library holding a readable
    /// manifest for app 10 and `second_manifest` as app 20's manifest.
    fn fake_steam_dir(label: &str, second_manifest: &str) -> std::path::PathBuf {
        let steam =
            std::env::temp_dir().join(format!("aigc_fake_steam_{}_{label}", std::process::id()));
        let _ = std::fs::remove_dir_all(&steam);
        let apps = steam.join("steamapps");
        std::fs::create_dir_all(&apps).unwrap();
        let library = steam.display().to_string().replace('\\', "\\\\");
        std::fs::write(
            apps.join("libraryfolders.vdf"),
            format!(
                "\"libraryfolders\"\n{{\n\t\"0\"\n\t{{\n\t\t\"path\"\t\t\"{library}\"\n\t}}\n}}\n"
            ),
        )
        .unwrap();
        std::fs::write(
            apps.join("appmanifest_10.acf"),
            "\"AppState\"\n{\n\t\"appid\"\t\t\"10\"\n\t\"installdir\"\t\t\"Ten\"\n\t\"name\"\t\t\"Ten\"\n}\n",
        )
        .unwrap();
        std::fs::write(apps.join("appmanifest_20.acf"), second_manifest).unwrap();
        steam
    }

    /// Scan the fake install and fold the result into a library where both
    /// games already have playtime.
    fn scan_over_stored(steam: &std::path::Path) -> Vec<Game> {
        let outcome = crate::discovery::steam::scan_steam_dir(
            &steamlocate::SteamDir::from_dir(steam).unwrap(),
        );
        let stored = vec![
            game("steam_10", "Ten", 300),
            game("steam_20", "Twenty", 5000),
        ];
        let merged = merge_scan(&stored, outcome);
        let _ = std::fs::remove_dir_all(steam);
        merged
    }

    /// A manifest Steam is rewriting, or one truncated by a crash, cannot be
    /// read: that game is still installed, so its playtime must survive.
    #[test]
    fn unreadable_manifest_keeps_the_stored_game() {
        let merged = scan_over_stored(&fake_steam_dir("unreadable", ""));
        let twenty = merged.iter().find(|g| g.id == "steam_20");
        assert_eq!(
            twenty.map(|g| g.play_time_minutes),
            Some(5000),
            "a game with an unreadable manifest lost its playtime: {merged:?}"
        );
    }

    /// Steam writes a manifest without a name for a queued install.
    #[test]
    fn nameless_manifest_keeps_the_stored_game() {
        let nameless =
            "\"AppState\"\n{\n\t\"appid\"\t\t\"20\"\n\t\"installdir\"\t\t\"Twenty\"\n}\n";
        let merged = scan_over_stored(&fake_steam_dir("nameless", nameless));
        let twenty = merged.iter().find(|g| g.id == "steam_20");
        assert_eq!(
            twenty.map(|g| g.play_time_minutes),
            Some(5000),
            "a game with a nameless manifest lost its playtime: {merged:?}"
        );
    }

    /// A scan rebuilds each Steam game it finds; a field a newer build stored
    /// on that game must come through the rebuild.
    #[test]
    fn scan_keeps_fields_this_build_does_not_know() {
        let twenty = "\"AppState\"\n{\n\t\"appid\"\t\t\"20\"\n\t\"installdir\"\t\t\"Twenty\"\n\t\"name\"\t\t\"Twenty\"\n}\n";
        let steam = fake_steam_dir("unknown_fields", twenty);
        let outcome = crate::discovery::steam::scan_steam_dir(
            &steamlocate::SteamDir::from_dir(&steam).unwrap(),
        );
        let _ = std::fs::remove_dir_all(&steam);
        let stored: Vec<Game> = serde_json::from_str(
            r#"[{"id":"steam_10","name":"Ten","source":"steam","source_id":"10","play_time_minutes":300,"notes":"my notes"}]"#,
        )
        .unwrap();
        let merged = merge_scan(&stored, outcome);
        let ten = merged.iter().find(|g| g.id == "steam_10").unwrap();
        let saved = serde_json::to_value(ten).unwrap();
        println!("after the scan: {saved}");
        assert_eq!(saved["notes"], "my notes", "lost in the scan: {saved}");
        assert_eq!(saved["play_time_minutes"], 300);
    }
}

#[cfg(test)]
mod event_order_tests {
    use tauri::{Listener as _, Manager as _};

    use super::after_start;
    use crate::models::{Game, GameSource};
    use crate::state::AppState;

    /// A launched game with no process to watch tells the page it ended; the
    /// page then reloads the library, so the last played time must be in the
    /// state by then.
    #[test]
    fn game_finished_follows_the_last_played_time() {
        let dir =
            std::env::temp_dir().join(format!("aigc_launcher_event_order_{}", std::process::id()));
        drop(std::fs::remove_dir_all(&dir));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("launcher-state.json");
        std::fs::write(
            &path,
            r#"{"games":[{"id":"g1","name":"Foo","source":"manual"}],"settings":{}}"#,
        )
        .unwrap();
        let app = tauri::test::mock_app();
        app.manage(AppState::load(path));
        let handle = app.handle().clone();
        let seen = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
        let record = std::sync::Arc::clone(&seen);
        app.listen_any("game-finished", move |_| {
            let launcher = handle.state::<AppState>().launcher.lock().games[0]
                .last_played
                .clone();
            record.lock().push(launcher);
        });
        let token = crate::process_watch::reserve(&app.state::<AppState>(), "g1").unwrap();
        let game = Game {
            id: "g1".to_owned(),
            name: "Foo".to_owned(),
            source: GameSource::Manual,
            ..Game::default()
        };

        after_start(app.handle(), &game, token);
        drop(std::fs::remove_dir_all(&dir));

        let seen = std::mem::take(&mut *seen.lock());
        assert_eq!(seen.len(), 1, "game-finished was not emitted once");
        assert!(
            seen[0].is_some(),
            "the page reloads before the last played time is set"
        );
        assert!(app.state::<AppState>().active_sessions.lock().is_empty());
    }
}

#[cfg(test)]
mod failed_save_tests {
    use super::{ScanOutcome, record_launch, store_scan};
    use crate::state::tests::failing_saves;
    use std::time::Duration;

    const ONE_GAME: &str =
        r#"{"games":[{"id":"g1","name":"Foo","source":"manual"}],"settings":{}}"#;

    /// The library a scan found is shown, and the page learns it was not saved.
    #[test]
    fn a_failed_scan_save_reaches_the_page() {
        let saves = failing_saves("scan", ONE_GAME);

        let games = store_scan(
            saves.app.handle(),
            ScanOutcome {
                games: Vec::new(),
                complete: true,
            },
        );

        assert_eq!(games.len(), 1, "a manual game is kept by every scan");
        let reported = saves.reported.recv_timeout(Duration::from_secs(5));
        println!("reported: {reported:?}");
        assert!(
            reported.is_ok_and(
                |message| message.starts_with("The scanned library could not be saved: ")
            ),
            "the page was not told the scan was not saved"
        );
    }

    #[test]
    fn a_failed_last_played_save_reaches_the_page() {
        let saves = failing_saves("launch", ONE_GAME);

        record_launch(saves.app.handle(), "g1", "Foo");

        let reported = saves.reported.recv_timeout(Duration::from_secs(5));
        println!("reported: {reported:?}");
        assert!(
            reported
                .is_ok_and(|message| message
                    .starts_with("The last played time of Foo could not be saved: ")),
            "the page was not told the launch was not saved"
        );
    }
}
