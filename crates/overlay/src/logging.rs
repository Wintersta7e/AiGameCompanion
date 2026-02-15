use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;

use chrono::Local;

use crate::config::CONFIG;

/// The log file path for this session. Created once at first write.
static LOG_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Initialize the log file path based on game name and current time.
/// Call once after game name is detected.
pub fn init_session_log(game_name: Option<&str>) {
    let _ = LOG_PATH.get_or_init(|| {
        if !CONFIG.logging.enabled {
            return None;
        }

        let dir = log_directory()?;
        fs::create_dir_all(&dir).ok()?;

        let game_slug = game_name
            .unwrap_or("Unknown")
            .replace(' ', "-")
            .replace(|c: char| !c.is_alphanumeric() && c != '-', "");

        let timestamp = Local::now().format("%Y-%m-%d_%H-%M-%S");
        let filename = format!("{game_slug}_{timestamp}.txt");
        let path = dir.join(filename);

        // Write header
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&path)
            .ok()?;

        let header = format!(
            "=== AI Game Companion - Session Log ===\nGame: {}\nDate: {}\n========================================\n\n",
            game_name.unwrap_or("Unknown"),
            Local::now().format("%Y-%m-%d %H:%M:%S")
        );
        file.write_all(header.as_bytes()).ok()?;

        Some(path)
    });
}

fn log_directory() -> Option<PathBuf> {
    if let Some(ref custom) = CONFIG.logging.directory {
        return Some(PathBuf::from(custom));
    }
    crate::config::dll_directory().map(|d| d.join("logs"))
}

/// Append a user+assistant message pair to the session log.
/// Call after each successful API response.
pub fn log_exchange(user_msg: &str, assistant_msg: &str) {
    let Some(Some(path)) = LOG_PATH.get() else { return };

    let mut file = match OpenOptions::new().append(true).open(path) {
        Ok(f) => f,
        Err(_) => return, // Silently fail -- logging should never crash the game
    };

    let now = Local::now().format("%H:%M:%S");
    let entry = format!(
        "[{now}] You:\n{user_msg}\n\n[{now}] Sage:\n{assistant_msg}\n\n"
    );

    let _ = file.write_all(entry.as_bytes());
}
