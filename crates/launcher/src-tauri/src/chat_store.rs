//! Saved chats: each linked game's exchanges, kept on this PC.
//!
//! One file per game key in the app's local data folder, named by a hash of
//! the key: a key may hold a local path, so it never reaches a file name, a
//! log line or the page. A file this run cannot read is never replaced (the
//! same rule as the launcher state): a damaged one is kept as a backup first,
//! and when that fails the game's chat is not saved this run.
//!
//! Every file operation holds the I/O lock; the memory lock is never held
//! across I/O, and is always taken after the I/O lock, never before.

use std::collections::HashMap;
use std::fmt;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use chrono::{DateTime, TimeDelta, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::ai::search::SearchReport;
use crate::ai::Provider;
use crate::state::{
    back_up, read_with_retry, should_promote_tmp, AppState, READ_ATTEMPTS, READ_RETRY_DELAY,
};

/// Exchanges older than this are removed when a chat is loaded.
const EXPIRY_DAYS: i64 = 700;

/// One finished question and answer, as saved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SavedExchange {
    pub(crate) saved_at: DateTime<Utc>,
    pub(crate) question: String,
    pub(crate) answer: String,
    pub(crate) provider: Provider,
    pub(crate) model: String,
    /// Whether a screenshot went with the question; the image is never kept.
    pub(crate) screenshot: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) search: Option<SearchReport>,
}

/// One entry of a chat file. An entry of a kind this build does not know is
/// dropped at load, like an unreadable one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ChatEntry {
    Exchange(SavedExchange),
}

/// Whether this run may change saved chats.
#[derive(Debug, Clone, Copy)]
pub(crate) struct StoreMode {
    /// The launcher state could not be read, so nothing is written this run.
    pub(crate) read_only: bool,
}

/// This run's store mode: read-only whenever the launcher state is.
pub(crate) fn store_mode(state: &AppState) -> StoreMode {
    StoreMode {
        read_only: state.load_error().is_some(),
    }
}

/// Whether the chat on screen is saved, as the page shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum SaveState {
    On,
    ReadOnly { reason: ReadOnlyReason },
}

/// Why a chat is not saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReadOnlyReason {
    /// The launcher state could not be read: nothing is saved this run.
    Library,
    /// This game's chat file could not be read or kept.
    ChatFile,
}

/// The chat the overlay shows: its exchanges, oldest first, and a number that
/// changes whenever another chat is put on screen.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChatView {
    pub(crate) display_generation: u64,
    pub(crate) exchanges: Vec<SavedExchange>,
    pub(crate) save: SaveState,
}

/// A chat file's name without its extension: a hash of the game key.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ChatHash(String);

impl fmt::Display for ChatHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// FNV-1a (64-bit) of the key's UTF-8 bytes, as 16 lower-case hex digits.
fn file_hash(key: &str) -> ChatHash {
    let hash = key.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    ChatHash(format!("{hash:016x}"))
}

/// The chat file of `hash` in `dir`.
fn chat_file(dir: &Path, hash: &ChatHash) -> PathBuf {
    dir.join(format!("{hash}.json"))
}

/// The folder chat files are kept in; `None` when the app's local data folder
/// cannot be found, and then every chat loads as empty.
pub(crate) fn chats_dir(app: &AppHandle) -> Option<PathBuf> {
    app.path()
        .app_local_data_dir()
        .ok()
        .map(|dir| dir.join("chats"))
        .or_else(|| {
            log_no_folder();
            None
        })
}

/// What a chat file held for the key it was read for.
struct Parsed {
    entries: Vec<SavedExchange>,
    /// Entries that could not be read and were left out.
    dropped: usize,
    /// Whether the file names the key it was read for.
    key_matches: bool,
    /// Whether the file is not a chat file at all.
    corrupt: bool,
}

