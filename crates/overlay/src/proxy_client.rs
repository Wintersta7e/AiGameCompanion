use std::collections::{HashMap, HashSet};
use std::time::Duration;

use futures_util::StreamExt;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};

use crate::config::CONFIG;
use crate::provider::Provider;
use crate::state::{sanitize_for_imgui, ChatMessage, MessageRole, STATE};

/// Maximum total bytes to accumulate from an SSE stream before aborting.
const MAX_STREAM_BYTES: usize = 2 * 1024 * 1024; // 2 MB

/// Max number of conversation messages to send to the proxy.
const MAX_HISTORY_MESSAGES: usize = 50;

/// Shared HTTP client for all proxy requests. Reuses connections to localhost.
static PROXY_CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .unwrap_or_else(|e| {
            tracing::warn!("Proxy HTTP client builder failed: {e}, using default");
            reqwest::Client::new()
        })
});

// --- Proxy request/response types ---

#[derive(Serialize)]
struct ProxyRequest {
    request_id: u64,
    messages: Vec<ProxyMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    screenshot: Option<String>,
    system_prompt: String,
    provider: String,
    model: String,
    max_tokens: u32,
}

#[derive(Serialize)]
struct ProxyMessage {
    role: &'static str,
    content: String,
}

#[derive(Deserialize)]
struct ProxyChunk {
    text: Option<String>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct HealthResponse {
    providers: HashMap<String, bool>,
}

// --- History formatting ---

/// Format conversation history for CLI providers. All but the last message
/// become context; the final message stands alone as the current question.
///
/// Single message: returns its content directly.
/// Multiple messages:
/// ```text
/// [Conversation history]
/// User: first message
/// Sage: first response
///
/// [Current message]
/// latest question
/// ```
fn build_history_text(messages: &[ChatMessage]) -> String {
    if messages.is_empty() {
        return String::new();
    }
    if messages.len() == 1 {
        return messages[0].content.clone();
    }

    let history = &messages[..messages.len() - 1];
    let current = &messages[messages.len() - 1];

    let mut out = String::from("[Conversation history]\n");
    for msg in history {
        let label = match msg.role {
            MessageRole::User => "User",
            MessageRole::Assistant => "Sage",
        };
        out.push_str(label);
        out.push_str(": ");
        out.push_str(&msg.content);
        out.push('\n');
    }
    out.push_str("\n[Current message]\n");
    out.push_str(&current.content);
    out
}

// --- Public API ---

/// Send a chat request through the localhost proxy, streaming SSE chunks back
/// into `STATE.streaming_response`. Returns the full accumulated text on success.
///
/// `generation` is checked each chunk to support cancellation.
pub async fn send_proxy_message(
    provider: Provider,
    messages: Vec<ChatMessage>,
    screenshot: Option<String>,
    generation: u64,
) -> Result<String, String> {
    // Read proxy connection details from state.
    let (port, token) = {
        let state = STATE.lock();
        let port = state.proxy_port.ok_or("Proxy not available. Launch the game from the launcher.")?;
        let token = state
            .proxy_token
            .clone()
            .ok_or("Proxy token missing.")?;
        (port, token)
    };

    // Filter out translation messages and trim history.
    let messages: Vec<ChatMessage> = messages
        .into_iter()
        .filter(|m| !m.is_translation)
        .collect();
    let messages = if messages.len() > MAX_HISTORY_MESSAGES {
        let mut start = messages.len() - MAX_HISTORY_MESSAGES;
        // Skip leading Assistant messages (conversation must start with User).
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

    let history_text = build_history_text(&messages);

    // Pick the right model from config based on provider.
    let model = match provider {
        Provider::Claude => CONFIG.api.claude.model.clone(),
        Provider::Openai => CONFIG.api.openai.model.clone(),
        Provider::Gemini => {
            // Gemini goes through api.rs directly, not the proxy.
            return Err("Gemini does not use the proxy. Use api::send_message instead.".into());
        }
    };

    // Build system prompt (prepend game name if detected).
    let game_name = STATE.lock().game_name.clone();
    let system_prompt = match game_name {
        Some(name) => format!("The user is currently playing {name}. {}", CONFIG.api.system_prompt),
        None => CONFIG.api.system_prompt.clone(),
    };

    let provider_str = match provider {
        Provider::Claude => "claude",
        Provider::Openai => "openai",
        Provider::Gemini => unreachable!(),
    };

    let request = ProxyRequest {
        request_id: generation,
        messages: vec![ProxyMessage {
            role: "user",
            content: history_text,
        }],
        screenshot,
        system_prompt,
        provider: provider_str.to_string(),
        model,
        max_tokens: CONFIG.api.max_tokens,
    };

    let url = format!("http://127.0.0.1:{port}/chat");

    let response = PROXY_CLIENT
        .post(&url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .json(&request)
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                "Proxy request timed out. Try again.".to_string()
            } else if e.is_connect() {
                "Cannot connect to proxy. Is the launcher running?".to_string()
            } else {
                format!("Proxy network error: {e}")
            }
        })?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(match status.as_u16() {
            401 => "Proxy authentication failed. Restart the game from the launcher.".into(),
            502 | 503 => format!("Provider unavailable (HTTP {status}): {body}"),
            code => format!("Proxy error (HTTP {code}): {body}"),
        });
    }

    // Stream SSE chunks using a byte buffer (same approach as api.rs).
    let mut stream = response.bytes_stream();
    let mut byte_buf: Vec<u8> = Vec::new();
    let mut full_text = String::new();
    let mut total_bytes: usize = 0;

    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.map_err(|e| format!("Proxy stream error: {e}"))?;
        total_bytes += chunk.len();
        if total_bytes > MAX_STREAM_BYTES {
            return Err("Response too large. Stream aborted.".into());
        }
        byte_buf.extend_from_slice(chunk.as_ref());

        // Process complete lines from the byte buffer.
        process_proxy_sse_lines(&mut byte_buf, &mut full_text, generation)?;
    }

    // Drain any remaining data after stream ends.
    if !byte_buf.is_empty() {
        byte_buf.push(b'\n');
        process_proxy_sse_lines(&mut byte_buf, &mut full_text, generation)?;
    }

    if full_text.is_empty() {
        Err("Empty response from proxy.".into())
    } else {
        Ok(full_text)
    }
}

