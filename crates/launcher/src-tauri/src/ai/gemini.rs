//! Direct Gemini streaming client (no proxy). Builds a `streamGenerateContent`
//! request from the chat history, an optional system instruction, and an
//! optional inline PNG screenshot, then forwards each decoded text chunk to a
//! caller-supplied callback.

use std::time::Duration;

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

use super::ChatMessage;

const GEMINI_ENDPOINT: &str = "https://generativelanguage.googleapis.com/v1beta/models";
const MAX_STREAM_BYTES: usize = 2 * 1024 * 1024;
const MAX_OUTPUT_TOKENS: u32 = 4_096;
/// How much of a refused request's body is read for Google's reason.
const MAX_ERROR_BODY_BYTES: usize = 8 * 1024;
/// How much of a provider's own error text is shown to the user.
pub(super) const MAX_ERROR_MESSAGE_CHARS: usize = 300;

/// The Gemini API key. The model is resolved separately by `resolve_model`: the
/// one chosen in Settings, then the legacy `config.toml` value, then the default.
#[derive(Debug)]
pub(super) struct GeminiConfig {
    pub api_key: String,
}

#[derive(Default, Deserialize)]
struct LauncherConfig {
    #[serde(default)]
    api: ApiConfig,
}

#[derive(Default, Deserialize)]
struct ApiConfig {
    #[serde(default)]
    gemini: FileGeminiConfig,
}

#[derive(Default, Deserialize)]
struct FileGeminiConfig {
    #[serde(default, alias = "key")]
    api_key: String,
    #[serde(default)]
    model: String,
}

/// A request content part: either text or inline base64 image data. Serialized
/// untagged so each variant maps directly onto Gemini's `parts[]` schema.
#[derive(Serialize)]
#[serde(untagged)]
enum Part {
    Text { text: String },
    InlineData { inline_data: InlineData },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InlineData {
    mime_type: String,
    data: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    system_instruction: Option<SystemInstruction>,
    contents: Vec<Content>,
    generation_config: GenerationConfig,
}

#[derive(Serialize)]
struct SystemInstruction {
    parts: Vec<Part>,
}

#[derive(Serialize)]
struct Content {
    role: &'static str,
    parts: Vec<Part>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GenerationConfig {
    max_output_tokens: u32,
}

#[derive(Deserialize)]
struct GeminiResponse {
    #[serde(default)]
    candidates: Vec<Candidate>,
    #[serde(default, rename = "promptFeedback")]
    prompt_feedback: Option<PromptFeedback>,
}

#[derive(Deserialize)]
struct Candidate {
    // A candidate carrying only `finishReason` (safety block, token limit) has
    // no `content`. Making it required meant the frame failed to deserialize
    // and was discarded as "unparseable", so a blocked prompt surfaced as the
    // generic "Empty response from API." with no hint of why.
    #[serde(default)]
    content: CandidateContent,
    #[serde(default, rename = "finishReason")]
    finish_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct PromptFeedback {
    #[serde(rename = "blockReason")]
    block_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct CandidateContent {
    #[serde(default)]
    parts: Vec<ResponsePart>,
}

#[derive(Deserialize)]
struct ResponsePart {
    text: Option<String>,
}

/// The model used when neither Settings nor the legacy file names one. Older
/// Flash models are closed to newly created keys.
const DEFAULT_MODEL: &str = "gemini-3.6-flash";

/// Load the Gemini API key. It prefers OS secret storage (set via Settings),
/// falling back to a legacy `config.toml` next to the executable, so that file
/// is optional.
pub(super) fn load_config() -> Result<GeminiConfig, String> {
    let api_key = crate::secrets::gemini_key()
        .or_else(|| {
            let file = read_config_file();
            let key = file.api.gemini.api_key.trim();
            (!key.is_empty()).then(|| key.to_owned())
        })
        .ok_or_else(|| "Gemini API key is not set. Add it in Settings.".to_owned())?;
    Ok(GeminiConfig { api_key })
}

/// The model named in the legacy `config.toml`, trimmed, or `""`. Read on its
/// own so a missing key never hides it.
pub(super) fn file_model() -> String {
    read_config_file().api.gemini.model.trim().to_owned()
}

/// The model a request names: the Settings choice, else the legacy file's,
/// else the default. Blank values count as no choice.
pub(crate) fn resolve_model(settings_model: &str, file_model: &str) -> String {
    [settings_model.trim(), file_model.trim()]
        .into_iter()
        .find(|model| !model.is_empty())
        .unwrap_or(DEFAULT_MODEL)
        .to_owned()
}

/// Read the legacy `config.toml` next to the executable, if present. Missing or
/// malformed files (which could leak the key in a parse error) yield defaults.
fn read_config_file() -> LauncherConfig {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf))
        .map(|dir| dir.join("config.toml"))
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|source| toml::from_str(&source).ok())
        .unwrap_or_default()
}

/// Map a chat message role onto a Gemini content role (`user` / `model`).
fn gemini_role(role: &str) -> &'static str {
    match role {
        "assistant" | "model" => "model",
        _ => "user",
    }
}

