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
use crate::ai::{ChatMessage, Provider};
use crate::state::{
    back_up, read_with_retry, should_promote_tmp, AppState, READ_ATTEMPTS, READ_RETRY_DELAY,
};

/// Exchanges older than this are removed when a chat is loaded.
const EXPIRY_DAYS: i64 = 700;

const CLEAR_REFUSED: &str = "The game changed -- the chat was not cleared.";
const DELETE_REFUSED_READ_ONLY: &str = "Nothing can be deleted while the library cannot be read.";
pub(crate) const DELETE_PARTIAL: &str = "Some saved chats could not be deleted.";

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

/// A chat file as written: the key it belongs to and its entries.
#[derive(Serialize)]
struct StoredChat<'a> {
    key: &'a str,
    entries: Vec<ChatEntry>,
}

/// What a request takes from the store when it is admitted.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ChatSnapshot {
    pub(crate) display_generation: u64,
    /// Whether the key's chat is the one on screen.
    pub(crate) displayed: bool,
    /// The key's clear generation: a save made under another is dropped.
    pub(crate) clear_generation: u64,
    /// The saved exchanges sent again with the question, oldest first.
    pub(crate) tail: Vec<ChatMessage>,
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
    ReadOnly {
        reason: ReadOnlyReason,
    },
    /// The last change to this chat's files failed; the chat is kept in memory.
    Failed,
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

/// The hash a store file belongs to, and whether it is the chat file itself:
/// `<h>.json`, `<h>.json.tmp`, `<h>.json.bak` or `<h>.json.bak.<n>`, where
/// `<h>` is 16 lower-case hex digits. `None` for any other name.
fn store_file(name: &str) -> Option<(&str, bool)> {
    let (hash, rest) = name.split_at_checked(16)?;
    let hex = hash
        .bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    let rest = rest.strip_prefix(".json").filter(|_| hex)?;
    let known = match rest {
        "" | ".tmp" | ".bak" => true,
        _ => rest
            .strip_prefix(".bak.")
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())),
    };
    known.then_some((hash, rest.is_empty()))
}

/// What deleting store files did.
#[derive(Debug, Default)]
struct Deleted {
    /// Chat files (`<h>.json`) deleted.
    chat_files: u32,
    /// Store files that could not be deleted, or a folder that could not be listed.
    failed: usize,
}

/// Delete every store file in `dir` whose hash `matches`; leave any other
/// file. A missing folder holds none.
fn delete_store_files(dir: &Path, matches: impl Fn(&str) -> bool) -> Deleted {
    let mut deleted = Deleted::default();
    let listing = match std::fs::read_dir(dir) {
        Ok(listing) => listing,
        Err(e) if e.kind() == ErrorKind::NotFound => return deleted,
        Err(e) => {
            log_unlisted(e.kind());
            deleted.failed += 1;
            return deleted;
        }
    };
    for item in listing {
        let Ok(item) = item else {
            deleted.failed += 1;
            continue;
        };
        let name = item.file_name();
        let Some((hash, chat_file)) = name.to_str().and_then(store_file) else {
            continue;
        };
        if !matches(hash) {
            continue;
        }
        match std::fs::remove_file(item.path()) {
            Ok(()) => deleted.chat_files += u32::from(chat_file),
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(_) => deleted.failed += 1,
        }
    }
    deleted
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
    /// The last change to its files failed.
    write_failed: bool,
}

impl KeyEntry {
    const fn empty() -> Self {
        Self {
            entries: Vec::new(),
            status: KeyStatus::Loaded,
            write_failed: false,
        }
    }