/// Parse a chat file's text read for `key`. An entry that does not read --
/// including one naming a provider or a search note this build does not know
/// -- is dropped and counted, never logged: the error could quote the chat.
fn parse_chat_file(text: &str, key: &str) -> Parsed {
    let value = serde_json::from_str::<serde_json::Value>(text).ok();
    let file = value.as_ref().and_then(|value| {
        Some((
            value.get("key")?.as_str()?,
            value.get("entries")?.as_array()?,
        ))
    });
    let Some((stored_key, raw)) = file else {
        return Parsed {
            entries: Vec::new(),
            dropped: 0,
            key_matches: true,
            corrupt: true,
        };
    };
    let entries: Vec<SavedExchange> = raw
        .iter()
        .filter_map(|entry| serde_json::from_value::<ChatEntry>(entry.clone()).ok())
        .map(|ChatEntry::Exchange(exchange)| exchange)
        .collect();
    Parsed {
        dropped: raw.len() - entries.len(),
        entries,
        key_matches: stored_key == key,
        corrupt: false,
    }
}

/// Remove the exchanges saved more than `EXPIRY_DAYS` before `now`; one saved
/// exactly that long ago stays. Returns how many were removed.
fn prune_expired(entries: &mut Vec<SavedExchange>, now: DateTime<Utc>) -> usize {
    let Some(cutoff) = now.checked_sub_signed(TimeDelta::days(EXPIRY_DAYS)) else {
        return 0;
    };
    let before = entries.len();
    entries.retain(|exchange| exchange.saved_at >= cutoff);
    before - entries.len()
}

/// Whether a key's file may be written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyStatus {
    /// Read, absent, or kept as a backup first.
    Loaded,
    /// Not read, or not kept: never replaced this run.
    Locked,
}

/// One game's chat in memory.
#[derive(Debug)]
struct KeyEntry {
    entries: Vec<SavedExchange>,
    status: KeyStatus,
}

impl KeyEntry {
    const fn empty() -> Self {
        Self {
            entries: Vec::new(),
            status: KeyStatus::Loaded,
        }
    }

    const fn locked() -> Self {
        Self {
            entries: Vec::new(),
            status: KeyStatus::Locked,
        }
    }
}

#[derive(Debug, Default)]
struct Memory {
    keys: HashMap<String, KeyEntry>,
    /// Per key, a number that changes when its chat is replaced; never shrinks.
    generations: HashMap<String, u64>,
    /// The key on screen and its generation when it was put there.
    displayed: Option<(String, u64)>,
    /// Counts every change of the chat on screen; the first one makes it 1.
    display_generation: u64,
}

/// Every game's saved chat, read on first use.
#[derive(Debug)]
pub(crate) struct ChatStore {
    dir: Option<PathBuf>,
    io: Mutex<()>,
    memory: Mutex<Memory>,
}

impl ChatStore {
    pub(crate) fn new(dir: Option<PathBuf>) -> Self {
        Self {
            dir,
            io: Mutex::new(()),
            memory: Mutex::new(Memory::default()),
        }
    }

    /// Read `key`'s chat into memory, once. Blocks on file I/O.
    pub(crate) fn load(&self, key: &str, mode: StoreMode) {
        if self.memory.lock().keys.contains_key(key) {
            return;
        }
        let io = self.io.lock();
        if self.memory.lock().keys.contains_key(key) {
            return;
        }
        let entry = self
            .dir
            .as_deref()
            .map_or_else(KeyEntry::empty, |dir| read_key(dir, key, mode, Utc::now()));
        drop(io);
        self.memory
            .lock()
            .keys
            .entry(key.to_owned())
            .or_insert(entry);
    }

    /// Put `key`'s chat on screen and return it. Showing the chat already on
    /// screen keeps its generation, so the page keeps what it shows.
    pub(crate) fn show(&self, key: &str, mode: StoreMode) -> ChatView {
        let mut memory = self.memory.lock();
        let generation = memory.generations.get(key).copied().unwrap_or(0);
        let on_screen = memory
            .displayed
            .as_ref()
            .is_some_and(|(shown, at)| shown == key && *at == generation);
        if !on_screen {
            memory.display_generation += 1;
            memory.displayed = Some((key.to_owned(), generation));
        }
        let (exchanges, status) = memory.keys.get(key).map_or_else(
            || (Vec::new(), KeyStatus::Locked),
            |entry| (entry.entries.clone(), entry.status),
        );
        ChatView {
            display_generation: memory.display_generation,
            exchanges,
            save: save_state(mode, status),
        }
    }

    /// No chat is on screen.
    pub(crate) fn hide(&self) {
        self.memory.lock().displayed = None;
    }
}

