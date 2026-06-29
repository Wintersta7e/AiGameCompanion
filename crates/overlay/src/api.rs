use std::time::Duration;

use futures_util::StreamExt;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};

use crate::config::CONFIG;
use crate::state::{sanitize_for_imgui, ChatMessage, MessageRole, STATE};

/// Maximum total bytes to accumulate from an SSE stream before aborting.
const MAX_STREAM_BYTES: usize = 2 * 1024 * 1024; // 2 MB

/// Max number of messages to send to the API. Older messages are trimmed to avoid
/// huge payloads (especially with screenshots) and runaway token costs.
const MAX_HISTORY_MESSAGES: usize = 50;

static CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .unwrap_or_else(|e| {
            tracing::warn!("HTTP client builder failed: {e}, using default (no timeout)");
            reqwest::Client::new()
        })
});

// --- Gemini API request structs ---

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    system_instruction: Option<SystemInstruction>,
    contents: Vec<Content>,
    generation_config: GenerationConfig,
    tools: Vec<Tool>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    safety_settings: Vec<SafetySetting>,
}

#[derive(Serialize)]
struct SafetySetting {
    category: &'static str,
    threshold: &'static str,
}

#[derive(Serialize)]
struct Tool {
    google_search: GoogleSearch,
}

#[derive(Serialize)]
struct GoogleSearch {}

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
struct GenerationConfig {
    max_output_tokens: u32,
}

// --- Gemini API response structs ---

#[derive(Deserialize)]
struct GeminiResponse {
    #[serde(default)]
    candidates: Vec<Candidate>,
}

#[derive(Deserialize)]
struct Candidate {
    content: CandidateContent,
}

#[derive(Deserialize)]
struct CandidateContent {
    #[serde(default)]
    parts: Vec<ResponsePart>,
}

#[derive(Deserialize)]
struct ResponsePart {
    text: Option<String>,
}

// --- Public API ---