/// Process complete SSE lines from the byte buffer.
///
/// Expected formats:
/// - `data: {"text": "chunk"}` -- text chunk to append
/// - `data: {"error": "msg"}` -- error from the provider
/// - `data: [DONE]` -- end of stream
fn process_proxy_sse_lines(
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
                tracing::warn!("Proxy SSE: non-UTF-8 line dropped");
                continue;
            }
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let Some(data) = line.strip_prefix("data: ") else {
            continue;
        };

        // End-of-stream sentinel.
        if data == "[DONE]" {
            break;
        }

        match serde_json::from_str::<ProxyChunk>(data) {
            Ok(chunk) => {
                // Check for error first.
                if let Some(err) = chunk.error {
                    return Err(format!("Provider error: {err}"));
                }

                if let Some(text) = chunk.text {
                    if !text.is_empty() {
                        let clean = sanitize_for_imgui(&text);
                        full_text.push_str(&clean);

                        let mut state = STATE.lock();
                        if state.request_generation != generation {
                            return Err("Cancelled".into());
                        }
                        state.streaming_response.push_str(&clean);
                    }
                }
            }
            Err(_) => {
                tracing::debug!("Proxy SSE: skipping unparseable chunk: {data}");
            }
        }
    }
    Ok(())
}

/// Fire-and-forget cancellation request to the proxy.
/// Spawns on the tokio runtime; does not await a response.
pub fn send_cancel(generation: u64) {
    let Some(rt) = crate::RUNTIME.as_ref() else {
        return;
    };

    let (port, token) = {
        let state = STATE.lock();
        let Some(port) = state.proxy_port else { return };
        let Some(ref token) = state.proxy_token else { return };
        (port, token.clone())
    };

    rt.spawn(async move {
        let url = format!("http://127.0.0.1:{port}/cancel");
        let body = serde_json::json!({ "request_id": generation });

        let result = PROXY_CLIENT
            .post(&url)
            .header("Authorization", format!("Bearer {token}"))
            .json(&body)
            .send()
            .await;

        if let Err(e) = result {
            tracing::debug!("Proxy cancel request failed (non-fatal): {e}");
        }
    });
}

/// Query the proxy's health endpoint and return the set of available providers.
/// Returns an empty set on any failure (timeout, connection refused, bad response).
pub async fn check_health(port: u16, token: &str) -> HashSet<Provider> {
    let url = format!("http://127.0.0.1:{port}/health");

    let response = match PROXY_CLIENT
        .get(&url)
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!("Proxy health check failed: {e}");
            return HashSet::new();
        }
    };

    if !response.status().is_success() {
        tracing::debug!("Proxy health check returned HTTP {}", response.status());
        return HashSet::new();
    }

    let health: HealthResponse = match response.json().await {
        Ok(h) => h,
        Err(e) => {
            tracing::debug!("Proxy health response parse error: {e}");
            return HashSet::new();
        }
    };

    let mut available = HashSet::new();
    for (name, is_up) in &health.providers {
        if *is_up {
            match name.as_str() {
                "claude" => { available.insert(Provider::Claude); }
                "openai" => { available.insert(Provider::Openai); }
                "gemini" => { available.insert(Provider::Gemini); }
                other => {
                    tracing::debug!("Proxy health: unknown provider '{other}', skipping");
                }
            }
        }
    }
    available
}
