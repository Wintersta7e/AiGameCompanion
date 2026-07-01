//! OS secret storage for the Gemini API key, so it no longer lives in plaintext
//! `config.toml`. Windows uses the Credential Manager (via `keyring`); other
//! hosts are no-op stubs (the launcher ships Windows-only, and its Linux test
//! build must not pull the secret-service/dbus backend).

#[cfg(windows)]
const SERVICE: &str = "AiGameCompanion";
#[cfg(windows)]
const GEMINI_USER: &str = "gemini-api-key";

/// Store the Gemini API key in OS secret storage. An empty key clears it.
#[cfg(windows)]
pub fn set_gemini_key(key: &str) -> Result<(), String> {
    let entry = keyring::Entry::new(SERVICE, GEMINI_USER)
        .map_err(|e| format!("secret store error: {e}"))?;
    if key.is_empty() {
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(format!("failed to clear key: {e}")),
        }
    } else {
        entry
            .set_password(key)
            .map_err(|e| format!("failed to store key: {e}"))
    }
}

/// Read the Gemini API key from OS secret storage, if one is stored.
#[cfg(windows)]
pub fn gemini_key() -> Option<String> {
    let entry = keyring::Entry::new(SERVICE, GEMINI_USER).ok()?;
    match entry.get_password() {
        Ok(key) if !key.trim().is_empty() => Some(key),
        _ => None,
    }
}

#[cfg(not(windows))]
pub fn set_gemini_key(_key: &str) -> Result<(), String> {
    Err("Secret storage is only available on Windows.".to_owned())
}

#[cfg(not(windows))]
pub fn gemini_key() -> Option<String> {
    None
}
