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
        // Return the TRIMMED key: a legacy credential stored with a trailing
        // newline otherwise passes the emptiness test and then produces an
        // invalid `x-goog-api-key` header, which surfaces as an opaque auth
        // failure rather than anything actionable.
        Ok(key) if !key.trim().is_empty() => Some(key.trim().to_owned()),
        Ok(_) | Err(keyring::Error::NoEntry) => None,
        Err(e) => {
            // Distinguish "no key" from "secret store unavailable" -- the
            // caller falls back to plaintext config.toml on None, and doing
            // that silently because the Credential Manager was locked is worth
            // a log line.
            tracing::warn!("Credential Manager read failed: {e}");
            None
        }
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