/// Send the full conversation history to the Gemini streaming API.
/// Text chunks are written to `STATE.streaming_response` as they arrive.
/// `generation` is checked each chunk to support cancellation.
pub async fn send_message(
    messages: Vec<ChatMessage>,
    screenshot: Option<String>,
    generation: u64,
) -> Result<String, String> {
    let config = &CONFIG.api;

    if config.gemini.key.is_empty() {
        return Err("No API key configured. Add your key to config.toml.".into());
    }

    // Trim conversation history to avoid huge payloads and token costs.
    // Exclude translation messages and ensure the slice starts with a User message.
    let messages: Vec<ChatMessage> = messages.into_iter().filter(|m| !m.is_translation).collect();
    let messages = if messages.len() > MAX_HISTORY_MESSAGES {
        let mut start = messages.len() - MAX_HISTORY_MESSAGES;
        // Skip leading Assistant messages (API requires User first)
        while start < messages.len() && messages[start].role == MessageRole::Assistant {
            start += 1;
        }
        if start >= messages.len() {
            return Err("No user messages in conversation history.".into());
        }
        messages[start..].to_vec()
    } else {
        messages
    };

    // Build contents array. Take screenshot out of Option to avoid cloning ~2.7MB.
    let mut contents: Vec<Content> = Vec::with_capacity(messages.len());
    let last_user_idx = messages.iter().rposition(|m| m.role == MessageRole::User);
    let mut screenshot = screenshot;

    for (i, msg) in messages.iter().enumerate() {
        let role = match msg.role {
            MessageRole::User => "user",
            MessageRole::Assistant => "model",
        };

        let is_last_user = Some(i) == last_user_idx;

        let parts = if is_last_user {
            if let Some(screenshot_data) = screenshot.take() {
                vec![
                    Part::Text {
                        text: msg.content.clone(),
                    },
                    Part::InlineData {
                        inline_data: InlineData {
                            mime_type: "image/png".into(),
                            data: screenshot_data,
                        },
                    },
                ]
            } else {
                vec![Part::Text {
                    text: msg.content.clone(),
                }]
            }
        } else {
            vec![Part::Text {
                text: msg.content.clone(),
            }]
        };

        contents.push(Content { role, parts });
    }

    // Prepend game name to system prompt if detected.
    let game_name = STATE.lock().game_name.clone();
    let system_text = match game_name {
        Some(name) => format!(
            "The user is currently playing {name}. {}",
            config.system_prompt
        ),
        None => config.system_prompt.clone(),
    };

    let system_instruction = if system_text.is_empty() {
        None
    } else {
        Some(SystemInstruction {
            parts: vec![Part::Text { text: system_text }],
        })
    };

    // Build safety settings from config.
    use crate::config::SafetyFilter;
    let safety_settings = if config.safety_filter != SafetyFilter::BlockMedium {
        let threshold = config.safety_filter.as_api_str();
        vec![
            SafetySetting {
                category: "HARM_CATEGORY_HARASSMENT",
                threshold,
            },
            SafetySetting {
                category: "HARM_CATEGORY_HATE_SPEECH",
                threshold,
            },
            SafetySetting {
                category: "HARM_CATEGORY_SEXUALLY_EXPLICIT",
                threshold,
            },
            SafetySetting {
                category: "HARM_CATEGORY_DANGEROUS_CONTENT",
                threshold,
            },
        ]
    } else {
        // BlockMedium is the API default -- omit to reduce payload
        vec![]
    };

    let request = GeminiRequest {
        system_instruction,
        contents,
        generation_config: GenerationConfig {
            max_output_tokens: config.max_tokens,
        },
        tools: vec![Tool {
            google_search: GoogleSearch {},
        }],
        safety_settings,
    };

    // Validate model name to prevent URL path traversal (ASCII only)
    if !config
        .gemini
        .model
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_')
    {
        return Err("Invalid model name in config.toml. Use ASCII alphanumeric, hyphens, dots, underscores only.".into());
    }

    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{}:streamGenerateContent?alt=sse",
        config.gemini.model
    );

    let response = CLIENT
        .post(&url)
        .header("x-goog-api-key", &config.gemini.key)
        .header("content-type", "application/json")
        .json(&request)
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                "Request timed out. Try again.".to_string()
            } else {
                format!("Network error: {e}")
            }
        })?;

    let status = response.status();
    if !status.is_success() {
        return Err(match status.as_u16() {
            400 => "Bad request. Try a shorter message.".into(),
            403 => "Invalid API key. Check config.toml.".into(),
            429 => "Rate limited. Free tier allows ~250 requests/day.".into(),
            500 | 503 => "API server error. Try again.".into(),
            code => format!("API error (HTTP {code})."),
        });
    }

    // Stream SSE chunks. Use a byte buffer to avoid corrupting multi-byte
    // UTF-8 characters that may be split across TCP chunks.
    let mut stream = response.bytes_stream();
    let mut byte_buf: Vec<u8> = Vec::new();
    let mut full_text = String::new();
    let mut total_bytes: usize = 0;

    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.map_err(|e| format!("Stream error: {e}"))?;
        total_bytes += chunk.len();
        if total_bytes > MAX_STREAM_BYTES {
            return Err("Response too large. Stream aborted.".into());
        }
        byte_buf.extend_from_slice(chunk.as_ref());

        // Process complete lines from the byte buffer
        process_sse_lines(&mut byte_buf, &mut full_text, generation)?;
    }

    // Drain any remaining data in the byte buffer after stream ends
    if !byte_buf.is_empty() {
        // Add a synthetic newline so the line gets processed
        byte_buf.push(b'\n');
        process_sse_lines(&mut byte_buf, &mut full_text, generation)?;
    }

    if full_text.is_empty() {
        Err("Empty response from API.".into())
    } else {
        Ok(full_text)
    }
}