const fn save_state(mode: StoreMode, status: KeyStatus) -> SaveState {
    if mode.read_only {
        SaveState::ReadOnly {
            reason: ReadOnlyReason::Library,
        }
    } else if matches!(status, KeyStatus::Locked) {
        SaveState::ReadOnly {
            reason: ReadOnlyReason::ChatFile,
        }
    } else {
        SaveState::On
    }
}

/// Read `key`'s file in `dir`. A file that cannot be read, or holds another
/// key's chat, is Locked. A damaged one is kept as a backup and the chat starts
/// from what could be read; read-only, or when no backup can be written, it is
/// Locked instead and what could be read is shown.
fn read_key(dir: &Path, key: &str, mode: StoreMode, now: DateTime<Utc>) -> KeyEntry {
    let hash = file_hash(key);
    let path = chat_file(dir, &hash);
    let tmp = path.with_extension("json.tmp");
    let probe = std::fs::metadata(&path);
    if should_promote_tmp(probe.err().map(|e| e.kind()), tmp.exists()) {
        if mode.read_only {
            log_not_promoted(&hash);
            return KeyEntry::locked();
        }
        if let Err(e) = std::fs::rename(&tmp, &path) {
            log_promote_failed(&hash, e.kind());
            return KeyEntry::locked();
        }
        log_promoted(&hash);
    }
    let text = match read_with_retry(
        &path,
        READ_ATTEMPTS,
        READ_RETRY_DELAY,
        |path| std::fs::read_to_string(path),
        |attempt, e| log_read_retry(&hash, attempt, e.kind()),
    ) {
        Ok(text) => text,
        Err(e) if e.kind() == ErrorKind::NotFound => return KeyEntry::empty(),
        Err(e) => {
            log_unreadable(&hash, e.kind());
            return KeyEntry::locked();
        }
    };
    let parsed = parse_chat_file(&text, key);
    if !parsed.key_matches {
        log_key_mismatch(&hash);
        return KeyEntry::locked();
    }
    let mut status = KeyStatus::Loaded;
    if parsed.corrupt || parsed.dropped > 0 {
        if mode.read_only {
            status = KeyStatus::Locked;
        } else {
            match back_up(&path, text.as_bytes()) {
                Ok(kept) if kept.reused => log_backup_reused(&hash),
                Ok(_) => log_backed_up(&hash),
                Err(e) => {
                    log_backup_failed(&hash, e.kind());
                    status = KeyStatus::Locked;
                }
            }
        }
    }
    let mut entries = parsed.entries;
    let expired = prune_expired(&mut entries, now);
    log_loaded(&hash, entries.len(), parsed.dropped, expired);
    KeyEntry { entries, status }
}

// Every store log line is built here, from the file hash, counts and error
// kinds only: never a key, a path or a serde error, which could quote a chat.

fn log_no_folder() {
    tracing::warn!("Saved chats are off this run: the app data folder could not be found");
}

fn log_not_promoted(hash: &ChatHash) {
    tracing::warn!("Chat {hash}: a leftover .tmp was not used, as nothing is written this run");
}

fn log_promote_failed(hash: &ChatHash, kind: ErrorKind) {
    tracing::warn!("Chat {hash}: the leftover .tmp could not be used ({kind}); not saved this run");
}

fn log_promoted(hash: &ChatHash) {
    tracing::info!("Chat {hash}: used the leftover .tmp, as the chat file was missing");
}

fn log_read_retry(hash: &ChatHash, attempt: u32, kind: ErrorKind) {
    tracing::warn!("Chat {hash}: reading failed (attempt {attempt} of {READ_ATTEMPTS}): {kind}");
}

fn log_unreadable(hash: &ChatHash, kind: ErrorKind) {
    tracing::warn!("Chat {hash}: the file could not be read ({kind}); not saved this run");
}

fn log_key_mismatch(hash: &ChatHash) {
    tracing::warn!("Chat {hash}: the file belongs to another game; left alone, not saved this run");
}

fn log_backed_up(hash: &ChatHash) {
    tracing::warn!("Chat {hash}: kept a copy of the damaged file");
}

fn log_backup_reused(hash: &ChatHash) {
    tracing::info!("Chat {hash}: a copy of the damaged file is already kept");
}

fn log_backup_failed(hash: &ChatHash, kind: ErrorKind) {
    tracing::warn!(
        "Chat {hash}: the damaged file could not be kept ({kind}); left alone, not saved this run"
    );
}