/// Build the request body: one content per chat turn, the screenshot attached to
/// the most recent user turn, and the system prompt when there is one.
fn build_request(
    messages: &[ChatMessage],
    system_prompt: &str,
    screenshot: Option<String>,
) -> GeminiRequest {
    let mut contents: Vec<Content> = messages
        .iter()
        .map(|message| Content {
            role: gemini_role(&message.role),
            parts: vec![Part::Text {
                text: message.content.clone(),
            }],
        })
        .collect();

    if let Some(data) = screenshot {
        if let Some(last_user) = contents
            .iter_mut()
            .rev()
            .find(|content| content.role == "user")
        {
            last_user.parts.push(Part::InlineData {
                inline_data: InlineData {
                    mime_type: "image/png".to_owned(),
                    data,
                },
            });
        }
    }

    let system_instruction = if system_prompt.trim().is_empty() {
        None
    } else {
        Some(SystemInstruction {
            parts: vec![Part::Text {
                text: system_prompt.to_owned(),
            }],
        })
    };

    GeminiRequest {
        system_instruction,
        contents,
        generation_config: GenerationConfig {
            max_output_tokens: MAX_OUTPUT_TOKENS,
        },
    }
}

/// Stream a Gemini response, passing each complete Gemini text chunk to `on_chunk`.
///
/// `screenshot` is a base64-encoded PNG attached to the most recent user turn.
#[allow(clippy::too_many_lines)] // linear request-build + SSE-parse pipeline
pub(super) async fn stream<F>(
    messages: &[ChatMessage],
    system_prompt: &str,
    screenshot: Option<String>,
    model: &str,
    api_key: &str,
    mut on_chunk: F,
) -> Result<(), String>
where
    F: FnMut(String) -> Result<(), String>,
{
    if messages
        .iter()
        .all(|message| message.content.trim().is_empty())
    {
        return Err("Question cannot be empty.".to_owned());
    }
    validate_model(model)?;

    let request = build_request(messages, system_prompt, screenshot);
    let url = format!("{GEMINI_ENDPOINT}/{model}:streamGenerateContent?alt=sse");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_mins(2))
        .build()
        .map_err(|error| format!("failed to create HTTP client: {error}"))?;
    let response = client
        .post(url)
        .header("x-goog-api-key", api_key)
        .header("content-type", "application/json")
        .json(&request)
        .send()
        .await
        .map_err(|error| {
            if error.is_timeout() {
                "Request timed out. Try again.".to_owned()
            } else {
                format!("Network error: {error}")
            }
        })?;

    let status = response.status();
    if !status.is_success() {
        let body = read_error_body(response).await;
        return Err(http_error_message(status.as_u16(), &body, api_key));
    }

    let mut stream = response.bytes_stream();
    let mut buffer = Vec::new();
    let mut total_bytes = 0usize;
    let mut received_text = false;

    while let Some(result) = stream.next().await {
        let bytes = result.map_err(|error| format!("Stream error: {error}"))?;
        total_bytes = total_bytes
            .checked_add(bytes.len())
            .ok_or_else(|| "Response too large. Stream aborted.".to_owned())?;
        if total_bytes > MAX_STREAM_BYTES {
            return Err("Response too large. Stream aborted.".to_owned());
        }
        buffer.extend_from_slice(&bytes);
        received_text |= process_sse_lines(&mut buffer, &mut on_chunk)?;
    }

    if !buffer.is_empty() {
        buffer.push(b'\n');
        received_text |= process_sse_lines(&mut buffer, &mut on_chunk)?;
    }

    if received_text {
        Ok(())
    } else {
        Err("Empty response from API.".to_owned())
    }
}

/// Read at most `MAX_ERROR_BODY_BYTES` of a refused response, stopping early
/// instead of draining the whole stream. A read error ends it with what arrived.
async fn read_error_body(response: reqwest::Response) -> String {
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(Ok(bytes)) = stream.next().await {
        body.extend_from_slice(&bytes);
        if body.len() >= MAX_ERROR_BODY_BYTES {
            break;
        }
    }
    body.truncate(MAX_ERROR_BODY_BYTES);
    String::from_utf8_lossy(&body).into_owned()
}