/// Process complete lines from the SSE byte buffer, extracting text chunks.
fn process_sse_lines(
    byte_buf: &mut Vec<u8>,
    full_text: &mut String,
    generation: u64,
) -> Result<(), String> {
    while let Some(newline_pos) = byte_buf.iter().position(|&b| b == b'\n') {
        let line_bytes = byte_buf[..newline_pos].to_vec();
        byte_buf.drain(..=newline_pos);

        let line = match String::from_utf8(line_bytes) {
            Ok(s) => s,
            Err(_) => {
                tracing::warn!("SSE: non-UTF-8 line dropped");
                continue;
            }
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        if let Some(json_str) = line.strip_prefix("data: ") {
            match serde_json::from_str::<GeminiResponse>(json_str) {
                Ok(resp) => {
                    let chunk_text: String = resp
                        .candidates
                        .into_iter()
                        .flat_map(|c| c.content.parts)
                        .filter_map(|p| p.text)
                        .collect::<Vec<_>>()
                        .join("");

                    if chunk_text.is_empty() {
                        // An error object (e.g. {"error":{...}}) deserializes as
                        // an empty-candidates response, so check for one here
                        // rather than silently dropping the stream's error.
                        if let Some(msg) = stream_error_message(json_str) {
                            return Err(format!("API error: {msg}"));
                        }
                    } else {
                        let clean = sanitize_for_imgui(&chunk_text);
                        full_text.push_str(&clean);

                        let mut state = STATE.lock();
                        if state.request_generation != generation {
                            return Err("Cancelled".into());
                        }
                        state.streaming_response.push_str(&clean);
                    }
                }
                Err(_) => {
                    // Non-`GeminiResponse` JSON: still surface an error object
                    // if present, otherwise skip the unparseable chunk.
                    if let Some(msg) = stream_error_message(json_str) {
                        return Err(format!("API error: {msg}"));
                    }
                    tracing::debug!("SSE: skipping unparseable JSON chunk");
                }
            }
        }
    }
    Ok(())
}

/// Extract `error.message` from a Gemini SSE `data:` payload, if present.
/// Gemini can stream an error object (e.g. quota exceeded) mid-response that
/// otherwise deserializes as an empty `GeminiResponse`.
fn stream_error_message(json_str: &str) -> Option<String> {
    let val: serde_json::Value = serde_json::from_str(json_str).ok()?;
    val.get("error")?
        .get("message")?
        .as_str()
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    // process_sse_lines briefly locks the global STATE (generation check +
    // streaming_response append). Serialize the STATE-touching tests so a
    // parallel test cannot change request_generation underneath us.
    static SERIAL: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

    // A real line captured from gemini-2.5-flash :streamGenerateContent?alt=sse.
    const GEMINI_LINE: &str = "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"PONG\"}],\"role\":\"model\"},\"finishReason\":\"STOP\",\"index\":0}],\"modelVersion\":\"gemini-2.5-flash\"}\n";

    #[test]
    fn extracts_text_from_data_line() {
        let _g = SERIAL.lock();
        STATE.lock().request_generation = 100;
        let mut buf = GEMINI_LINE.as_bytes().to_vec();
        let mut full = String::new();
        let r = process_sse_lines(&mut buf, &mut full, 100);
        assert!(r.is_ok());
        assert_eq!(full, "PONG");
        assert!(
            buf.is_empty(),
            "a complete line should be drained from the buffer"
        );
    }

    #[test]
    fn joins_multiple_parts_in_one_chunk() {
        let _g = SERIAL.lock();
        STATE.lock().request_generation = 100;
        let line = "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"foo\"},{\"text\":\"bar\"}]}}]}\n";
        let mut buf = line.as_bytes().to_vec();
        let mut full = String::new();
        assert!(process_sse_lines(&mut buf, &mut full, 100).is_ok());
        assert_eq!(full, "foobar");
    }

    #[test]
    fn buffers_incomplete_line_until_newline() {
        // A chunk that splits mid-line must leave the partial line buffered
        // (this is why the parser uses a byte buffer, not line-at-a-time reads).
        let mut buf = b"data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"par".to_vec();
        let original = buf.clone();
        let mut full = String::new();
        assert!(process_sse_lines(&mut buf, &mut full, 0).is_ok());
        assert!(full.is_empty());
        assert_eq!(buf, original, "incomplete line must remain in the buffer");
    }

    #[test]
    fn skips_blank_and_non_data_lines() {
        let mut buf = b"\n: keep-alive\nevent: message\n".to_vec();
        let mut full = String::new();
        assert!(process_sse_lines(&mut buf, &mut full, 0).is_ok());
        assert!(full.is_empty());
        assert!(buf.is_empty());
    }

    #[test]
    fn cancels_when_generation_changed_mid_stream() {
        let _g = SERIAL.lock();
        STATE.lock().request_generation = 5;
        // A stale generation (a newer request superseded this one).
        let mut buf = GEMINI_LINE.as_bytes().to_vec();
        let mut full = String::new();
        let r = process_sse_lines(&mut buf, &mut full, 4);
        assert_eq!(r, Err("Cancelled".to_string()));
    }

    #[test]
    fn surfaces_api_error_object_in_stream() {
        // Gemini can emit an error object as an SSE data line. It must surface
        // as an error, not be silently dropped.
        let line = "data: {\"error\":{\"code\":429,\"message\":\"quota exceeded\"}}\n";
        let mut buf = line.as_bytes().to_vec();
        let mut full = String::new();
        let r = process_sse_lines(&mut buf, &mut full, 0);
        assert_eq!(r, Err("API error: quota exceeded".to_string()));
    }
}
