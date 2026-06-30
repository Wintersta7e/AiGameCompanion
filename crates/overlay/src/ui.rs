use imgui::{Condition, StyleColor, StyleVar, Ui};

use crate::state::{ChatMessage, MessageRole, STATE};

// ── Sage palette (overlay has no per-game cover art, so it uses the brand
//    accent rather than the launcher's cover-driven colour). ──────────────
const ACCENT: [f32; 4] = [0.878, 0.635, 0.235, 1.0]; // brand gold
const ACCENT_DIM: [f32; 4] = [0.878, 0.635, 0.235, 0.20];
const TEXT_HI: [f32; 4] = [0.925, 0.925, 0.937, 1.0];
const TEXT_MID: [f32; 4] = [0.604, 0.604, 0.655, 1.0];
const TEXT_LO: [f32; 4] = [0.360, 0.360, 0.410, 1.0];
const SAGE_BAR: [f32; 4] = [0.45, 0.47, 0.50, 0.55];
const OK_COLOR: [f32; 4] = [0.29, 0.84, 0.63, 1.0];
const ERROR_COLOR: [f32; 4] = [0.91, 0.39, 0.42, 1.0];

/// Reference resolution for UI layout. All sizes are authored for this
/// resolution and then scaled proportionally to the actual display.
const REF_WIDTH: f32 = 1920.0;