/// What a refused request tells the user: the status, Google's own reason when
/// the body carries one (the key blanked out, cut to a readable length), then
/// what to do about it.
fn http_error_message(status: u16, body: &str, api_key: &str) -> String {
    let mut out = format!("Gemini refused the request (HTTP {status})");
    let message = stream_error_message(body)
        .map(|message| {
            let message = if api_key.is_empty() {
                message
            } else {
                message.replace(api_key, "[key]")
            };
            message
                .trim()
                .chars()
                .take(MAX_ERROR_MESSAGE_CHARS)
                .collect::<String>()
        })
        .filter(|message| !message.is_empty());
    if let Some(message) = message {
        out.push_str(": ");
        out.push_str(&message);
    }
    if !out.ends_with(['.', '!', '?']) {
        out.push('.');
    }
    let hint = match status {
        400 | 404 => {
            Some("If this model isn't available for your key, choose another in Settings.")
        }
        401 | 403 => Some("Check the Gemini key in Settings."),
        429 => Some("Rate limited. Try again later."),
        500..=599 => Some("Gemini is having trouble. Try again."),
        _ => None,
    };
    if let Some(hint) = hint {
        out.push(' ');
        out.push_str(hint);
    }
    out
}

pub(crate) fn validate_model(model: &str) -> Result<(), String> {
    if model.is_empty()
        || !model.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '.' | '_')
        })
    {
        return Err("Invalid Gemini model name. Use ASCII letters, digits, hyphens, dots and underscores only.".to_owned());
    }
    Ok(())
}

fn process_sse_lines<F>(buffer: &mut Vec<u8>, on_chunk: &mut F) -> Result<bool, String>
where
    F: FnMut(String) -> Result<(), String>,
{
    let mut received_text = false;
    while let Some(newline_position) = buffer.iter().position(|&byte| byte == b'\n') {
        let line_bytes = buffer[..newline_position].to_vec();
        buffer.drain(..=newline_position);
        let Ok(line) = String::from_utf8(line_bytes) else {
            tracing::warn!("SSE: non-UTF-8 line dropped");
            continue;
        };
        let Some(json) = line.trim().strip_prefix("data: ") else {
            continue;
        };

        if let Ok(response) = serde_json::from_str::<GeminiResponse>(json) {
            // Tell the user WHY nothing came back, rather than letting a safety
            // block or a token-limit stop fall through to "Empty response".
            if let Some(reason) = response
                .prompt_feedback
                .as_ref()
                .and_then(|feedback| feedback.block_reason.as_deref())
            {
                return Err(format!("Gemini blocked this request ({reason})."));
            }
            let blocked = response
                .candidates
                .iter()
                .filter_map(|candidate| candidate.finish_reason.as_deref())
                .find(|reason| !matches!(*reason, "STOP" | "MAX_TOKENS"))
                .map(str::to_owned);
            let truncated = response
                .candidates
                .iter()
                .any(|candidate| candidate.finish_reason.as_deref() == Some("MAX_TOKENS"));
            let text = response
                .candidates
                .into_iter()
                .flat_map(|candidate| candidate.content.parts)
                .filter_map(|part| part.text)
                .collect::<String>();
            if text.is_empty() {
                if let Some(message) = stream_error_message(json) {
                    return Err(format!("API error: {message}"));
                }
                if let Some(reason) = blocked {
                    return Err(format!("Gemini stopped early ({reason})."));
                }
                if truncated {
                    return Err("Gemini hit its output limit before writing anything.".to_owned());
                }
            } else {
                received_text = true;
                on_chunk(text)?;
            }
        } else {
            if let Some(message) = stream_error_message(json) {
                return Err(format!("API error: {message}"));
            }
            tracing::debug!("SSE: skipping unparseable JSON chunk");
        }
    }
    Ok(received_text)
}