    const fn locked() -> Self {
        Self {
            entries: Vec::new(),
            status: KeyStatus::Locked,
            write_failed: false,
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
        let (exchanges, save) = memory.keys.get(key).map_or_else(
            || (Vec::new(), save_state(mode, KeyStatus::Locked, false)),
            |entry| {
                (
                    entry.entries.clone(),
                    save_state(mode, entry.status, entry.write_failed),
                )
            },
        );
        ChatView {
            display_generation: memory.display_generation,
            exchanges,
            save,
        }
    }

    /// What a request for `key` sends again and checks, under one hold of the
    /// memory lock and without I/O.
    pub(crate) fn snapshot(&self, key: &str) -> ChatSnapshot {
        let memory = self.memory.lock();
        let clear_generation = memory.generations.get(key).copied().unwrap_or(0);
        ChatSnapshot {
            display_generation: memory.display_generation,
            displayed: memory
                .displayed
                .as_ref()
                .is_some_and(|(shown, at)| shown == key && *at == clear_generation),
            clear_generation,
            tail: memory
                .keys
                .get(key)
                .map_or_else(Vec::new, |chat| resend_tail(&chat.entries)),
        }
    }

    /// The number of the chat on screen now.
    pub(crate) fn display_generation(&self) -> u64 {
        self.memory.lock().display_generation
    }

    /// Save a finished answer to `key`'s chat and return the chat's status.
    /// Dropped when the chat was cleared or deleted since the request was
    /// admitted (`clear_generation`); `None` saves nothing. A chat that may
    /// not be written keeps the answer in memory only. Blocks on file I/O.
    pub(crate) fn append_exchange(
        &self,
        key: &str,
        clear_generation: u64,
        entry: Option<SavedExchange>,
        mode: StoreMode,
    ) -> SaveState {
        let io = self.io.lock();
        let mut memory = self.memory.lock();
        let current = memory.generations.get(key).copied().unwrap_or(0);
        let Some(chat) = memory.keys.get_mut(key) else {
            return save_state(mode, KeyStatus::Locked, false);
        };
        let Some(entry) = entry.filter(|_| current == clear_generation) else {
            return save_state(mode, chat.status, chat.write_failed);
        };
        chat.entries.push(entry);
        if mode.read_only || chat.status == KeyStatus::Locked {
            return save_state(mode, chat.status, chat.write_failed);
        }
        let stored = StoredChat {
            key,
            entries: chat
                .entries
                .iter()
                .cloned()
                .map(ChatEntry::Exchange)
                .collect(),
        };
        let text = serde_json::to_string(&stored).map_err(std::io::Error::from);
        drop(memory);
        let hash = file_hash(key);
        let written = self
            .dir
            .as_deref()
            .map(|dir| text.and_then(|text| write_chat(dir, &hash, &text)));
        let failed = match written {
            Some(Ok(())) => false,
            Some(Err(e)) => {
                log_write_failed(&hash, e.kind());
                true
            }
            None => true,
        };
        if let Some(chat) = self.memory.lock().keys.get_mut(key) {
            chat.write_failed = failed;
        }
        drop(io);
        if failed {
            SaveState::Failed
        } else {
            SaveState::On
        }
    }

    /// No chat is on screen.
    pub(crate) fn hide(&self) {
        self.memory.lock().displayed = None;
    }

    /// Start the chat on screen afresh: empty it and delete its file, `.tmp`
    /// and backups. Refused unless `key` is the game on screen and
    /// `display_generation` is the page's current one. A Locked chat's files,
    /// and every file in read-only mode, stay as they are. Blocks on file I/O.
    pub(crate) fn clear(
        &self,
        key: Option<&str>,
        display_generation: u64,
        mode: StoreMode,
    ) -> Result<ChatView, String> {
        // Held throughout, so no write of this chat lands between the check and the delete.
        let io = self.io.lock();
        let mut memory = self.memory.lock();
        let Some(key) = key.filter(|key| {
            memory.display_generation == display_generation
                && memory
                    .displayed
                    .as_ref()
                    .is_some_and(|(shown, _)| shown == key)
        }) else {
            return Err(CLEAR_REFUSED.to_owned());
        };
        let generation = memory.generations.entry(key.to_owned()).or_insert(0);
        *generation += 1;
        let generation = *generation;
        memory.display_generation += 1;
        memory.displayed = Some((key.to_owned(), generation));
        let display_generation = memory.display_generation;
        let status = memory.keys.get_mut(key).map_or(KeyStatus::Locked, |entry| {
            entry.entries.clear();
            entry.status
        });
        drop(memory);

        let mut write_failed = false;
        if !mode.read_only && status == KeyStatus::Loaded {
            if let Some(dir) = self.dir.as_deref() {
                let hash = file_hash(key);
                let deleted = delete_store_files(dir, |h| h == hash.0);
                if deleted.failed > 0 {
                    log_clear_failed(&hash, deleted.failed);
                }
                write_failed = deleted.failed > 0;
            }
            if let Some(entry) = self.memory.lock().keys.get_mut(key) {
                entry.write_failed = write_failed;
            }
        }
        drop(io);
        Ok(ChatView {
            display_generation,
            exchanges: Vec::new(),
            save: save_state(mode, status, write_failed),
        })
    }

    /// Delete every saved chat -- every game's file, `.tmp` and backups,
    /// Locked ones too -- and empty the store; any other file in the folder
    /// stays. Returns how many chat files were deleted. Refused in read-only
    /// mode before any file is touched. Blocks on file I/O.
    pub(crate) fn delete_all(&self, mode: StoreMode) -> Result<u32, String> {
        if mode.read_only {
            return Err(DELETE_REFUSED_READ_ONLY.to_owned());
        }
        let io = self.io.lock();
        let deleted = self
            .dir
            .as_deref()
            .map_or_else(Deleted::default, |dir| delete_store_files(dir, |_| true));
        let mut memory = self.memory.lock();
        let Memory {
            keys, generations, ..
        } = &mut *memory;
        // A request answered for any chat emptied here must not save into it.
        let emptied: Vec<String> = keys.drain().map(|(key, _)| key).collect();
        for key in emptied {
            generations.entry(key).or_insert(0);
        }
        *generations = std::mem::take(generations)
            .into_iter()
            .map(|(key, generation)| (key, generation + 1))
            .collect();
        memory.displayed = None;
        memory.display_generation += 1;
        drop(memory);
        drop(io);
        log_deleted_all(deleted.chat_files, deleted.failed);
        if deleted.failed > 0 {
            Err(DELETE_PARTIAL.to_owned())
        } else {
            Ok(deleted.chat_files)
        }
    }
}

const fn save_state(mode: StoreMode, status: KeyStatus, write_failed: bool) -> SaveState {
    if mode.read_only {
        SaveState::ReadOnly {
            reason: ReadOnlyReason::Library,
        }
    } else if matches!(status, KeyStatus::Locked) {
        SaveState::ReadOnly {
            reason: ReadOnlyReason::ChatFile,
        }
    } else if write_failed {
        SaveState::Failed
    } else {
        SaveState::On
    }
}

/// The exchanges a request sends again, oldest first: each question as a
/// user turn and its answer as an assistant turn.
fn resend_tail(entries: &[SavedExchange]) -> Vec<ChatMessage> {
    entries
        .iter()
        .flat_map(|exchange| {
            [
                ChatMessage {
                    role: "user".to_owned(),
                    content: exchange.question.clone(),
                },
                ChatMessage {
                    role: "assistant".to_owned(),
                    content: exchange.answer.clone(),
                },
            ]
        })
        .collect()
}

/// Replace `hash`'s chat file in `dir` with `text` atomically: write a
/// `.tmp`, flush it to disk, then rename it over the file.
fn write_chat(dir: &Path, hash: &ChatHash, text: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    std::fs::create_dir_all(dir)?;
    let path = chat_file(dir, hash);
    let tmp = path.with_extension("json.tmp");
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, &path)
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
    KeyEntry {
        entries,
        status,
        write_failed: false,
    }
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

fn log_unlisted(kind: ErrorKind) {
    tracing::warn!("Saved chats: the chats folder could not be listed ({kind})");
}

fn log_clear_failed(hash: &ChatHash, failed: usize) {
    tracing::warn!("Chat {hash}: cleared, but {failed} file(s) could not be deleted");
}

fn log_write_failed(hash: &ChatHash, kind: ErrorKind) {
    tracing::warn!("Chat {hash}: the answer could not be saved ({kind}); kept until you quit");
}

fn log_deleted_all(deleted: u32, failed: usize) {
    tracing::info!("Saved chats: deleted {deleted} chat(s); {failed} file(s) could not be deleted");
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

    /// Save one exchange for `key` the way a request does, in a fresh store
    /// that loaded and showed it first; returns the save status.
    fn append_to(dir: &Path, key: &str, mode: StoreMode) -> SaveState {
        let store = ChatStore::new(Some(dir.to_owned()));
        store.load(key, mode);
        store.show(key, mode);
        let snapshot = store.snapshot(key);
        store.append_exchange(
            key,
            snapshot.clear_generation,
            Some(exchange("Saved?", Utc::now())),
            mode,
        )
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
        let saved = append_to(dir, key, WRITABLE);
        println!("directory, an answer: {saved:?}");
        assert_eq!(saved, LOCKED, "an answer for a directory at the file name");
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
                let saved = append_to(dir, key, WRITABLE);
                println!("no access, an answer: {saved:?}");
                assert_eq!(saved, LOCKED, "an answer for an unreadable file");
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
        let saved = append_to(dir, key, WRITABLE);
        println!("another key's file, an answer: {saved:?}");
        assert_eq!(saved, LOCKED, "an answer for another key's file");
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

        // Clear empties the chat on screen and deletes nothing.
        let shown = store.show(readable, READ_ONLY);
        let cleared = store.clear(Some(readable), shown.display_generation, READ_ONLY);
        println!("clear: {cleared:?}");
        let cleared = cleared.expect("a read-only clear was refused");
        assert_eq!(
            cleared.save,
            SaveState::ReadOnly {
                reason: ReadOnlyReason::Library
            },
            "clear"
        );
        assert!(cleared.exchanges.is_empty(), "clear kept the exchanges");
        // Delete all is refused before it touches a file.
        let deleted = store.delete_all(READ_ONLY);
        println!("delete all: {deleted:?}");
        assert_eq!(
            deleted,
            Err(DELETE_REFUSED_READ_ONLY.to_owned()),
            "delete all"
        );
        // An answer is kept in memory and sent again, and written nowhere.
        let snapshot = store.snapshot(readable);
        let saved = store.append_exchange(
            readable,
            snapshot.clear_generation,
            Some(exchange("Kept?", Utc::now())),
            READ_ONLY,
        );
        let tail = store.snapshot(readable).tail;
        println!("an answer: {saved:?}, tail {tail:?}");
        assert_eq!(
            saved,
            SaveState::ReadOnly {
                reason: ReadOnlyReason::Library
            },
            "an answer"
        );
        let sent: Vec<(&str, &str)> = tail
            .iter()
            .map(|turn| (turn.role.as_str(), turn.content.as_str()))
            .collect();
        assert_eq!(sent, [("user", "Kept?"), ("assistant", "Go north.")]);

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

        // Clear puts a new chat on screen: both numbers move on.
        let key_generation = |key: &str| store.memory.lock().generations.get(key).copied();
        let before = key_generation("exe:a").unwrap_or(0);
        let cleared = store
            .clear(Some("exe:a"), back, WRITABLE)
            .expect("clearing the chat on screen was refused")
            .display_generation;
        let after = key_generation("exe:a");
        println!("clear: display {back} -> {cleared}, key {before} -> {after:?}");
        assert!(cleared > back, "a cleared chat must be a new generation");
        assert_eq!(after, Some(before + 1), "the key's clear generation");
        assert_eq!(
            show("exe:a"),
            cleared,
            "the cleared chat is the one on screen"
        );

        // A clear between a request's snapshot and its save drops the save.
        let dir = temp_dir("generations");
        let store = ChatStore::new(Some(dir.clone()));
        store.load("exe:a", WRITABLE);
        let shown = store.show("exe:a", WRITABLE);
        let asked = store.snapshot("exe:a");
        store
            .clear(Some("exe:a"), shown.display_generation, WRITABLE)
            .expect("clearing the chat on screen was refused");
        let files = listing(&dir);
        let saved = store.append_exchange(
            "exe:a",
            asked.clear_generation,
            Some(exchange("Late?", Utc::now())),
            WRITABLE,
        );
        let tail = store.snapshot("exe:a").tail;
        println!("a save after a clear: {saved:?}, tail {tail:?}");
        assert_eq!(listing(&dir), files, "a save after a clear wrote a file");
        assert!(tail.is_empty(), "a save after a clear reached the chat");
        drop(std::fs::remove_dir_all(&dir));
    }

    #[test]
    fn round_trip_and_stored_shape() {
        let dir = temp_dir("round_trip");
        let key = "exe:round-trip";
        let store = ChatStore::new(Some(dir.clone()));
        store.load(key, WRITABLE);
        store.show(key, WRITABLE);
        let searched = SavedExchange {
            search: Some(SearchReport {
                searched: true,
                queries: vec!["foo".to_owned()],
                sources: vec![SearchSource {
                    title: "Foo".to_owned(),
                    url: "https://example.com/".to_owned(),
                    host: "example.com".to_owned(),
                }],
                suggestion_html: "<div>foo</div>".to_owned(),
                note: SearchNote::QueriesOnly,
                hints: true,
            }),
            ..exchange("Second?", Utc::now())
        };
        let saved = [
            exchange("First?", Utc::now()),
            searched,
            SavedExchange {
                screenshot: true,
                ..exchange("Third?", Utc::now())
            },
        ];
        for exchange in &saved {
            let snapshot = store.snapshot(key);
            let status = store.append_exchange(
                key,
                snapshot.clear_generation,
                Some(exchange.clone()),
                WRITABLE,
            );
            assert_eq!(status, SaveState::On, "{}", exchange.question);
        }
        let reopened = view(&dir, key, WRITABLE);
        println!("reopened: {:?}", questions(&reopened));
        assert_eq!(reopened.exchanges, saved, "the chat read back");

        let path = path_of(&dir, key);
        let text = std::fs::read_to_string(&path).unwrap();
        println!("{text}");
        assert!(
            !text.contains('\n') && !text.contains(": "),
            "the file is not compact JSON"
        );
        assert!(!tmp_of(&path).exists(), "a .tmp was left");
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        let keys = |value: &serde_json::Value| {
            let mut keys: Vec<String> = value.as_object().unwrap().keys().cloned().collect();
            keys.sort();
            keys
        };
        println!("file keys: {:?}", keys(&value));
        assert_eq!(keys(&value), ["entries", "key"]);
        let entry_keys = keys(&value["entries"][1]);
        println!("entry keys: {entry_keys:?}");
        assert_eq!(
            entry_keys,
            [
                "answer",
                "kind",
                "model",
                "provider",
                "question",
                "savedAt",
                "screenshot",
                "search"
            ]
        );
        drop(std::fs::remove_dir_all(&dir));
    }

    /// `key`'s chat file with one exchange, and a `.tmp`, `.bak` and `.bak.3`.
    fn write_with_copies(dir: &Path, key: &str) -> Vec<PathBuf> {
        let path = path_of(dir, key);
        let text = file_text(key, &[good()]);
        let paths = vec![
            path.clone(),
            tmp_of(&path),
            bak_of(&path),
            path.with_extension("json.bak.3"),
        ];
        for path in &paths {
            std::fs::write(path, &text).unwrap();
        }
        paths
    }

    /// The listing of `key`'s files only.
    fn listing_of(dir: &Path, key: &str) -> Vec<(String, u64, std::time::SystemTime)> {
        let hash = file_hash(key).to_string();
        listing(dir)
            .into_iter()
            .filter(|(name, ..)| name.starts_with(&hash))
            .collect()
    }

    /// Load `key` and put it on screen; returns its display generation.
    fn on_screen(store: &ChatStore, key: &str) -> u64 {
        store.load(key, WRITABLE);
        store.show(key, WRITABLE).display_generation
    }

    #[test]
    fn clear_removes_backups() {
        let dir = temp_dir("clear");
        let (a, b) = ("exe:a", "exe:b");
        let a_files = write_with_copies(&dir, a);
        write_with_copies(&dir, b);
        let b_before = listing_of(&dir, b);
        let store = ChatStore::new(Some(dir.clone()));
        let shown = on_screen(&store, a);

        // Refused: nothing is cleared, in memory or on disk.
        let before = listing(&dir);
        for (name, key, generation) in [
            ("a stale display generation", Some(a), shown - 1),
            ("no key", None, shown),
            ("a key not on screen", Some(b), shown),
        ] {
            let result = store.clear(key, generation, WRITABLE);
            println!("{name}: {result:?}");
            assert_eq!(result, Err(CLEAR_REFUSED.to_owned()), "{name}");
        }
        assert_eq!(listing(&dir), before, "a refused clear changed the folder");
        assert_eq!(
            questions(&store.show(a, WRITABLE)),
            ["Where next?"],
            "a refused clear emptied the chat"
        );

        let cleared = store.clear(Some(a), shown, WRITABLE).unwrap();
        println!("cleared: {:?}, {:?}", cleared.save, questions(&cleared));
        println!("after: {:?}", listing(&dir));
        assert_eq!(
            cleared.save,
            SaveState::On,
            "a clear that deleted every file"
        );
        assert!(
            cleared.exchanges.is_empty(),
            "the cleared chat has exchanges"
        );
        for path in &a_files {
            assert!(!path.exists(), "{} is still there", path.display());
        }
        assert_eq!(listing_of(&dir, b), b_before, "another key's files changed");
        assert!(
            store.show(a, WRITABLE).exchanges.is_empty(),
            "memory still holds the cleared chat"
        );

        // A file that cannot be deleted: the chat says so until a clear succeeds.
        let path = path_of(&dir, a);
        let stuck = path.with_extension("json.bak.5");
        std::fs::write(&path, file_text(a, &[good()])).unwrap();
        std::fs::create_dir(&stuck).unwrap();
        let shown = store.show(a, WRITABLE).display_generation;
        let cleared = store.clear(Some(a), shown, WRITABLE).unwrap();
        println!("a delete failed: {:?}", cleared.save);
        assert_eq!(cleared.save, SaveState::Failed, "a delete failed");
        assert!(!path.exists(), "the deletable file is still there");
        assert_eq!(
            store.show(a, WRITABLE).save,
            SaveState::Failed,
            "shown again"
        );
        std::fs::remove_dir(&stuck).unwrap();
        let shown = store.show(a, WRITABLE).display_generation;
        let cleared = store.clear(Some(a), shown, WRITABLE).unwrap();
        println!("cleared again: {:?}", cleared.save);
        assert_eq!(cleared.save, SaveState::On, "a clear after the failure");

        // A Locked chat is emptied on screen; every file stays.
        let locked = "exe:locked";
        let path = path_of(&dir, locked);
        let mut unknown = entry(&exchange("Who is this?", Utc::now()));
        unknown["provider"] = serde_json::json!("elsewhere");
        std::fs::write(&path, file_text(locked, &[good(), unknown])).unwrap();
        std::fs::write(tmp_of(&path), "older").unwrap();
        std::fs::write(bak_of(&path), "older").unwrap();
        for n in 1..=99 {
            std::fs::write(path.with_extension(format!("json.bak.{n}")), "older").unwrap();
        }
        let shown = on_screen(&store, locked);
        assert_eq!(
            questions(&store.show(locked, WRITABLE)),
            ["Where next?"],
            "the Locked chat's readable entry"
        );
        let before = listing(&dir);
        let cleared = store.clear(Some(locked), shown, WRITABLE).unwrap();
        println!(
            "Locked: {:?}, {} exchanges",
            cleared.save,
            cleared.exchanges.len()
        );
        assert_eq!(cleared.save, LOCKED, "a Locked chat");
        assert!(
            cleared.exchanges.is_empty(),
            "the Locked chat has exchanges"
        );
        assert_eq!(listing(&dir), before, "a Locked chat's files changed");
        drop(std::fs::remove_dir_all(&dir));
    }

    #[test]
    fn delete_all_empties_the_store() {
        let dir = temp_dir("delete_all");
        let store = ChatStore::new(Some(dir.clone()));
        // A key cleared once before: its clear generation is 1.
        let cleared = "exe:cleared";
        let shown = on_screen(&store, cleared);
        store.clear(Some(cleared), shown, WRITABLE).unwrap();

        let keys = ["exe:a", cleared, "exe:locked"];
        write_with_copies(&dir, "exe:a");
        write_with_copies(&dir, cleared);
        // A Locked key: its file holds another key's chat.
        let path = path_of(&dir, "exe:locked");
        std::fs::write(&path, file_text("exe:someone-else", &[good()])).unwrap();
        std::fs::write(bak_of(&path), "older").unwrap();
        let others = [
            "notes.txt",
            "0123456789abcdef.json.old",
            "0123456789ABCDEF.json",
            "0123456789abcdef.json.bak.x",
        ];
        for name in others {
            std::fs::write(dir.join(name), "keep").unwrap();
        }
        on_screen(&store, "exe:locked");
        on_screen(&store, "exe:a");
        let before: Vec<u64> = {
            let memory = store.memory.lock();
            keys.iter()
                .map(|key| memory.generations.get(*key).copied().unwrap_or(0))
                .collect()
        };
        let display_before = store.memory.lock().display_generation;
        println!("before: {:?}", listing(&dir));

        let deleted = store.delete_all(WRITABLE);
        let mut left: Vec<String> = listing(&dir).into_iter().map(|(name, ..)| name).collect();
        println!("deleted {deleted:?}, left {left:?}");
        assert_eq!(deleted, Ok(3), "the chat files deleted");
        left.sort();
        let mut expected = others.map(str::to_owned).to_vec();
        expected.sort();
        assert_eq!(left, expected, "what stays in the folder");
        let memory = store.memory.lock();
        let after: Vec<Option<u64>> = keys
            .iter()
            .map(|key| memory.generations.get(*key).copied())
            .collect();
        println!(
            "generations {before:?} -> {after:?}, display {display_before} -> {}",
            memory.display_generation
        );
        assert!(memory.keys.is_empty(), "memory still holds chats");
        assert!(memory.displayed.is_none(), "a chat is still on screen");
        assert!(
            memory.display_generation > display_before,
            "the display generation"
        );
        let expected: Vec<Option<u64>> = before.iter().map(|g| Some(g + 1)).collect();
        assert_eq!(after, expected, "every clear generation");
        drop(memory);

        // A store-named file that cannot be deleted.
        let stuck = dir.join("fedcba9876543210.json.bak.2");
        std::fs::create_dir(&stuck).unwrap();
        write_with_copies(&dir, "exe:a");
        let deleted = store.delete_all(WRITABLE);
        println!("with a stuck file: {deleted:?}");
        assert_eq!(deleted, Err(DELETE_PARTIAL.to_owned()), "a file remained");
        assert!(
            listing_of(&dir, "exe:a").is_empty(),
            "the deletable files stay"
        );

        // No folder: nothing to delete.
        for store in [
            ChatStore::new(Some(dir.join("absent"))),
            ChatStore::new(None),
        ] {
            assert_eq!(store.delete_all(WRITABLE), Ok(0), "no folder");
        }
        drop(std::fs::remove_dir_all(&dir));
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
            SaveState::Failed,
        ];
        let (mut rust_states, mut rust_reasons) = (Vec::new(), Vec::new());
        for state in states {
            // A new variant fails to compile here; add it to the list too.
            match state {
                SaveState::On
                | SaveState::Failed
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