pub fn draw_panel(ui: &Ui) {
    // Snapshot state we need for drawing, then drop the lock.
    let (
        messages_snapshot,
        is_loading,
        error_snapshot,
        attach_screenshot,
        input_snapshot,
        streaming_snapshot,
        available_providers,
        current_provider,
    ) = {
        let state = STATE.lock();
        let is_loading = state.is_loading;

        let mut providers = Vec::new();
        if state.is_provider_available(crate::provider::Provider::Gemini) {
            providers.push(crate::provider::Provider::Gemini);
        }
        if state.is_provider_available(crate::provider::Provider::Claude) {
            providers.push(crate::provider::Provider::Claude);
        }
        if state.is_provider_available(crate::provider::Provider::Openai) {
            providers.push(crate::provider::Provider::Openai);
        }

        (
            state
                .messages
                .iter()
                .map(|m| (m.role, m.content.clone()))
                .collect::<Vec<_>>(),
            is_loading,
            state.error.clone(),
            state.attach_screenshot,
            state.input_buffer.clone(),
            if is_loading {
                state.streaming_response.clone()
            } else {
                String::new()
            },
            providers,
            state.active_provider,
        )
    };

    // Scale factor: sizes are authored for 1920-wide; scale up on larger displays.
    let display_w = ui.io().display_size[0];
    let scale = (display_w / REF_WIDTH).max(1.0);

    let win_w = 500.0 * scale;
    let win_h = 440.0 * scale;
    let margin = 16.0 * scale;
    let btn_w = 68.0 * scale;
    let input_h = 56.0 * scale;
    let input_area_height = 104.0 * scale;
    let status_bar_height = 26.0 * scale;

    // ── Style: rounded, padded, comfortable dark panel ──────────────────
    // Bound as named locals so they auto-pop (in reverse) at end of fn,
    // staying active for the whole window draw.
    let _sv_wr = ui.push_style_var(StyleVar::WindowRounding(13.0));
    let _sv_cr = ui.push_style_var(StyleVar::ChildRounding(11.0));
    let _sv_fr = ui.push_style_var(StyleVar::FrameRounding(9.0));
    let _sv_pr = ui.push_style_var(StyleVar::PopupRounding(9.0));
    let _sv_sr = ui.push_style_var(StyleVar::ScrollbarRounding(8.0));
    let _sv_ss = ui.push_style_var(StyleVar::ScrollbarSize(8.0));
    let _sv_wp = ui.push_style_var(StyleVar::WindowPadding([16.0, 14.0]));
    let _sv_fp = ui.push_style_var(StyleVar::FramePadding([11.0, 8.0]));
    let _sv_is = ui.push_style_var(StyleVar::ItemSpacing([10.0, 10.0]));
    let _sv_wb = ui.push_style_var(StyleVar::WindowBorderSize(1.0));
    let _sv_fb = ui.push_style_var(StyleVar::FrameBorderSize(1.0));

    let _sc_wbg = ui.push_style_color(StyleColor::WindowBg, [0.055, 0.055, 0.062, 0.95]);
    let _sc_cbg = ui.push_style_color(StyleColor::ChildBg, [1.0, 1.0, 1.0, 0.015]);
    let _sc_pbg = ui.push_style_color(StyleColor::PopupBg, [0.07, 0.07, 0.08, 0.98]);
    let _sc_bdr = ui.push_style_color(StyleColor::Border, [1.0, 1.0, 1.0, 0.08]);
    let _sc_txt = ui.push_style_color(StyleColor::Text, TEXT_HI);
    let _sc_txd = ui.push_style_color(StyleColor::TextDisabled, TEXT_LO);
    let _sc_fbg = ui.push_style_color(StyleColor::FrameBg, [1.0, 1.0, 1.0, 0.04]);
    let _sc_fbh = ui.push_style_color(StyleColor::FrameBgHovered, [1.0, 1.0, 1.0, 0.07]);
    let _sc_fba = ui.push_style_color(StyleColor::FrameBgActive, [1.0, 1.0, 1.0, 0.09]);
    let _sc_btn = ui.push_style_color(StyleColor::Button, [1.0, 1.0, 1.0, 0.05]);
    let _sc_bth = ui.push_style_color(StyleColor::ButtonHovered, [1.0, 1.0, 1.0, 0.09]);
    let _sc_bta = ui.push_style_color(StyleColor::ButtonActive, [1.0, 1.0, 1.0, 0.12]);
    let _sc_hdr = ui.push_style_color(StyleColor::Header, ACCENT_DIM);
    let _sc_hdh = ui.push_style_color(StyleColor::HeaderHovered, [1.0, 1.0, 1.0, 0.08]);
    let _sc_sep = ui.push_style_color(StyleColor::Separator, [1.0, 1.0, 1.0, 0.07]);
    let _sc_sgb = ui.push_style_color(StyleColor::ScrollbarBg, [0.0, 0.0, 0.0, 0.0]);
    let _sc_sgg = ui.push_style_color(StyleColor::ScrollbarGrab, [1.0, 1.0, 1.0, 0.12]);

    ui.window("AI Game Companion")
        .position([margin, margin], Condition::FirstUseEver)
        .size([win_w, win_h], Condition::FirstUseEver)
        .build(|| {
            let window_size = ui.content_region_avail();

            // ── Header: brand + provider ────────────────────────────────
            ui.text_colored(ACCENT, "\u{25C6}"); // Sage mark
            ui.same_line();
            ui.text_colored(TEXT_HI, "SAGE");

            let dropdown_height = 30.0 * scale;
            let combo_w = 132.0 * scale;

            if available_providers.len() > 1 {
                let current_label = format!("{current_provider}");
                ui.same_line_with_pos(window_size[0] - combo_w);
                ui.set_next_item_width(combo_w);
                if let Some(_combo) = ui.begin_combo("##provider", &current_label) {
                    for &p in &available_providers {
                        let label = format!("{p}");
                        let selected = p == current_provider;
                        if ui.selectable_config(&label).selected(selected).build() && !selected {
                            // Switching providers must cancel any in-flight request,
                            // otherwise the old provider's stream keeps appending to
                            // streaming_response while the user starts a new turn.
                            let mut state = STATE.lock();
                            let (old_provider, old_gen) = state.cancel_in_flight();
                            state.active_provider = p;
                            drop(state);
                            if old_provider != crate::provider::Provider::Gemini {
                                crate::proxy_client::send_cancel(old_gen);
                            }
                        }
                    }
                }
            } else if available_providers.is_empty() {
                ui.same_line_with_pos(window_size[0] - combo_w);
                ui.text_colored(ERROR_COLOR, "No provider");
            } else {
                // Exactly one provider: show it as a quiet right-aligned label.
                let label = format!("{current_provider}");
                let tw = ui.calc_text_size(&label)[0];
                ui.same_line_with_pos(window_size[0] - tw);
                ui.text_colored(TEXT_MID, &label);
            }

            ui.spacing();
            ui.separator();
            ui.spacing();

            // ── Chat history (scrollable) ───────────────────────────────
            let chat_height =
                window_size[1] - input_area_height - status_bar_height - dropdown_height;

            if let Some(_child) = ui
                .child_window("##chat_history")
                .size([0.0, chat_height])
                .begin()
            {
                let indent = 12.0 * scale;

                for (role, content) in &messages_snapshot {
                    draw_message(ui, *role, content, false, indent, scale);
                }

                // Streaming response in progress (with a soft caret).
                if is_loading && !streaming_snapshot.is_empty() {
                    let live = format!("{streaming_snapshot}\u{258C}");
                    draw_message(ui, MessageRole::Assistant, &live, true, indent, scale);
                    ui.set_scroll_here_y_with_ratio(1.0);
                } else if is_loading {
                    // Awaiting first token: a quiet "thinking" line.
                    let _c = ui.push_style_color(StyleColor::Text, TEXT_LO);
                    ui.text("Sage is thinking\u{2026}");
                }

                // Auto-scroll to bottom when new content arrives.
                if ui.scroll_y() >= ui.scroll_max_y() - 20.0 {
                    ui.set_scroll_here_y_with_ratio(1.0);
                }
            }

            ui.spacing();

            // ── Input row ───────────────────────────────────────────────
            let mut input_buf = input_snapshot;
            ui.input_text_multiline(
                "##input",
                &mut input_buf,
                [window_size[0] - btn_w - 12.0, input_h],
            )
            .build();

            // Enter (without Shift) sends.
            let enter_pressed = ui.is_key_pressed(imgui::Key::Enter) && !ui.io().key_shift;

            ui.same_line();
            if is_loading {
                // Cancel replaces Send while a request is in flight.
                if ui.button_with_size("Stop", [btn_w, input_h]) {
                    let mut state = STATE.lock();
                    let partial = (!state.streaming_response.is_empty())
                        .then(|| format!("{} [cancelled]", state.streaming_response));
                    let (provider, old_gen) = state.cancel_in_flight();
                    if let Some(p) = partial {
                        state.push_message(ChatMessage::new(MessageRole::Assistant, p));
                    }
                    state.error = Some("Cancelled.".into());
                    drop(state);
                    if provider != crate::provider::Provider::Gemini {
                        crate::proxy_client::send_cancel(old_gen);
                    }
                }
            } else {
                let send_enabled = !input_buf.trim().is_empty();
                let mut send_pressed = false;
                // Accent-filled Send button (scoped colour push).
                {
                    let _b = ui.push_style_color(StyleColor::Button, ACCENT);
                    let _bh =
                        ui.push_style_color(StyleColor::ButtonHovered, [0.95, 0.71, 0.31, 1.0]);
                    let _ba = ui.push_style_color(StyleColor::ButtonActive, [0.80, 0.57, 0.20, 1.0]);
                    let _bt = ui.push_style_color(StyleColor::Text, [0.043, 0.043, 0.051, 1.0]);
                    ui.enabled(send_enabled, || {
                        send_pressed = ui.button_with_size("Send", [btn_w, input_h]);
                    });
                }

                if (send_pressed || enter_pressed) && send_enabled {
                    let generation = {
                        let mut state = STATE.lock();
                        let text = state.input_buffer.trim().to_string();
                        if !text.is_empty() {
                            state.push_message(ChatMessage::new(MessageRole::User, text));
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
                        // Clear local buffer so write-back doesn't overwrite cleared state.
                        input_buf.clear();

                        let skip_screenshot = attach_screenshot
                            && current_provider == crate::provider::Provider::Openai;

                        if attach_screenshot && !skip_screenshot {
                            // Initiate hide-capture-show; the API call fires from
                            // lib.rs once capture completes.
                            let mut state = STATE.lock();
                            state.capture_pending = true;
                            state.capture_wait_frames = 2;
                            state.captured_screenshot = None;
                            state.send_pending_capture = true;
                            state.capture_generation = state.request_generation;
                            crate::CAPTURE_ACTIVE.store(true, std::sync::atomic::Ordering::Release);
                        } else {
                            if skip_screenshot {
                                STATE.lock().push_message(ChatMessage::new(
                                    MessageRole::Assistant,
                                    "(Screenshots not yet available for OpenAI)".into(),
                                ));
                            }
                            let messages = STATE.lock().messages.clone();
                            crate::spawn_api_request(gen, messages, None);
                        }
                    }
                }
            }

            // ── Footer controls: screenshot toggle + new chat ───────────
            ui.spacing();
            let mut attach = attach_screenshot;
            ui.checkbox("Attach screenshot", &mut attach);
            ui.same_line_with_pos(window_size[0] - 92.0 * scale);
            if ui.small_button("New chat") {
                let mut state = STATE.lock();
                state.messages.clear();
                state.error = None;
                let (provider, old_gen) = state.cancel_in_flight();
                drop(state);
                if provider != crate::provider::Provider::Gemini {
                    crate::proxy_client::send_cancel(old_gen);
                }
            }

            // Write back UI changes to state.
            {
                let mut state = STATE.lock();
                state.input_buffer = input_buf;
                state.attach_screenshot = attach;
            }

            // ── Status bar ──────────────────────────────────────────────
            ui.separator();
            if let Some(err) = &error_snapshot {
                ui.text_colored(ERROR_COLOR, "\u{25CF}");
                ui.same_line();
                ui.text_colored(TEXT_MID, format!("Error: {err}"));
            } else if is_loading {
                ui.text_colored(ACCENT, "\u{25CF}");
                ui.same_line();
                ui.text_colored(TEXT_MID, "Streaming\u{2026}");
            } else {
                ui.text_colored(OK_COLOR, "\u{25CF}");
                ui.same_line();
                ui.text_colored(TEXT_MID, "Ready  \u{00B7}  F9 toggle  \u{00B7}  F10 translate");
            }
        });
}

/// Draw a single chat turn: a coloured accent rail, a role label, and the
/// wrapped body. The rail is drawn beside (not behind) the text, so it needs
/// no draw-list channel splitting and renders correctly in immediate mode.
fn draw_message(ui: &Ui, role: MessageRole, content: &str, streaming: bool, indent: f32, scale: f32) {
    let (label, label_color, bar_color) = match role {
        MessageRole::User => ("YOU", TEXT_LO, ACCENT),
        MessageRole::Assistant => ("SAGE", ACCENT, SAGE_BAR),
    };
    let body_color = match role {
        MessageRole::User => TEXT_HI,
        MessageRole::Assistant => [0.85, 0.86, 0.88, 1.0],
    };

    let start = ui.cursor_screen_pos();

    ui.indent_by(indent);
    ui.group(|| {
        let _lc = ui.push_style_color(StyleColor::Text, label_color);
        ui.text(label);
        drop(_lc);
        let _bc = ui.push_style_color(StyleColor::Text, if streaming { ACCENT } else { body_color });
        // text_wrapped wraps at the child window's content-region edge.
        ui.text_wrapped(content);
    });
    ui.unindent_by(indent);

    // Accent rail beside the turn.
    let end = ui.cursor_screen_pos();
    let dl = ui.get_window_draw_list();
    dl.add_rect(
        [start[0], start[1]],
        [start[0] + 3.0 * scale, (end[1] - 8.0 * scale).max(start[1] + 4.0)],
        bar_color,
    )
    .filled(true)
    .rounding(2.0)
    .build();

    ui.spacing();
}
