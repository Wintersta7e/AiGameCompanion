use std::collections::HashSet;
use std::io::{ErrorKind, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use parking_lot::Mutex;

use crate::models::{LauncherSettings, LauncherState};

/// Reads of the state file, in total, before it counts as unreadable: about a
/// second, for a scanner that holds the file open without read sharing.
const READ_ATTEMPTS: u32 = 5;
const READ_RETRY_DELAY: Duration = Duration::from_millis(200);
/// Backups go to `.json.bak`, then `.json.bak.1` ... `.json.bak.<this>`.
const MAX_BACKUP_SUFFIX: u32 = 99;

pub(crate) struct AppState {
    pub launcher: Mutex<LauncherState>,
    pub state_path: PathBuf,
    /// Game ids with an active play session (a running process being watched).
    /// Guards against launching the same game twice.
    pub active_sessions: Mutex<HashSet<String>>,
    /// Serializes `save()` so the watcher thread and command threads cannot
    /// interleave writes to the shared temp file.
    save_lock: Mutex<()>,
    /// Why this run must not write the state file: set when the file found at
    /// load was neither fully loaded nor preserved. `None` means writable.
    load_error: Option<String>,
}

impl AppState {
    pub(crate) fn load(state_path: PathBuf) -> Self {
        // Recover from an interrupted atomic write (a .tmp left behind), but
        // only over a state file that is really absent. `Path::exists()` is
        // false for any metadata error too, and renaming over a file that is
        // merely unreadable would destroy it.
        let tmp_path = state_path.with_extension("json.tmp");
        let probe = std::fs::metadata(&state_path);
        if should_promote_tmp(probe.err().map(|e| e.kind()), tmp_path.exists()) {
            crate::util::log_if_err(
                "recover the interrupted state write",
                std::fs::rename(&tmp_path, &state_path),
            );
        }

        let (launcher, load_error) = read_state(&state_path);
        if let Some(reason) = &load_error {
            tracing::error!("Launcher state is read-only this run: {reason}");
        }
        Self {
            launcher: Mutex::new(launcher),
            state_path,
            active_sessions: Mutex::new(HashSet::new()),
            save_lock: Mutex::new(()),
            load_error,
        }
    }

    /// Why saving is refused this run, or `None` when the state is writable.
    pub(crate) fn load_error(&self) -> Option<&str> {
        self.load_error.as_deref()
    }

    pub(crate) fn save(&self) -> Result<(), String> {
        // Read-only: the file on disk was neither fully loaded nor preserved,
        // so anything written now would replace data this run never saw.
        if let Some(reason) = self.load_error() {
            return Err(reason.to_owned());
        }
        // Serialize concurrent saves so they cannot clobber each other's temp file.
        let _write = self.save_lock.lock();
        // Clone state and drop lock before file I/O
        let state = self.launcher.lock().clone();
        let json = serde_json::to_string_pretty(&state).map_err(|e| e.to_string())?;
        // Atomic write: write to temp file, flush it to disk, then rename.
        // Without the fsync the rename can commit while the tmp file's data is
        // still dirty, so a power loss leaves a directory entry pointing at
        // unwritten bytes -- and the next start would back that garbage up as
        // the "last good" copy.
        let tmp_path = self.state_path.with_extension("json.tmp");
        {
            let mut file = std::fs::File::create(&tmp_path).map_err(|e| e.to_string())?;
            file.write_all(json.as_bytes()).map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
        }
        std::fs::rename(&tmp_path, &self.state_path).map_err(|e| e.to_string())
    }

    /// The one write path for a settings setter. Refuses in read-only mode
    /// before changing memory, so a refused value is never in effect; else
    /// applies `edit` in one lock hold, releases it and saves.
    pub(crate) fn edit_settings(
        &self,
        edit: impl FnOnce(&mut LauncherSettings),
    ) -> Result<(), String> {
        if let Some(reason) = self.load_error() {
            return Err(reason.to_owned());
        }
        edit(&mut self.launcher.lock().settings);
        self.save()
    }
}

/// Read the state file and decide whether this run may write it. Returns the
/// state to run on and, when the file must be left alone, the reason.
///
/// Writable: parsed with nothing dropped; absent; or corrupt / partly
/// unreadable with its bytes now in a backup. Read-only: unreadable after every
/// retry, or corrupt / partly unreadable with no backup written.
fn read_state(state_path: &Path) -> (LauncherState, Option<String>) {
    let contents = match read_with_retry(state_path, READ_ATTEMPTS, READ_RETRY_DELAY, |path| {
        std::fs::read_to_string(path)
    }) {
        Ok(contents) => contents,
        Err(e) if e.kind() == ErrorKind::NotFound => return (LauncherState::default(), None),
        Err(e) => {
            let reason = format!("could not read {}: {e}", state_path.display());
            return (LauncherState::default(), Some(reason));
        }
    };

    match serde_json::from_str::<LauncherState>(&contents) {
        Ok(state) => {
            // One malformed game or per-game entry is dropped rather than
            // failing the whole file, so a parse can succeed and still lose data.
            let dropped = dropped_entries(&contents, &state);
            if dropped == 0 {
                return (state, None);
            }
            tracing::warn!("{dropped} unreadable entries in {}", state_path.display());
            let reason = back_up(state_path, contents.as_bytes()).err().map(|e| {
                format!(
                    "{dropped} entries in {} could not be read, and it could not be backed up: {e}",
                    state_path.display()
                )
            });
            (state, reason)
        }
        Err(parse_error) => {
            tracing::error!(
                "Corrupt launcher state at {}: {parse_error}",
                state_path.display()
            );
            let reason = back_up(state_path, contents.as_bytes()).err().map(|e| {
                format!(
                    "{} is not valid ({parse_error}), and it could not be backed up: {e}",
                    state_path.display()
                )
            });
            (LauncherState::default(), reason)
        }
    }
}

/// Call `read` up to `attempts` times, `delay` apart, until it succeeds or
/// fails with `NotFound` -- an absent file will not appear by waiting, but a
/// lock held by another process usually goes away.
fn read_with_retry(
    path: &Path,
    attempts: u32,
    delay: Duration,
    mut read: impl FnMut(&Path) -> std::io::Result<String>,
) -> std::io::Result<String> {
    let mut attempt = 1;
    loop {
        match read(path) {
            Err(e) if e.kind() != ErrorKind::NotFound && attempt < attempts => {
                tracing::warn!(
                    "Reading {} failed (attempt {attempt} of {attempts}): {e}",
                    path.display()
                );
                std::thread::sleep(delay);
                attempt += 1;
            }
            result => return result,
        }
    }
}

/// Whether a leftover `.tmp` may become the state file: only when probing the
/// state file said it does not exist. Any other probe error means a file may be
/// there that could not be read.
const fn should_promote_tmp(state_error: Option<ErrorKind>, tmp_exists: bool) -> bool {
    tmp_exists && matches!(state_error, Some(ErrorKind::NotFound))
}

/// Entries the load dropped as unreadable: the file's `games` beyond the loaded
/// games, plus its `game_prefs` entries beyond the loaded ones, where a
/// `game_prefs` that is not an object counts as one.
fn dropped_entries(contents: &str, state: &LauncherState) -> usize {
    let Ok(raw) = serde_json::from_str::<serde_json::Value>(contents) else {
        return 0;
    };
    let games = raw
        .get("games")
        .and_then(serde_json::Value::as_array)
        .map_or(0, Vec::len)
        .saturating_sub(state.games.len());
    let prefs = raw.get("game_prefs").map_or(0, |value| {
        value.as_object().map_or(1, |entries| {
            entries.len().saturating_sub(state.game_prefs.len())
        })
    });
    games + prefs
}

/// Write `bytes` -- what the load already read, never a second read of a file
/// that may not be readable -- to a backup name that does not exist yet:
/// `.json.bak`, else the first free `.json.bak.1` ... `.json.bak.99`. Never
/// replaces an existing file, which may be the only copy of an older library.
fn back_up(state_path: &Path, bytes: &[u8]) -> std::io::Result<PathBuf> {
    let mut last_error = None;
    for n in 0..=MAX_BACKUP_SUFFIX {
        let candidate = if n == 0 {
            state_path.with_extension("json.bak")
        } else {
            state_path.with_extension(format!("json.bak.{n}"))
        };
        let mut file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => file,
            Err(e) => {
                last_error = Some(e);
                continue;
            }
        };
        if let Err(e) = file.write_all(bytes).and_then(|()| file.sync_all()) {
            drop(file);
            crate::util::log_if_err(
                "remove the incomplete backup",
                std::fs::remove_file(&candidate),
            );
            return Err(e);
        }
        tracing::warn!(
            "Kept a copy of the launcher state at {}",
            candidate.display()
        );
        return Ok(candidate);
    }
    Err(last_error.unwrap_or_else(|| std::io::Error::other("no backup name was tried")))
}

