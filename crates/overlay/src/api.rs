use std::time::Duration;

use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};

use crate::config::CONFIG;
use crate::state::{ChatMessage, MessageRole};

/// Max number of messages to send to the API. Older messages are trimmed to avoid
/// huge payloads (especially with screenshots) and runaway token costs.
const MAX_HISTORY_MESSAGES: usize = 50;

static CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
});

// --- Gemini API request structs ---

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

/// Send the full conversation history to the Gemini API.
/// `screenshot` is an optional base64-encoded PNG to attach to the last user message.
pub async fn send_message(
    messages: Vec<ChatMessage>,
    screenshot: Option<String>,
) -> Result<String, String> {
    let config = &CONFIG.api;

    if config.key.is_empty() {
        return Err("No API key configured. Add your key to config.toml.".into());
    }

    // Trim conversation history to avoid huge payloads and token costs.
    // Ensure the trimmed slice starts with a User message (API requirement).
    let messages = if messages.len() > MAX_HISTORY_MESSAGES {
        let mut start = messages.len() - MAX_HISTORY_MESSAGES;
        if messages[start].role == MessageRole::Assistant {
            start += 1;
        }
        messages[start..].to_vec()
    } else {
        messages
    };

    // Build contents array
    let mut contents: Vec<Content> = Vec::with_capacity(messages.len());

    for (i, msg) in messages.iter().enumerate() {
        let role = match msg.role {
            MessageRole::User => "user",
            MessageRole::Assistant => "model",
        };

        let is_last_user = msg.role == MessageRole::User && i == messages.len() - 1;

        let parts = if is_last_user {
            if let Some(ref screenshot_data) = screenshot {
                vec![
                    Part::Text {
                        text: msg.content.clone(),
                    },
                    Part::InlineData {
                        inline_data: InlineData {
                            mime_type: "image/png".into(),
                            data: screenshot_data.clone(),
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

    let system_instruction = if config.system_prompt.is_empty() {
        None
    } else {
        Some(SystemInstruction {
            parts: vec![Part::Text {
                text: config.system_prompt.clone(),
            }],
        })
    };

    let request = GeminiRequest {
        system_instruction,
        contents,
        generation_config: GenerationConfig {
            max_output_tokens: config.max_tokens,
        },
    };

    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent",
        config.model
    );

    let response = CLIENT
        .post(&url)
        .header("x-goog-api-key", &config.key)
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

    let body: GeminiResponse = response.json().await.map_err(|_| {
        "Unexpected API response.".to_string()
    })?;

    // Extract text from response candidates
    let text = body
        .candidates
        .into_iter()
        .flat_map(|c| c.content.parts)
        .filter_map(|p| p.text)
        .collect::<Vec<_>>()
        .join("\n");

    if text.is_empty() {
        Err("Empty response from API.".into())
    } else {
        Ok(text)
    }
}
