//! Overlay AI commands: streaming dispatch, cancellation, provider availability,
//! and persisting the selected provider.

use tauri::ipc::Channel;
use tauri::{AppHandle, State};

use crate::ai::{AiState, ChatMessage, Provider, ProviderAvailability, RequestParams, SageEvent};
use crate::state::AppState;

/// Report which providers can currently serve a request (for the UI dropdown).
#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn available_providers(ai: State<'_, AiState>) -> ProviderAvailability {
    ai.availability()
}

/// Start a streaming chat request. Tokens arrive on `channel`; issuing a newer
/// request cancels this one.
#[tauri::command]
#[allow(clippy::too_many_arguments, clippy::needless_pass_by_value)]
pub fn ask_sage(
    app: AppHandle,
    request_id: u64,
    conversation_id: u64,
    provider: Provider,
    messages: Vec<ChatMessage>,
    attach_screenshot: bool,
    channel: Channel<SageEvent>,
) {
    crate::ai::spawn_request(
        &app,
        RequestParams {
            request_id,
            conversation_id,
            provider,
            messages,
            attach_screenshot,
        },
        channel,
    );
}

/// Cancel the in-flight request if it matches `request_id` (Stop button).
#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn cancel_sage(ai: State<'_, AiState>, request_id: u64) {
    ai.cancel(request_id);
}

/// Persist the user's selected provider so it survives restarts.
#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn set_active_provider(provider: Provider, state: State<'_, AppState>) -> Result<(), String> {
    {
        let mut launcher = state.launcher.lock();
        provider
            .as_str()
            .clone_into(&mut launcher.settings.active_provider);
    }
    state.save()
}

#[derive(serde::Serialize)]
pub struct TranslateResult {
    pub text: String,
}

/// Capture the detected game window and translate its on-screen foreign text to
/// English. One-shot (not part of the streaming chat slot).
#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub async fn translate_screen(
    overlay: State<'_, crate::overlay::OverlayState>,
) -> Result<TranslateResult, String> {
    let hwnd = overlay
        .game
        .lock()
        .as_ref()
        .map(|game| game.hwnd)
        .ok_or_else(|| "No game detected -- open the overlay over a game first.".to_owned())?;
    let text = crate::ai::translate_capture(hwnd).await?;
    Ok(TranslateResult { text })
}

/// Store (or clear, when empty) the Gemini API key in OS secret storage. Returns
/// the refreshed availability so the UI can flip the Gemini pill without a
/// restart. The key is never returned or logged.
#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn set_gemini_key(ai: State<'_, AiState>, key: String) -> Result<ProviderAvailability, String> {
    crate::secrets::set_gemini_key(key.trim())?;
    Ok(ai.availability())
}

/// Re-run CLI detection (claude/codex) off the UI thread and return the refreshed
/// availability.
#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub async fn recheck_clis(ai: State<'_, AiState>) -> Result<ProviderAvailability, String> {
    let cfg = tokio::task::spawn_blocking(|| {
        let claude = crate::ai::detect_cli("claude");
        let codex = crate::ai::detect_cli("codex");
        let codex_workdir = crate::ai::ensure_codex_workdir(codex);
        crate::ai::CliConfig {
            claude,
            codex,
            codex_workdir,
        }
    })
    .await
    .map_err(|error| format!("CLI re-check failed: {error}"))?;
    ai.set_cli(cfg);
    Ok(ai.availability())
}
