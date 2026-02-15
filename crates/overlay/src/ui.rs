use imgui::{Condition, StyleColor, Ui};

use crate::api;
use crate::capture;
use crate::state::{ChatMessage, MessageRole, STATE};
use crate::RUNTIME;

const USER_COLOR: [f32; 4] = [0.4, 0.7, 1.0, 1.0]; // light blue
const ASSISTANT_COLOR: [f32; 4] = [0.6, 1.0, 0.6, 1.0]; // light green
const ERROR_COLOR: [f32; 4] = [1.0, 0.4, 0.4, 1.0]; // red

/// Reference resolution for UI layout. All sizes are authored for this
/// resolution and then scaled proportionally to the actual display.
const REF_WIDTH: f32 = 1920.0;

pub fn draw_panel(ui: &Ui) {
    // Snapshot state we need for drawing, then drop the lock.
    let (messages_snapshot, is_loading, error_snapshot, attach_screenshot, input_snapshot, streaming_snapshot) = {
        let state = STATE.lock();
        (
            state
                .messages
                .iter()
                .map(|m| (m.role, m.content.clone()))
                .collect::<Vec<_>>(),
            state.is_loading,
            state.error.clone(),
            state.attach_screenshot,
            state.input_buffer.clone(),
            state.streaming_response.clone(),
        )
    };

    // Scale factor: sizes are authored for 1920-wide; scale up on larger displays.
    let display_w = ui.io().display_size[0];
    let scale = (display_w / REF_WIDTH).max(1.0);

    let win_w = 500.0 * scale;
    let win_h = 400.0 * scale;
    let margin = 16.0 * scale;
    let btn_w = 64.0 * scale;
    let input_h = 60.0 * scale;
    let input_area_height = 100.0 * scale;
    let status_bar_height = 24.0 * scale;

    let window_bg = ui.push_style_color(StyleColor::WindowBg, [0.08, 0.08, 0.10, 0.92]);

    ui.window("Claude Game Companion")
        .position([margin, margin], Condition::FirstUseEver)
        .size([win_w, win_h], Condition::FirstUseEver)
        .build(|| {
            let window_size = ui.content_region_avail();

            // --- Chat history (scrollable) ---
            let chat_height = window_size[1] - input_area_height - status_bar_height;

            if let Some(_child) =
                ui.child_window("##chat_history")
                    .size([0.0, chat_height])
                    .begin()
            {
                for (role, content) in &messages_snapshot {
                    let (label, color) = match role {
                        MessageRole::User => ("You", USER_COLOR),
                        MessageRole::Assistant => ("Sage", ASSISTANT_COLOR),
                    };
                    let _color = ui.push_style_color(StyleColor::Text, color);
                    ui.text_wrapped(format!("{label}: {content}"));
                    _color.pop();
                    ui.spacing();
                }

                // Show streaming response in progress
                if is_loading && !streaming_snapshot.is_empty() {
                    let _color = ui.push_style_color(StyleColor::Text, ASSISTANT_COLOR);
                    ui.text_wrapped(format!("Sage: {streaming_snapshot}"));
                    _color.pop();
                    ui.spacing();
                    ui.set_scroll_here_y_with_ratio(1.0);
                }

                // Auto-scroll to bottom when new content arrives
                if ui.scroll_y() >= ui.scroll_max_y() - 20.0 {
                    ui.set_scroll_here_y_with_ratio(1.0);
                }
            }

            ui.separator();

            // --- Input section ---
            let mut input_buf = input_snapshot;
            ui.input_text_multiline("##input", &mut input_buf, [window_size[0] - btn_w - 16.0, input_h])
                .build();

            // Check for Enter (without Shift) to send
            let enter_pressed = ui.is_key_pressed(imgui::Key::Enter)
                && !ui.io().key_shift;

            ui.same_line();
            if is_loading {
                // Show Cancel button instead of Send when loading
                if ui.button_with_size("Cancel", [btn_w, input_h]) {
                    let mut state = STATE.lock();
                    // Save partial response if any
                    if !state.streaming_response.is_empty() {
                        let partial = format!("{} [cancelled]", state.streaming_response);
                        state.streaming_response.clear();
                        state.messages.push(ChatMessage {
                            role: MessageRole::Assistant,
                            content: partial,
                        });
                    }
                    state.is_loading = false;
                    state.request_generation += 1; // invalidate in-flight request
                    state.error = Some("Cancelled.".into());
                }
            } else {
                let send_enabled = !input_buf.trim().is_empty();
                let mut send_pressed = false;
                ui.enabled(send_enabled, || {
                    send_pressed = ui.button_with_size("Send", [btn_w, input_h]);
                });

                // Handle send
                if (send_pressed || enter_pressed) && send_enabled {
                    let generation = {
                        let mut state = STATE.lock();
                        let text = state.input_buffer.trim().to_string();
                        if !text.is_empty() {
                            state.messages.push(ChatMessage {
                                role: MessageRole::User,
                                content: text,
                            });
                            state.input_buffer.clear();
                            state.is_loading = true;
                            state.error = None;
                            state.request_generation += 1;
                            state.streaming_response.clear();
                            Some(state.request_generation)
                        } else {
                            None
                        }
                    };

                    if let Some(gen) = generation {
                        // Clear local buffer so write-back doesn't overwrite the cleared state
                        input_buf.clear();

                        // Clone conversation history for the async task
                        let messages = STATE.lock().messages.clone();

                        // Capture screenshot if requested
                        let screenshot = if attach_screenshot {
                            match capture::capture_screenshot() {
                                Some(data) => Some(data),
                                None => {
                                    STATE.lock().error =
                                        Some("Screenshot capture failed — sending text only.".into());
                                    None
                                }
                            }
                        } else {
                            None
                        };

                        RUNTIME.spawn(async move {
                            let result = api::send_message(messages, screenshot, gen).await;
                            let mut state = STATE.lock();
                            // Only apply result if this request hasn't been cancelled
                            if state.request_generation == gen {
                                match result {
                                    Ok(response) => {
                                        state.messages.push(ChatMessage {
                                            role: MessageRole::Assistant,
                                            content: response,
                                        });
                                        state.streaming_response.clear();
                                        state.is_loading = false;
                                    }
                                    Err(err) => {
                                        // If we got partial content before error, keep it
                                        if !state.streaming_response.is_empty() {
                                            let partial = state.streaming_response.clone();
                                            state.streaming_response.clear();
                                            state.messages.push(ChatMessage {
                                                role: MessageRole::Assistant,
                                                content: partial,
                                            });
                                        }
                                        state.error = Some(err);
                                        state.is_loading = false;
                                    }
                                }
                            }
                        });
                    }
                }
            }

            let mut attach = attach_screenshot;
            ui.checkbox("Attach Screenshot", &mut attach);
            ui.same_line();
            // Clear button — reset conversation
            if ui.small_button("Clear Chat") {
                let mut state = STATE.lock();
                state.messages.clear();
                state.error = None;
                state.is_loading = false;
                state.streaming_response.clear();
                state.request_generation += 1; // cancel any in-flight request
            }

            // Write back UI changes to state
            {
                let mut state = STATE.lock();
                state.input_buffer = input_buf;
                state.attach_screenshot = attach;
            }

            // --- Status bar ---
            ui.separator();
            if let Some(err) = &error_snapshot {
                let _color = ui.push_style_color(StyleColor::Text, ERROR_COLOR);
                ui.text(format!("Error: {err}"));
                _color.pop();
            } else if is_loading {
                ui.text("Streaming...");
            } else {
                ui.text("Ready");
            }
        });

    window_bg.pop();
}