fn stream_error_message(json: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    value
        .get("error")?
        .get("message")?
        .as_str()
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::print_stdout,
        reason = "a panic is how a test reports a failed assumption, and the scans print what they counted"
    )]

    use super::{
        build_request, http_error_message, process_sse_lines, resolve_model, stream_error_message,
        validate_model, ChatMessage,
    };

    #[test]
    fn resolve_model_precedence() {
        for (settings, file, expected) in [
            ("", "", "gemini-3.6-flash"),
            ("", "gemini-x", "gemini-x"),
            ("gemini-y", "gemini-x", "gemini-y"),
            ("  ", " gemini-x ", "gemini-x"),
        ] {
            assert_eq!(
                resolve_model(settings, file),
                expected,
                "settings {settings:?}, file {file:?}"
            );
        }
    }

    #[test]
    fn http_errors_explain_themselves() {
        const KEY: &str = "AIzaTESTKEY123";
        let body =
            |message: &str| serde_json::json!({ "error": { "message": message } }).to_string();
        let model_hint = "If this model isn't available for your key, choose another in Settings.";
        let key_hint = "Check the Gemini key in Settings.";

        let cases = [
            (
                403,
                body("API key not valid."),
                vec!["HTTP 403", "API key not valid.", key_hint],
            ),
            (
                401,
                body("Unauthorised."),
                vec!["HTTP 401", "Unauthorised.", key_hint],
            ),
            (
                404,
                body("models/gemini-nope-1 is not found."),
                vec!["HTTP 404", "gemini-nope-1", model_hint],
            ),
            (
                400,
                body("Bad model."),
                vec!["HTTP 400", "Bad model.", model_hint],
            ),
            (
                429,
                body("Quota exceeded."),
                vec!["HTTP 429", "Rate limited. Try again later."],
            ),
            (
                503,
                body("Overloaded."),
                vec!["HTTP 503", "Gemini is having trouble. Try again."],
            ),
            (418, body("Teapot."), vec!["HTTP 418", "Teapot."]),
        ];
        for (status, body, expected) in cases {
            let out = http_error_message(status, &body, KEY);
            println!("{out}");
            assert!(out.starts_with("Gemini refused the request (HTTP "));
            for part in expected {
                assert!(out.contains(part), "{out:?} should contain {part:?}");
            }
            assert!(!out.contains("config.toml"));
        }

        let teapot = http_error_message(418, &body("Teapot."), KEY);
        assert!(!teapot.contains("Settings") && !teapot.contains("Try again"));

        let server = http_error_message(500, "<html>not json</html>", KEY);
        println!("{server}");
        assert!(server.contains("HTTP 500"));
        assert!(server.contains("Gemini is having trouble. Try again."));
        assert!(!server.contains(": "), "no message part without a message");
        assert!(!server.contains("config.toml"));

        let long = http_error_message(400, &body(&"\u{e9}".repeat(1_000)), KEY);
        assert!(long.contains(&"\u{e9}".repeat(300)));
        assert!(!long.contains(&"\u{e9}".repeat(301)));
        assert!(!long.contains("config.toml"));

        let leaked = http_error_message(400, &body(&format!("API key {KEY} is not valid.")), KEY);
        println!("{leaked}");
        assert!(leaked.contains("API key [key] is not valid."));
        assert!(!leaked.contains(KEY));
        assert!(!leaked.contains("config.toml"));
    }

    #[test]
    fn validate_model_messages_name_no_file() {
        let error = validate_model("../x").expect_err("a path is not a model name");
        println!("{error}");
        assert!(error.contains("Gemini model name"));
        assert!(!error.contains("config.toml"));
        assert!(validate_model("gemini-3.6-flash").is_ok());
    }

    fn turn(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: role.to_owned(),
            content: content.to_owned(),
        }
    }

    #[test]
    fn request_carries_no_tools() {
        let chat = build_request(
            &[turn("user", "q1"), turn("model", "a1"), turn("user", "q2")],
            "Be brief.",
            Some("QUJD".into()),
        );
        let translation = build_request(
            &[turn("user", "Translate the text in this image.")],
            "You translate on-screen text.",
            Some("QUJD".into()),
        );
        for request in [chat, translation] {
            let value = serde_json::to_value(&request).expect("the request serialises");
            let text = value.to_string();
            println!("{text}");
            assert!(value.get("tools").is_none(), "the request carries tools");
            assert!(!text.contains(concat!("google", "_search")));
        }
    }

    #[test]
    fn no_search_tool_in_sources() {
        for needle in [concat!("google", "_search"), concat!("Google", "Search")] {
            let (files, count) = crate::util::count_in_sources(needle, None);
            println!("scanned {files} files, found {count} occurrence(s) of {needle}");
            assert!(files > 0, "the source scan found no files");
            assert_eq!(count, 0, "no Gemini request may carry the search tool");
        }
    }

    #[test]
    fn buffers_split_utf8_and_emits_complete_text_chunks() {
        let line =
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"hello \u{e9}\"}]}}]}\n";
        let bytes = line.as_bytes();
        let split = bytes.iter().position(|byte| *byte == 0xc3).unwrap_or(1) + 1;
        let mut buffer = bytes[..split].to_vec();
        let mut chunks = Vec::new();

        assert!(!process_sse_lines(&mut buffer, &mut |chunk| {
            chunks.push(chunk);
            Ok(())
        })
        .expect("partial line should be buffered"));
        buffer.extend_from_slice(&bytes[split..]);
        assert!(process_sse_lines(&mut buffer, &mut |chunk| {
            chunks.push(chunk);
            Ok(())
        })
        .expect("complete line should parse"));
        assert_eq!(chunks, ["hello \u{e9}"]);
    }

    #[test]
    fn rejects_unsafe_model_names() {
        assert!(validate_model("gemini-2.5-flash").is_ok());
        assert!(validate_model("../model").is_err());
    }

    #[test]
    fn detects_streamed_api_errors() {
        let json = r#"{"error":{"message":"quota exceeded"}}"#;
        assert_eq!(
            stream_error_message(json).as_deref(),
            Some("quota exceeded")
        );
    }
}