#[cfg(test)]
mod tests {
    #![allow(let_underscore_drop, reason = "test cleanup is best-effort")]

    use super::*;
    use crate::models::{Game, GamePrefs, GameSource};
    use std::path::Path;

    /// Unique temp path per test (process id + label) so parallel tests
    /// never collide on the same file. Cleans any leftover first.
    fn temp_state_path(label: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "aigc_launcher_state_{}_{label}.json",
            std::process::id()
        ));
        cleanup(&p);
        p
    }

    fn cleanup(path: &Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_dir(path);
        let _ = std::fs::remove_file(path.with_extension("json.tmp"));
        for name in backup_names(path) {
            let _ = std::fs::remove_file(&name);
            let _ = std::fs::remove_dir(&name);
        }
    }

    /// `.json.bak`, then `.json.bak.1` ... `.json.bak.99`.
    fn backup_names(path: &Path) -> Vec<PathBuf> {
        std::iter::once(path.with_extension("json.bak"))
            .chain((1..=99).map(|n| path.with_extension(format!("json.bak.{n}"))))
            .collect()
    }

    /// A directory at every backup name, so no backup can be written.
    fn occupy_every_backup_name(path: &Path) {
        for name in backup_names(path) {
            std::fs::create_dir(&name).unwrap();
        }
    }

    const ONE_GAME: &str =
        r#"{"games":[{"id":"g1","name":"Foo","play_time_minutes":900}],"settings":{}}"#;

    #[test]
    fn load_returns_default_when_file_absent() {
        let path = temp_state_path("absent");
        let app = AppState::load(path.clone());
        let st = app.launcher.lock();
        assert!(st.games.is_empty());
        assert!(
            st.settings.scan_on_startup,
            "default scan_on_startup is true"
        );
        drop(st);
        cleanup(&path);
    }

    #[test]
    fn save_then_load_round_trips_games() {
        let path = temp_state_path("round_trip");
        let app = AppState::load(path.clone());
        app.launcher.lock().games.push(Game {
            id: "g1".to_owned(),
            name: "Test Game".to_owned(),
            ..Default::default()
        });
        app.save().unwrap();

        let reloaded = AppState::load(path.clone());
        let st = reloaded.launcher.lock();
        assert_eq!(st.games.len(), 1);
        assert_eq!(st.games[0].id, "g1");
        assert_eq!(st.games[0].name, "Test Game");
        drop(st);
        cleanup(&path);
    }

    #[test]
    fn load_fills_defaults_for_fields_missing_in_old_json() {
        let path = temp_state_path("schema_evo");
        // Older on-disk shape: the game lacks play_time_minutes/source and the
        // settings object lacks launch_on_startup. Game and LauncherSettings
        // are #[serde(default)], so missing fields fall back to defaults rather
        // than wiping the whole state.
        let json = r#"{"games":[{"id":"g1","name":"Old","exe_name":"g.exe"}],"settings":{"scan_on_startup":false}}"#;
        std::fs::write(&path, json).unwrap();

        let app = AppState::load(path.clone());
        let st = app.launcher.lock();
        assert_eq!(st.games.len(), 1);
        assert_eq!(st.games[0].play_time_minutes, 0); // defaulted
        assert_eq!(st.games[0].source, GameSource::Manual); // defaulted
        assert!(!st.settings.scan_on_startup); // explicit value preserved
        assert!(!st.settings.launch_on_startup); // defaulted to false
        assert!(st.settings.minimize_to_tray); // defaulted to true
        drop(st);
        cleanup(&path);
    }

    /// The previous schema-evolution test supplied BOTH top-level keys, so it
    /// only ever exercised `Game` and `LauncherSettings` -- which were already
    /// protected. The container was not, and that is where a real upgrade
    /// breaks: adding one field made every existing file fail to parse.
    #[test]
    fn load_tolerates_missing_top_level_fields() {
        let path = temp_state_path("toplevel_evo");
        std::fs::write(&path, r#"{"games":[{"id":"g1","name":"Old"}]}"#).unwrap();
        let app = AppState::load(path.clone());
        let st = app.launcher.lock();
        assert_eq!(st.games.len(), 1, "missing `settings` must not lose games");
        assert!(
            st.settings.scan_on_startup,
            "settings fall back to defaults"
        );
        drop(st);
        cleanup(&path);
    }

    #[test]
    fn load_tolerates_unknown_game_source() {
        let path = temp_state_path("unknown_source");
        std::fs::write(
            &path,
            r#"{"games":[{"id":"g1","name":"Future","source":"xbox"}],"settings":{}}"#,
        )
        .unwrap();
        let app = AppState::load(path.clone());
        let st = app.launcher.lock();
        assert_eq!(
            st.games.len(),
            1,
            "a newer source must not wipe the library"
        );
        assert_eq!(st.games[0].source, GameSource::Manual);
        drop(st);
        cleanup(&path);
    }

    /// One malformed entry costs that entry, not the whole library.
    #[test]
    fn load_drops_only_the_unreadable_game_entry() {
        let path = temp_state_path("bad_entry");
        std::fs::write(
            &path,
            r#"{"games":[{"id":"ok","name":"Good"},{"id":"bad","play_time_minutes":"lots"}],"settings":{}}"#,
        )
        .unwrap();
        let app = AppState::load(path.clone());
        let st = app.launcher.lock();
        assert_eq!(st.games.len(), 1);
        assert_eq!(st.games[0].id, "ok");
        drop(st);
        cleanup(&path);
    }

    #[test]
    fn corrupt_state_falls_back_to_default_and_backs_up() {
        let path = temp_state_path("corrupt");
        std::fs::write(&path, "{ this is not valid json").unwrap();

        let app = AppState::load(path.clone());
        assert!(
            app.launcher.lock().games.is_empty(),
            "corrupt state resets to default"
        );

        let backup = path.with_extension("json.bak");
        assert!(
            backup.exists(),
            "corrupt state must be backed up before reset"
        );
        assert_eq!(
            std::fs::read_to_string(&backup).unwrap(),
            "{ this is not valid json"
        );
        cleanup(&path);
    }

    #[test]
    fn recovers_state_from_leftover_tmp_when_main_missing() {
        let path = temp_state_path("tmp_recovery");
        let tmp = path.with_extension("json.tmp");
        // Simulate an interrupted atomic write: only the .tmp survived.
        let json = r#"{"games":[{"id":"recovered","name":"R","exe_name":"r.exe"}],"settings":{}}"#;
        std::fs::write(&tmp, json).unwrap();

        let app = AppState::load(path.clone());
        assert!(
            path.exists(),
            "tmp should be promoted to the main state file"
        );
        assert!(!tmp.exists(), "tmp should be renamed away after recovery");
        assert_eq!(app.launcher.lock().games[0].id, "recovered");
        cleanup(&path);
    }

    /// An unreadable file used to load as an empty library, and the next save
    /// replaced it with that empty library.
    #[cfg(unix)]
    #[test]
    fn unreadable_state_is_never_overwritten() {
        use std::os::unix::fs::PermissionsExt;

        let path = temp_state_path("unreadable");
        let original = ONE_GAME;
        std::fs::write(&path, original).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&path).is_ok() {
            println!("skipped: running as root");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            cleanup(&path);
            return;
        }

        let app = AppState::load(path.clone());
        let saved = app.save();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        assert!(saved.is_err(), "save must refuse, got {saved:?}");
        assert!(
            !path.with_extension("json.tmp").exists(),
            "save must not create a .tmp"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        cleanup(&path);
    }

    /// Runs on every platform: a directory where the file should be cannot be
    /// read, and must not be taken for "no state yet".
    #[test]
    fn read_error_makes_state_read_only() {
        let path = temp_state_path("read_error");
        std::fs::create_dir(&path).unwrap();

        let app = AppState::load(path.clone());

        assert!(app.load_error().is_some(), "a read error must be read-only");
        assert!(app.save().is_err());
        assert!(!path.with_extension("json.tmp").exists());
        assert!(path.is_dir());
        cleanup(&path);
    }

    #[test]
    fn corrupt_state_without_backup_is_read_only() {
        let path = temp_state_path("corrupt_no_backup");
        std::fs::write(&path, "{ not json").unwrap();
        occupy_every_backup_name(&path);

        let app = AppState::load(path.clone());

        assert!(
            app.load_error().is_some(),
            "an unpreserved file is read-only"
        );
        assert!(app.save().is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json");
        cleanup(&path);
    }

    #[test]
    fn writable_outcomes_stay_writable() {
        let absent = temp_state_path("writable_absent");
        let parsed = temp_state_path("writable_parsed");
        std::fs::write(&parsed, ONE_GAME).unwrap();
        let corrupt = temp_state_path("writable_corrupt");
        std::fs::write(&corrupt, "{ not json").unwrap();

        for path in [absent, parsed, corrupt] {
            let app = AppState::load(path.clone());
            assert_eq!(app.load_error(), None, "{}", path.display());
            app.save().unwrap();
            cleanup(&path);
        }
    }

    #[test]
    fn read_retries_transient_errors_only() {
        use std::io::{Error, ErrorKind};
        let path = Path::new("unused");

        let mut calls = 0;
        let read = read_with_retry(path, READ_ATTEMPTS, Duration::ZERO, |_| {
            calls += 1;
            if calls <= 2 {
                Err(Error::from(ErrorKind::PermissionDenied))
            } else {
                Ok("content".to_owned())
            }
        });
        assert_eq!(read.unwrap(), "content");
        assert_eq!(calls, 3);

        let mut calls = 0;
        let read = read_with_retry(path, READ_ATTEMPTS, Duration::ZERO, |_| {
            calls += 1;
            Err(Error::from(ErrorKind::NotFound))
        });
        assert_eq!(read.unwrap_err().kind(), ErrorKind::NotFound);
        assert_eq!(calls, 1);

        let mut calls = 0;
        let read = read_with_retry(path, READ_ATTEMPTS, Duration::ZERO, |_| {
            calls += 1;
            Err(Error::other(format!("failure {calls}")))
        });
        assert_eq!(read.unwrap_err().to_string(), "failure 5");
        assert_eq!(calls, 5);
    }

    #[test]
    fn tmp_is_promoted_only_when_state_is_absent() {
        use std::io::ErrorKind;
        assert!(should_promote_tmp(Some(ErrorKind::NotFound), true));
        assert!(!should_promote_tmp(Some(ErrorKind::PermissionDenied), true));
        assert!(!should_promote_tmp(None, true));
        assert!(!should_promote_tmp(Some(ErrorKind::NotFound), false));
    }

    #[test]
    fn dropped_entries_are_preserved() {
        let original = r#"{"games":[{"id":"ok","name":"Good"},{"id":"bad","play_time_minutes":"lots"}],"settings":{}}"#;

        let path = temp_state_path("dropped_backed_up");
        std::fs::write(&path, original).unwrap();
        let app = AppState::load(path.clone());
        assert_eq!(app.launcher.lock().games.len(), 1);
        assert_eq!(app.load_error(), None);
        assert_eq!(
            std::fs::read_to_string(path.with_extension("json.bak")).unwrap(),
            original
        );
        cleanup(&path);

        let path = temp_state_path("dropped_no_backup");
        std::fs::write(&path, original).unwrap();
        occupy_every_backup_name(&path);
        let app = AppState::load(path.clone());
        assert_eq!(app.launcher.lock().games.len(), 1);
        assert!(
            app.load_error().is_some(),
            "an unpreserved file is read-only"
        );
        assert!(app.save().is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        cleanup(&path);
    }

    #[test]
    fn game_prefs_load_save_and_backup() {
        // No record in the file: empty, and the rest loads intact.
        let path = temp_state_path("prefs_absent");
        std::fs::write(&path, ONE_GAME).unwrap();
        let app = AppState::load(path.clone());
        println!(
            "no record: {} dropped",
            dropped_entries(ONE_GAME, &app.launcher.lock())
        );
        let st = app.launcher.lock();
        assert!(st.game_prefs.is_empty());
        assert_eq!(st.games.len(), 1);
        assert_eq!(st.games[0].play_time_minutes, 900);
        assert!(st.settings.scan_on_startup);
        drop(st);
        assert_eq!(app.load_error(), None);
        assert!(!path.with_extension("json.bak").exists());
        cleanup(&path);

        // One readable entry survives a save and a reload unchanged.
        let one = r#"{"game_prefs":{"library:x":{}}}"#;
        let path = temp_state_path("prefs_one");
        std::fs::write(&path, one).unwrap();
        let app = AppState::load(path.clone());
        let loaded = app.launcher.lock().game_prefs.clone();
        println!(
            "one entry: {} dropped, loaded {loaded:?}",
            dropped_entries(one, &app.launcher.lock())
        );
        assert_eq!(loaded.keys().collect::<Vec<_>>(), ["library:x"]);
        assert_eq!(loaded.get("library:x"), Some(&GamePrefs::default()));
        app.save().unwrap();
        assert_eq!(
            AppState::load(path.clone()).launcher.lock().game_prefs,
            loaded
        );
        cleanup(&path);

        // An unreadable entry is dropped, after the file is backed up.
        let mixed = r#"{"games":[{"id":"g1","name":"Foo"}],"game_prefs":{"library:x":5,"library:y":{}},"settings":{}}"#;
        let path = temp_state_path("prefs_mixed");
        std::fs::write(&path, mixed).unwrap();
        let app = AppState::load(path.clone());
        let kept: Vec<String> = app.launcher.lock().game_prefs.keys().cloned().collect();
        println!(
            "one unreadable entry: {} dropped, kept {kept:?}",
            dropped_entries(mixed, &app.launcher.lock())
        );
        assert_eq!(kept, ["library:y"]);
        assert_eq!(app.launcher.lock().games.len(), 1);
        assert_eq!(
            std::fs::read_to_string(path.with_extension("json.bak")).unwrap(),
            mixed
        );
        assert_eq!(app.load_error(), None);
        cleanup(&path);

        // With no backup possible, the run is read-only and the file kept.
        let path = temp_state_path("prefs_mixed_no_backup");
        std::fs::write(&path, mixed).unwrap();
        occupy_every_backup_name(&path);
        let app = AppState::load(path.clone());
        println!("no backup possible: {:?}", app.load_error());
        assert!(
            app.load_error().is_some(),
            "an unpreserved file is read-only"
        );
        app.save().unwrap_err();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), mixed);
        cleanup(&path);

        // A record that is not an object loads empty, after a backup.
        let scalar = r#"{"game_prefs":5}"#;
        let path = temp_state_path("prefs_scalar");
        std::fs::write(&path, scalar).unwrap();
        let app = AppState::load(path.clone());
        println!(
            "not an object: {} dropped",
            dropped_entries(scalar, &app.launcher.lock())
        );
        assert!(app.launcher.lock().game_prefs.is_empty());
        assert_eq!(
            std::fs::read_to_string(path.with_extension("json.bak")).unwrap(),
            scalar
        );
        assert_eq!(app.load_error(), None);
        cleanup(&path);
    }

    /// Outside the store's own files, code reads per-game values through the
    /// store's accessors, never the field.
    #[test]
    fn game_prefs_field_stays_in_the_store() {
        let uses = crate::util::count_in_sources_by_file(concat!(".game", "_prefs"));
        let calls = crate::util::count_in_sources_by_file(concat!(".game", "_prefs("));
        println!("{} files scanned", uses.len());
        assert!(!uses.is_empty(), "the source scan found no files");
        assert_eq!(uses.len(), calls.len(), "both scans read the same files");
        let mut outside = Vec::new();
        for ((file, all), (_, accessor_calls)) in uses.iter().zip(&calls) {
            let access = all - accessor_calls;
            println!("{}: {access}", file.display());
            if access > 0 && file != Path::new("state.rs") && file != Path::new("models.rs") {
                outside.push((file.clone(), access));
            }
        }
        assert!(
            outside.is_empty(),
            "game_prefs field access outside state.rs and models.rs: {outside:?}"
        );
    }

    #[test]
    fn backup_never_replaces_an_existing_one() {
        let path = temp_state_path("backup_kept");
        std::fs::write(path.with_extension("json.bak"), "older backup").unwrap();
        std::fs::write(&path, "{ not json").unwrap();

        let app = AppState::load(path.clone());

        assert_eq!(app.load_error(), None);
        assert_eq!(
            std::fs::read_to_string(path.with_extension("json.bak")).unwrap(),
            "older backup"
        );
        assert_eq!(
            std::fs::read_to_string(path.with_extension("json.bak.1")).unwrap(),
            "{ not json"
        );
        cleanup(&path);
    }

    /// The antivirus case: another handle holds the file with no read sharing
    /// for the whole load.
    #[cfg(windows)]
    #[test]
    fn locked_state_is_read_only() {
        use std::os::windows::fs::OpenOptionsExt;
        let path = temp_state_path("locked");
        std::fs::write(&path, ONE_GAME).unwrap();
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .unwrap();

        let app = AppState::load(path.clone());
        let saved = app.save();
        drop(lock);

        assert!(app.load_error().is_some(), "a held file must be read-only");
        assert!(saved.is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), ONE_GAME);
        cleanup(&path);
    }

    #[cfg(windows)]
    #[test]
    fn transient_lock_is_retried() {
        use std::os::windows::fs::OpenOptionsExt;
        let path = temp_state_path("transient_lock");
        std::fs::write(&path, ONE_GAME).unwrap();
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            drop(lock);
        });

        let app = AppState::load(path.clone());
        release.join().unwrap();

        assert_eq!(app.load_error(), None);
        assert_eq!(app.launcher.lock().games.len(), 1);
        cleanup(&path);
    }

    #[test]
    fn edit_settings_refuses_before_changing_memory() {
        let path = temp_state_path("edit_settings_read_only");
        std::fs::create_dir(&path).unwrap();
        let app = AppState::load(path.clone());
        let before = serde_json::to_string(&*app.launcher.lock()).unwrap();
        let refused = app
            .edit_settings(|settings| settings.gemini_model = "changed".to_owned())
            .unwrap_err();
        println!("read-only: refused with {refused:?}");
        assert_eq!(
            serde_json::to_string(&*app.launcher.lock()).unwrap(),
            before,
            "a refused edit changed memory"
        );
        assert!(!path.with_extension("json.tmp").exists());
        cleanup(&path);

        let path = temp_state_path("edit_settings_writable");
        let app = AppState::load(path.clone());
        app.edit_settings(|settings| settings.gemini_model = "changed".to_owned())
            .unwrap();
        assert_eq!(app.launcher.lock().settings.gemini_model, "changed");
        let reloaded = AppState::load(path.clone());
        println!(
            "writable: memory and file hold {:?}",
            reloaded.launcher.lock().settings.gemini_model
        );
        assert_eq!(reloaded.launcher.lock().settings.gemini_model, "changed");
        cleanup(&path);
    }

    /// Outside this file, only Save's merge assigns the settings, and no code
    /// borrows them mutably or assigns one of their fields: every other write
    /// goes through `edit_settings`, which refuses before changing memory.
    #[test]
    fn settings_are_written_only_by_their_writers() {
        let count = |needle: &str| -> Vec<(PathBuf, usize)> {
            crate::util::count_in_sources_by_file(needle)
                .into_iter()
                .filter(|(file, _)| file != Path::new("state.rs"))
                .collect()
        };
        let hits = |counts: &[(PathBuf, usize)]| -> Vec<(PathBuf, usize)> {
            counts.iter().filter(|(_, n)| *n > 0).cloned().collect()
        };
        let assigned = count(concat!("launcher.settings", " = "));
        println!("{} files scanned (state.rs excluded)", assigned.len());
        assert!(!assigned.is_empty(), "the source scan found no files");
        let mut wrong = Vec::new();
        println!("launcher.settings = : {:?}", hits(&assigned));
        if hits(&assigned) != [(PathBuf::from("commands/settings.rs"), 1)] {
            wrong.push(format!(
                "launcher.settings = is allowed once, in commands/settings.rs: {:?}",
                hits(&assigned)
            ));
        }
        let mut needles = vec![concat!("&mut launcher", ".settings").to_owned()];
        let fields = serde_json::to_value(LauncherSettings::default()).unwrap();
        for field in fields.as_object().unwrap().keys() {
            needles.push(format!(".settings.{field} = "));
            needles.push(format!(".settings.{field}.clone_from("));
        }
        for needle in &needles {
            let found = hits(&count(needle));
            println!("{needle}: {found:?}");
            if !found.is_empty() {
                wrong.push(format!("{needle} outside state.rs: {found:?}"));
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    #[test]
    fn save_is_atomic_and_leaves_no_tmp() {
        let path = temp_state_path("atomic");
        let app = AppState::load(path.clone());
        app.launcher.lock().games.push(Game {
            id: "x".to_owned(),
            ..Default::default()
        });
        app.save().unwrap();

        assert!(path.exists());
        assert!(
            !path.with_extension("json.tmp").exists(),
            "atomic save must not leave a .tmp behind"
        );
        let contents = std::fs::read_to_string(&path).unwrap();
        serde_json::from_str::<serde_json::Value>(&contents).unwrap();
        cleanup(&path);
    }
}
