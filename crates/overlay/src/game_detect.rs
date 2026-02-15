use tracing::info;
use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
};

use crate::config::CONFIG;

/// Detect the game name using a 3-tier priority:
///   1. `[[games]]` name override (if configured and matches current process)
///   2. Window title of the host process
///   3. Cleaned exe name (strip .exe, split camelCase)
pub fn detect_game_name() -> Option<String> {
    // 1. Check [[games]] config for a name override
    if let Some(name) = name_from_config() {
        info!("Game name from config: {name}");
        return Some(name);
    }

    // 2. Try the window title
    if let Some(name) = name_from_window_title() {
        info!("Game name from window title: {name}");
        return Some(name);
    }

    // 3. Fall back to cleaned exe name
    if let Some(name) = name_from_exe() {
        info!("Game name from exe: {name}");
        return Some(name);
    }

    info!("Could not detect game name");
    None
}

/// Check if the current process exe matches a `[[games]]` entry with a `name` field.
fn name_from_config() -> Option<String> {
    let exe_name = current_exe_name()?;
    let exe_lower = exe_name.to_lowercase();

    for game in &CONFIG.games {
        if game.process.to_lowercase() == exe_lower {
            return game.name.clone();
        }
    }
    None
}

/// Find the main visible window of the current process and return its title.
fn name_from_window_title() -> Option<String> {
    let pid = unsafe { GetCurrentProcessId() };
    let mut best_title = String::new();

    unsafe {
        let _ = EnumWindows(
            Some(enum_window_callback),
            LPARAM(&mut (pid, &mut best_title) as *mut (u32, &mut String) as isize),
        );
    }

    let title = best_title.trim().to_string();
    if is_usable_title(&title) {
        Some(title)
    } else {
        None
    }
}

unsafe extern "system" fn enum_window_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let data = &mut *(lparam.0 as *mut (u32, &mut String));
    let (target_pid, ref mut best_title) = *data;

    let mut window_pid: u32 = 0;
    GetWindowThreadProcessId(hwnd, Some(&mut window_pid));

    if window_pid != target_pid {
        return BOOL(1); // continue
    }

    if !IsWindowVisible(hwnd).as_bool() {
        return BOOL(1); // skip invisible
    }

    let mut buf = [0u16; 512];
    let len = GetWindowTextW(hwnd, &mut buf) as usize;
    if len == 0 {
        return BOOL(1); // no title
    }

    let title = String::from_utf16_lossy(&buf[..len]);

    // Keep the longest visible window title (main game window is usually longest)
    if title.len() > best_title.len() {
        **best_title = title;
    }

    BOOL(1) // continue enumeration
}

/// Filter out generic/useless window titles.
fn is_usable_title(title: &str) -> bool {
    if title.len() < 2 {
        return false;
    }

    let lower = title.to_lowercase();
    const JUNK: &[&str] = &[
        "window",
        "default",
        "ime",
        "msctfime",
        "gdi+",
        "dwm",
        "desktop",
        "program manager",
    ];

    !JUNK.iter().any(|j| lower == *j)
}

/// Get the current process exe filename (e.g. "DarkSoulsIII.exe").
fn current_exe_name() -> Option<String> {
    use hudhook::windows::Win32::System::LibraryLoader::GetModuleFileNameW;

    let mut buf = [0u16; 512];
    let len = unsafe { GetModuleFileNameW(None, &mut buf) } as usize;
    if len == 0 {
        return None;
    }

    let path = String::from_utf16_lossy(&buf[..len]);
    path.rsplit('\\')
        .next()
        .map(|s| s.to_string())
}

/// Clean up an exe name into a readable game name.
/// "DarkSoulsIII.exe" -> "Dark Souls III"
/// "horizon-zero-dawn.exe" -> "Horizon Zero Dawn"
fn name_from_exe() -> Option<String> {
    let exe = current_exe_name()?;

    // Strip .exe
    let name = exe
        .strip_suffix(".exe")
        .or_else(|| exe.strip_suffix(".EXE"))
        .unwrap_or(&exe);

    if name.is_empty() {
        return None;
    }

    // Replace hyphens and underscores with spaces
    let name = name.replace(['-', '_'], " ");

    // Insert spaces before uppercase letters in camelCase/PascalCase
    // "DarkSoulsIII" -> "Dark Souls I I I" -> we'll handle Roman numerals
    let mut result = String::with_capacity(name.len() + 8);
    let chars: Vec<char> = name.chars().collect();

    for (i, &ch) in chars.iter().enumerate() {
        if i > 0 && ch.is_uppercase() {
            let prev = chars[i - 1];
            // Insert space if previous char is lowercase,
            // or if previous is uppercase and next is lowercase (end of acronym)
            if prev.is_lowercase()
                || (prev.is_uppercase()
                    && chars.get(i + 1).is_some_and(|c| c.is_lowercase()))
            {
                result.push(' ');
            }
        }
        result.push(ch);
    }

    // Title-case each word
    let result = result
        .split_whitespace()
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => {
                    let upper: String = first.to_uppercase().collect();
                    upper + &chars.collect::<String>()
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ");

    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}
