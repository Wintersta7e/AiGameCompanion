//! Overlay AI commands: streaming dispatch, cancellation, provider availability,
//! and persisting the selected provider.

use tauri::ipc::Channel;
use tauri::{AppHandle, Manager, State};

use crate::ai::{AiState, ChatMessage, Provider, ProviderAvailability, RequestParams, SageEvent};
use crate::state::AppState;

/// The Gemini model chosen in Settings. The lock is released on return, so no
/// caller holds it across an await or another lock.
fn settings_model(state: &AppState) -> String {
    state.launcher.lock().settings.gemini_model.clone()
}

/// Report which providers can currently serve a request (for the UI dropdown).
#[tauri::command]
#[expect(
    clippy::needless_pass_by_value,
    reason = "a Tauri command receives its arguments by value"
)]
pub(crate) fn available_providers(
    ai: State<'_, AiState>,
    state: State<'_, AppState>,
) -> ProviderAvailability {
    ai.availability(&settings_model(&state))
}

/// Start a streaming chat request. Tokens arrive on `channel`; issuing a newer
/// request cancels this one.
#[tauri::command]
#[expect(
    clippy::needless_pass_by_value,
    reason = "a Tauri command receives its arguments by value"
)]
#[expect(
    clippy::too_many_arguments,
    reason = "a Tauri command takes each IPC argument as its own parameter"
)]
pub(crate) fn ask_sage(
    app: AppHandle,
    request_id: u64,
    conversation_id: u64,
    provider: Provider,
    messages: Vec<ChatMessage>,
    attach_screenshot: bool,
    hints: bool,
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
            hints,
        },
        channel,
    );
}

/// Cancel the in-flight request if it matches `request_id` (Stop button).
#[tauri::command]
#[expect(
    clippy::needless_pass_by_value,
    reason = "a Tauri command receives its arguments by value"
)]
pub(crate) fn cancel_sage(ai: State<'_, AiState>, request_id: u64) {
    ai.cancel(request_id);
}

/// Persist the user's selected provider so it survives restarts.
#[tauri::command]
#[expect(
    clippy::needless_pass_by_value,
    reason = "a Tauri command receives its arguments by value"
)]
pub(crate) fn set_active_provider(
    app: AppHandle,
    provider: Provider,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let saved =
        state.edit_settings(|settings| provider.as_str().clone_into(&mut settings.active_provider));
    crate::ai::notify_providers_changed(&app);
    saved
}

#[derive(serde::Serialize)]
pub(crate) struct TranslateResult {
    pub text: String,
}

/// Capture the linked game window and translate its on-screen foreign text to
/// English. One-shot (not part of the streaming chat slot).
#[tauri::command]
pub(crate) async fn translate_screen(app: AppHandle) -> Result<TranslateResult, String> {
    // Linked and revalidated, not just read: an unlinked window is never
    // captured, and a recycled handle would screenshot an unrelated window.
    let game = crate::overlay::linked_game(&app)
        .ok_or_else(|| "No linked game -- link the window in the overlay first.".to_owned())?;
    let model = settings_model(&app.state::<AppState>());
    let text = crate::ai::translate_capture(game.hwnd, game.pid, model).await?;
    Ok(TranslateResult { text })
}

/// Store (or clear, when empty) the Gemini API key in OS secret storage. Returns
/// the refreshed availability so the UI can flip the Gemini pill without a
/// restart. The key is never returned or logged.
#[tauri::command]
#[expect(
    clippy::needless_pass_by_value,
    reason = "a Tauri command receives its arguments by value"
)]
pub(crate) fn set_gemini_key(
    app: AppHandle,
    ai: State<'_, AiState>,
    state: State<'_, AppState>,
    key: String,
) -> Result<ProviderAvailability, String> {
    crate::secrets::set_gemini_key(key.trim())?;
    crate::ai::notify_providers_changed(&app);
    Ok(ai.availability(&settings_model(&state)))
}

/// Re-run CLI detection (claude/codex) off the UI thread and return the refreshed
/// availability.
#[tauri::command]
pub(crate) async fn recheck_clis(
    app: AppHandle,
    ai: State<'_, AiState>,
    state: State<'_, AppState>,
) -> Result<ProviderAvailability, String> {
    let model = settings_model(&state);
    let cfg = tokio::task::spawn_blocking(crate::ai::detect_all)
        .await
        .map_err(|error| format!("CLI re-check failed: {error}"))?;
    ai.set_cli(cfg);
    crate::ai::notify_providers_changed(&app);
    Ok(ai.availability(&model))
}
