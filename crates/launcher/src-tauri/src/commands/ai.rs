//! Overlay AI commands: streaming dispatch, cancellation, provider availability,
//! and persisting the selected provider.

use tauri::ipc::Channel;
use tauri::{AppHandle, State};

use crate::ai::{AiState, ChatMessage, Provider, ProviderAvailability, RequestParams, SageEvent};
use crate::state::AppState;

/// The Gemini model chosen in Settings. The lock is released on return, so no
/// caller holds it across an await or another lock.
fn settings_model(state: &AppState) -> String {
    state.launcher.lock().settings.gemini_model.clone()
}

/// Report which providers can currently serve a request (for the UI dropdown).
/// Re-reads the Gemini key and the legacy config file first, so a key or file
/// changed outside the launcher shows up here.
#[tauri::command]
#[expect(
    clippy::needless_pass_by_value,
    reason = "a Tauri command receives its arguments by value"
)]
pub(crate) fn available_providers(
    ai: State<'_, AiState>,
    state: State<'_, AppState>,
) -> ProviderAvailability {
    ai.set_gemini(crate::ai::gemini_presence());
    ai.availability(&settings_model(&state))
}

/// Start a streaming chat request. Tokens arrive on `channel`; issuing a newer
/// request cancels this one. A refused screenshot is the command's error, so
/// the overlay knows on return whether the screenshot went.
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
    shot_id: Option<u64>,
    hints: bool,
    channel: Channel<SageEvent>,
) -> Result<(), String> {
    crate::ai::spawn_request(
        &app,
        RequestParams {
            request_id,
            conversation_id,
            provider,
            messages,
            shot_id,
            hints,
        },
        channel,
    )
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

/// Persist the user's selected provider so it survives restarts. A refused
/// save (a read-only run) announces nothing: the stored provider did not
/// change, and a window that re-read it would put the old one back.
#[tauri::command]
#[expect(
    clippy::needless_pass_by_value,
    reason = "a Tauri command receives its arguments by value"
)]
pub(crate) fn set_active_provider<R: tauri::Runtime>(
    app: AppHandle<R>,
    provider: Provider,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state.edit_settings(|settings| provider.as_str().clone_into(&mut settings.active_provider))?;
    crate::ai::notify_providers_changed(&app);
    Ok(())
}

/// Translate the on-screen foreign text of the translate screenshot `shot_id`,
/// the one the overlay shows, to English. One-shot (not part of the streaming
/// chat slot).
#[tauri::command]
pub(crate) async fn translate_screen(
    app: AppHandle,
    shot_id: u64,
) -> Result<crate::ai::Translation, String> {
    crate::ai::translate_held(&app, shot_id).await
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
    ai.set_gemini(crate::ai::gemini_presence());
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
    let (cfg, gemini) =
        tokio::task::spawn_blocking(|| (crate::ai::detect_all(), crate::ai::gemini_presence()))
            .await
            .map_err(|error| format!("CLI re-check failed: {error}"))?;
    ai.set_cli(cfg);
    ai.set_gemini(gemini);
    crate::ai::notify_providers_changed(&app);
    Ok(ai.availability(&model))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tauri::{Listener as _, Manager as _};

    #[test]
    fn a_refused_provider_pick_announces_no_change() {
        let dir =
            std::env::temp_dir().join(format!("aigc-test-refused-pick-{}", std::process::id()));
        // A folder where the state file belongs cannot be read, so this run
        // is read-only and refuses every save.
        let path = dir.join("launcher-state.json");
        std::fs::create_dir_all(&path).unwrap();
        let app = tauri::test::mock_app();
        app.manage(AppState::load(path));
        let (tx, rx) = std::sync::mpsc::channel();
        app.listen_any("providers-changed", move |_| {
            tx.send(()).unwrap();
        });

        let refused = set_active_provider(app.handle().clone(), Provider::Claude, app.state());
        let announced = rx
            .recv_timeout(std::time::Duration::from_millis(500))
            .is_ok();
        std::fs::remove_dir_all(&dir).unwrap();
        println!("refused: {refused:?}, announced: {announced}");
        assert!(refused.is_err(), "a read-only run saved the provider");
        assert!(
            !announced,
            "a refused pick announced a change, so every window re-read the old provider"
        );
    }
}
