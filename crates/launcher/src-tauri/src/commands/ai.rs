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