fn log_loaded(hash: &ChatHash, entries: usize, dropped: usize, expired: usize) {
    tracing::info!(
        "Chat {hash}: loaded {entries} entries ({dropped} unreadable, {expired} expired)"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::search::{SearchNote, SearchReport, SearchSource};
    use std::path::Path;

    const WRITABLE: StoreMode = StoreMode { read_only: false };
    const READ_ONLY: StoreMode = StoreMode { read_only: true };
    const LOCKED: SaveState = SaveState::ReadOnly {
        reason: ReadOnlyReason::ChatFile,
    };
    /// The only parameter types a log formatter may take: none can hold a key.
    const LOG_PARAMETER_TYPES: [&str; 5] = ["&ChatHash", "usize", "u32", "u64", "ErrorKind"];

    /// A fresh directory per test (process id + label), removed first.
    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aigc_launcher_chats_{}_{label}",
            std::process::id()
        ));
        drop(std::fs::remove_dir_all(&dir));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn exchange(question: &str, saved_at: DateTime<Utc>) -> SavedExchange {
        SavedExchange {
            saved_at,
            question: question.to_owned(),
            answer: "Go north.".to_owned(),
            provider: Provider::Gemini,
            model: "test-model".to_owned(),
            screenshot: false,
            search: None,
        }
    }

    fn entry(exchange: &SavedExchange) -> serde_json::Value {
        serde_json::to_value(ChatEntry::Exchange(exchange.clone())).unwrap()
    }

    fn file_text(key: &str, entries: &[serde_json::Value]) -> String {
        serde_json::json!({ "key": key, "entries": entries }).to_string()
    }

    fn path_of(dir: &Path, key: &str) -> PathBuf {
        chat_file(dir, &file_hash(key))
    }

    /// What a fresh store shows for `key` after loading it.
    fn view(dir: &Path, key: &str, mode: StoreMode) -> ChatView {
        let store = ChatStore::new(Some(dir.to_owned()));
        store.load(key, mode);
        store.show(key, mode)
    }

    fn questions(view: &ChatView) -> Vec<&str> {
        view.exchanges.iter().map(|x| x.question.as_str()).collect()
    }

    /// Every file in `dir`, sorted: name, size and modification time.
    fn listing(dir: &Path) -> Vec<(String, u64, std::time::SystemTime)> {
        let mut files: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                let meta = entry.metadata().unwrap();
                (
                    entry.file_name().to_string_lossy().into_owned(),
                    meta.len(),
                    meta.modified().unwrap(),
                )
            })
            .collect();
        files.sort();
        files
    }

    /// Every string anywhere in `value`.
    fn strings(value: &serde_json::Value, found: &mut Vec<String>) {
        match value {
            serde_json::Value::String(text) => found.push(text.clone()),
            serde_json::Value::Array(items) => items.iter().for_each(|v| strings(v, found)),
            serde_json::Value::Object(map) => {
                for (key, v) in map {
                    found.push(key.clone());
                    strings(v, found);
                }
            }
            serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
            }
        }
    }

    #[test]
    fn file_names_hide_the_key() {
        for (key, expected) in [
            ("", "cbf29ce484222325"),
            ("a", "af63dc4c8601ec8c"),
            ("foobar", "85944171f73967e8"),
        ] {
            let hash = file_hash(key).to_string();
            println!("file_hash({key:?}) = {hash}");
            assert_eq!(hash, expected, "the hash of {key:?}");
        }

        let dir = temp_dir("names");
        let key = r"exe:c:\games\foo\foo.exe";
        let path = path_of(&dir, key);
        let name = path.file_name().unwrap().to_str().unwrap().to_owned();
        println!("file name: {name}");
        let (stem, extension) = name.split_once('.').unwrap();
        assert_eq!(extension, "json", "{name}");
        assert!(
            stem.len() == 16
                && stem
                    .chars()
                    .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
            "not 16 lower-case hex digits: {name}"
        );

        std::fs::write(
            &path,
            file_text(key, &[entry(&exchange("Where next?", Utc::now()))]),
        )
        .unwrap();
        let shown = serde_json::to_value(view(&dir, key, WRITABLE)).unwrap();
        let mut found = Vec::new();
        strings(&shown, &mut found);
        println!("strings in the view: {found:?}");
        assert!(found.iter().any(|s| s == "Where next?"), "{found:?}");
        assert!(!found.iter().any(|s| s == key), "the view names the key");
        drop(std::fs::remove_dir_all(&dir));

        // Every log line is built by a formatter that cannot take the key.
        let code = crate::util::strip_test_modules(include_str!("chat_store.rs"));
        let mut rest = code.as_str();
        let mut formatters = 0;
        while let Some((_, after)) = rest.split_once("fn log_") {
            let (signature, tail) = after
                .split_once(')')
                .expect("a log formatter's parameter list is not closed");
            rest = tail;
            formatters += 1;
            println!("fn log_{signature})");
            let (_, params) = signature
                .split_once('(')
                .expect("a log formatter has no parameter list");
            for param in params.split(',').map(str::trim).filter(|p| !p.is_empty()) {
                let ty = param.split_once(':').map_or("", |(_, ty)| ty.trim());
                assert!(
                    LOG_PARAMETER_TYPES.contains(&ty),
                    "log_{signature}): `{param}` is not one of {LOG_PARAMETER_TYPES:?}"
                );
            }
        }
        assert!(formatters > 0, "no log formatter found in chat_store.rs");
    }

    fn tmp_of(path: &Path) -> PathBuf {
        path.with_extension("json.tmp")
    }

    fn bak_of(path: &Path) -> PathBuf {
        path.with_extension("json.bak")
    }

    fn good() -> serde_json::Value {
        entry(&exchange("Where next?", Utc::now()))
    }

    #[test]
    fn unreadable_chats_are_never_replaced() {
        let dir = temp_dir("unreadable");
        unreadable_paths_are_locked(&dir);
        damaged_files_are_kept_first(&dir);
        a_leftover_tmp_is_used_only_without_a_file(&dir);
        another_keys_file_is_locked(&dir);
        drop(std::fs::remove_dir_all(&dir));
    }

    fn unreadable_paths_are_locked(dir: &Path) {
        // A directory where the file should be.
        let key = "exe:directory";
        let path = path_of(dir, key);
        std::fs::create_dir(&path).unwrap();
        let shown = view(dir, key, WRITABLE);
        println!("directory: {:?}", shown.save);
        assert_eq!(shown.save, LOCKED, "a directory at the file name");
        assert!(path.is_dir(), "the directory was touched");
        assert!(!tmp_of(&path).exists(), "a .tmp was written");
        assert!(!bak_of(&path).exists(), "a backup was written");

        // A file this user cannot read.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let good = good();
            let key = "exe:no-access";
            let path = path_of(dir, key);
            let text = file_text(key, std::slice::from_ref(&good));
            std::fs::write(&path, &text).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
            if std::fs::read(&path).is_ok() {
                println!("no access: skipped, the file is still readable (running as root?)");
            } else {
                let shown = view(dir, key, WRITABLE);
                println!("no access: {:?}", shown.save);
                assert_eq!(shown.save, LOCKED, "an unreadable file");
            }
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                text,
                "the file changed"
            );
            assert!(!bak_of(&path).exists(), "a backup was written");
        }
    }

    fn damaged_files_are_kept_first(dir: &Path) {
        let good = good();
        // Corrupt bytes: kept as they were, and the chat starts empty.
        let key = "exe:corrupt";
        let path = path_of(dir, key);
        let corrupt = format!("{{\"key\":\"{key}\",\"entries\":[");
        std::fs::write(&path, &corrupt).unwrap();
        let shown = view(dir, key, WRITABLE);
        println!(
            "corrupt: {:?}, {} exchanges",
            shown.save,
            shown.exchanges.len()
        );
        assert_eq!(shown.save, SaveState::On, "a corrupt file that was kept");
        assert!(
            shown.exchanges.is_empty(),
            "a corrupt file showed exchanges"
        );
        assert_eq!(
            std::fs::read_to_string(bak_of(&path)).unwrap(),
            corrupt,
            "the backup does not hold the corrupt bytes"
        );

        // No backup name left: nothing can be kept, so nothing may replace it.
        let key = "exe:no-backup-name";
        let path = path_of(dir, key);
        std::fs::write(&path, &corrupt).unwrap();
        std::fs::write(bak_of(&path), "older").unwrap();
        for n in 1..=99 {
            std::fs::write(path.with_extension(format!("json.bak.{n}")), "older").unwrap();
        }
        let shown = view(dir, key, WRITABLE);
        println!("every backup name taken: {:?}", shown.save);
        assert_eq!(shown.save, LOCKED, "every backup name taken");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            corrupt,
            "the file changed"
        );

        // One entry of two unreadable (a provider this build does not know).
        let key = "exe:partial";
        let path = path_of(dir, key);
        let mut unknown = entry(&exchange("Who is this?", Utc::now()));
        unknown["provider"] = serde_json::json!("elsewhere");
        let text = file_text(key, &[good, unknown]);
        std::fs::write(&path, &text).unwrap();
        let shown = view(dir, key, WRITABLE);
        println!(
            "one entry unreadable: {:?}, {:?}",
            shown.save,
            questions(&shown)
        );
        assert_eq!(shown.save, SaveState::On, "a partial file that was kept");
        assert_eq!(questions(&shown), ["Where next?"], "the readable entry");
        assert_eq!(
            std::fs::read_to_string(bak_of(&path)).unwrap(),
            text,
            "the backup does not hold the partial file"
        );
    }

    fn a_leftover_tmp_is_used_only_without_a_file(dir: &Path) {
        let good = good();
        // A leftover .tmp becomes the file only when the file is absent.
        let key = "exe:tmp-only";
        let path = path_of(dir, key);
        let text = file_text(key, std::slice::from_ref(&good));
        std::fs::write(tmp_of(&path), &text).unwrap();
        let shown = view(dir, key, WRITABLE);
        println!(".tmp only: {:?}, {:?}", shown.save, questions(&shown));
        assert_eq!(questions(&shown), ["Where next?"], "the promoted .tmp");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            text,
            "not promoted"
        );
        assert!(!tmp_of(&path).exists(), "the .tmp is still there");

        let key = "exe:tmp-and-file";
        let path = path_of(dir, key);
        let newer = file_text(key, &[entry(&exchange("From the .tmp", Utc::now()))]);
        std::fs::write(&path, file_text(key, std::slice::from_ref(&good))).unwrap();
        std::fs::write(tmp_of(&path), &newer).unwrap();
        let shown = view(dir, key, WRITABLE);
        println!(".tmp beside the file: {:?}", questions(&shown));
        assert_eq!(questions(&shown), ["Where next?"], "the file, not the .tmp");
        assert_eq!(
            std::fs::read_to_string(tmp_of(&path)).unwrap(),
            newer,
            "the .tmp changed"
        );
    }

    fn another_keys_file_is_locked(dir: &Path) {
        let good = good();
        // A file holding another key's chat: none of it is shown.
        let key = "exe:mine";
        let path = path_of(dir, key);
        let text = file_text("exe:someone-else", std::slice::from_ref(&good));
        std::fs::write(&path, &text).unwrap();
        let shown = view(dir, key, WRITABLE);
        println!(
            "another key's file: {:?}, {} exchanges",
            shown.save,
            shown.exchanges.len()
        );
        assert_eq!(shown.save, LOCKED, "another key's file");
        assert!(
            shown.exchanges.is_empty(),
            "another key's exchanges were shown"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            text,
            "the file changed"
        );
    }

    #[test]
    fn read_only_writes_nothing() {
        let dir = temp_dir("read_only");
        let readable = "exe:readable";
        std::fs::write(
            path_of(&dir, readable),
            file_text(readable, &[entry(&exchange("Where next?", Utc::now()))]),
        )
        .unwrap();
        let corrupt = "exe:corrupt";
        std::fs::write(path_of(&dir, corrupt), "{\"key\":").unwrap();
        let tmp_only = "exe:tmp-only";
        std::fs::write(
            path_of(&dir, tmp_only).with_extension("json.tmp"),
            file_text(tmp_only, &[]),
        )
        .unwrap();
        let before = listing(&dir);
        println!("before: {before:?}");

        let store = ChatStore::new(Some(dir.clone()));
        for key in [readable, corrupt, tmp_only, "exe:absent"] {
            store.load(key, READ_ONLY);
            let shown = store.show(key, READ_ONLY);
            println!("{key}: {:?}, {:?}", shown.save, questions(&shown));
            assert_eq!(
                shown.save,
                SaveState::ReadOnly {
                    reason: ReadOnlyReason::Library
                },
                "{key}"
            );
            let expected: &[&str] = if key == readable {
                &["Where next?"]
            } else {
                &[]
            };
            assert_eq!(questions(&shown), expected, "{key}");
        }

        let after = listing(&dir);
        println!("after: {after:?}");
        assert_eq!(after, before, "a read-only run changed the chats folder");
        drop(std::fs::remove_dir_all(&dir));
    }

    #[test]
    fn generations() {
        let store = ChatStore::new(None);
        let show = |key: &str| {
            store.load(key, WRITABLE);
            store.show(key, WRITABLE).display_generation
        };
        let first = show("exe:a");
        let again = show("exe:a");
        let other = show("exe:b");
        store.hide();
        let back = show("exe:a");
        println!("a {first}, a again {again}, b {other}, a after hide {back}");
        assert_eq!(again, first, "the same chat shown twice");
        assert!(other > again, "another chat must be a new generation");
        assert!(
            back > other,
            "a chat shown after a hide must be a new generation"
        );
    }

    /// The `state` and `reason` literals of every `SaveState` equal those of
    /// chat.ts's `SaveState`.
    fn save_state_literals_match(chat: &str) {
        let states = [
            SaveState::On,
            SaveState::ReadOnly {
                reason: ReadOnlyReason::Library,
            },
            SaveState::ReadOnly {
                reason: ReadOnlyReason::ChatFile,
            },
        ];
        let (mut rust_states, mut rust_reasons) = (Vec::new(), Vec::new());
        for state in states {
            // A new variant fails to compile here; add it to the list too.
            match state {
                SaveState::On
                | SaveState::ReadOnly {
                    reason: ReadOnlyReason::Library | ReadOnlyReason::ChatFile,
                } => {}
            }
            let json = serde_json::to_value(state).unwrap();
            rust_states.push(json["state"].as_str().unwrap().to_owned());
            if let Some(reason) = json.get("reason") {
                rust_reasons.push(reason.as_str().unwrap().to_owned());
            }
        }
        let typescript_literals = |field: &str| {
            let (_, declaration) = chat
                .split_once("type SaveState =")
                .expect("chat.ts declares no SaveState");
            let mut depth = 0_i32;
            let end = declaration
                .char_indices()
                .find(|&(_, c)| {
                    match c {
                        '{' => depth += 1,
                        '}' => depth -= 1,
                        _ => {}
                    }
                    c == ';' && depth == 0
                })
                .map_or(declaration.len(), |(at, _)| at);
            let (declaration, _) = declaration.split_at(end);
            let mut literals = Vec::new();
            for (_, after) in declaration.split(&format!("{field}:")).enumerate().skip(1) {
                let value = after.split([';', '}']).next().unwrap_or_default();
                literals.extend(value.split('\'').skip(1).step_by(2).map(str::to_owned));
            }
            literals
        };
        let sorted = |mut list: Vec<String>| {
            list.sort();
            list.dedup();
            list
        };
        let (rust_states, rust_reasons) = (sorted(rust_states), sorted(rust_reasons));
        let ts_states = sorted(typescript_literals("state"));
        let ts_reasons = sorted(typescript_literals("reason"));
        println!("state: Rust {rust_states:?}, TypeScript {ts_states:?}");
        println!("reason: Rust {rust_reasons:?}, TypeScript {ts_reasons:?}");
        assert!(!rust_states.is_empty(), "no SaveState value serialised");
        assert_eq!(ts_states, rust_states, "SaveState's state literals");
        assert_eq!(ts_reasons, rust_reasons, "SaveState's reason literals");
    }

    /// Rust's `Provider` names equal the companion store's `Provider` literals.
    fn providers_match(companion: &str) {
        let providers: Vec<String> = [Provider::Gemini, Provider::Claude, Provider::Openai]
            .into_iter()
            .map(|provider| {
                // A new variant fails to compile here; add it to the list too.
                match provider {
                    Provider::Gemini | Provider::Claude | Provider::Openai => {}
                }
                serde_json::to_value(provider)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        let names: Vec<&str> = providers.iter().map(String::as_str).collect();
        println!("Provider: {names:?}");
        let mismatches = crate::util::alias_mismatches(companion, "Provider", &names);
        assert!(mismatches.is_empty(), "{mismatches:#?}");
    }

    #[test]
    fn chat_types_mirror_rust() {
        let chat = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../src/lib/utils/chat.ts"
        ));
        let companion = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../src/lib/stores/companion.svelte.ts"
        ));

        save_state_literals_match(chat);

        let full = SavedExchange {
            screenshot: true,
            search: Some(SearchReport {
                searched: true,
                queries: vec!["foo".to_owned()],
                sources: vec![SearchSource {
                    title: "Foo".to_owned(),
                    url: "https://example.com/".to_owned(),
                    host: "example.com".to_owned(),
                }],
                suggestion_html: "<div>foo</div>".to_owned(),
                note: SearchNote::None,
                hints: false,
            }),
            ..exchange("Where next?", Utc::now())
        };
        let empty = exchange("Where next?", Utc::now());
        // `provider` names the TypeScript alias, which lives in the companion store.
        let source = format!("{chat}\n{companion}");
        let (full_fields, empty_fields) = (
            crate::util::json_fields(&serde_json::to_value(&full).unwrap()),
            crate::util::json_fields(&serde_json::to_value(&empty).unwrap()),
        );
        let typescript = crate::util::ts_fields(chat, "interface SavedExchange {");
        println!("SavedExchange: full {full_fields:?}, empty {empty_fields:?}");
        println!("SavedExchange: TypeScript {typescript:?}");
        assert!(
            !full_fields.is_empty() && !typescript.is_empty(),
            "no fields read from Rust or TypeScript"
        );
        let mismatches =
            crate::util::mirror_mismatches(&typescript, &full_fields, &empty_fields, &source);
        assert!(mismatches.is_empty(), "{mismatches:#?}");

        providers_match(companion);

        let view = ChatView {
            display_generation: 2,
            exchanges: vec![full],
            save: SaveState::On,
        };
        let fields = crate::util::json_fields(&serde_json::to_value(&view).unwrap());
        let typescript = crate::util::ts_fields(chat, "interface ChatView {");
        println!("ChatView: Rust {fields:?}, TypeScript {typescript:?}");
        assert!(
            !fields.is_empty() && !typescript.is_empty(),
            "no fields read from Rust or TypeScript"
        );
        let mismatches = crate::util::mirror_mismatches(&typescript, &fields, &fields, &source);
        assert!(mismatches.is_empty(), "{mismatches:#?}");
    }

    /// Per file, the needle's count, printed; fails when no file was read.
    fn scan(needle: &str) -> Vec<(PathBuf, usize)> {
        let counts = crate::util::count_in_sources_by_file(needle);
        println!("{needle}: {} files scanned", counts.len());
        for (file, count) in counts.iter().filter(|(_, count)| *count > 0) {
            println!("  {}: {count}", file.display());
        }
        assert!(!counts.is_empty(), "no source file scanned for {needle}");
        counts
    }

    #[test]
    fn stored_types_hold_no_image_bytes() {
        let stored = [
            PathBuf::from("chat_store.rs"),
            Path::new("ai").join("search.rs"),
        ];
        for needle in [concat!("Vec<", "u8>"), concat!("base", "64")] {
            for (file, count) in scan(needle) {
                if stored.contains(&file) {
                    assert_eq!(count, 0, "{needle} in {}", file.display());
                }
            }
        }
        let needle = concat!("\"", "chats", "\"");
        for (file, count) in scan(needle) {
            if file == stored[0] {
                assert!(count >= 1, "chat_store.rs does not name the folder");
            } else {
                assert_eq!(count, 0, "{needle} in {}", file.display());
            }
        }
    }

    #[test]
    fn write_sites_are_confined() {
        let allowed = [
            PathBuf::from("state.rs"),
            PathBuf::from("chat_store.rs"),
            PathBuf::from("main.rs"),
            Path::new("ai").join("cli.rs"),
            Path::new("ai").join("mod.rs"),
        ];
        for needle in [
            concat!("File::", "create"),
            concat!("Open", "Options"),
            concat!("fs::", "write"),
            concat!("fs::", "rename"),
            concat!("remove_", "file"),
            concat!("remove_", "dir"),
            concat!("create_", "dir"),
            concat!("fs::", "copy"),
        ] {
            for (file, count) in scan(needle) {
                assert!(
                    count == 0 || allowed.contains(&file),
                    "{needle} in {} ({count}), outside {allowed:?}",
                    file.display()
                );
            }
        }
    }
}
