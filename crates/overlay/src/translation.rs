use std::time::Duration;

use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};

use crate::config::{TranslationProvider, CONFIG};
use crate::logging;
use crate::state::{ChatMessage, MessageRole, STATE};

static LOCAL_CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
});

// --- OpenAI-compatible request/response structs ---

#[derive(Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<OaiMessage>,
    max_tokens: u32,
    stream: bool,
}

#[derive(Serialize)]
struct OaiMessage {
    role: &'static str,
    content: OaiContent,
}

#[derive(Serialize)]
#[serde(untagged)]
#[allow(dead_code)]
enum OaiContent {
    Text(String),
    Parts(Vec<OaiContentPart>),
}

#[derive(Serialize)]
#[serde(tag = "type")]
enum OaiContentPart {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image_url")]
    ImageUrl { image_url: ImageUrl },
}

#[derive(Serialize)]
struct ImageUrl {
    url: String,
}

#[derive(Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: ChoiceMessage,
}

#[derive(Deserialize)]
struct ChoiceMessage {
    content: String,
}

fn build_translation_prompt() -> String {
    format!(
        "Translate all foreign/non-English text visible on screen to {}. \
         If no foreign text is visible, say so briefly. \
         Be concise -- just provide the translations, grouped logically.",
        CONFIG.translation.target_language
    )
}

async fn send_local_translation(screenshot: String) -> Result<String, String> {
    let config = &CONFIG.translation.local;
    let prompt = build_translation_prompt();

    let request = ChatCompletionRequest {
        model: config.model.clone(),
        messages: vec![OaiMessage {
            role: "user",
            content: OaiContent::Parts(vec![
                OaiContentPart::Text { text: prompt },
                OaiContentPart::ImageUrl {
                    image_url: ImageUrl {
                        url: format!("data:image/png;base64,{screenshot}"),
                    },
                },
            ]),
        }],
        max_tokens: CONFIG.api.max_tokens,
        stream: false,
    };

    let response = LOCAL_CLIENT
        .post(&config.endpoint)
        .header("content-type", "application/json")
        .json(&request)
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                "Local model timed out. Is it running?".to_string()
            } else if e.is_connect() {
                format!(
                    "Cannot connect to local model at {}. Is Ollama/LM Studio running?",
                    config.endpoint
                )
            } else {
                format!("Local model error: {e}")
            }
        })?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(format!("Local model error (HTTP {status}): {body}"));
    }

    let resp: ChatCompletionResponse = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse local model response: {e}"))?;

    resp.choices
        .into_iter()
        .next()
        .map(|c| c.message.content)
        .ok_or_else(|| "Empty response from local model.".into())
}

/// Spawn a translation request on the tokio runtime.
/// Dispatches to Gemini or local model based on config.
pub fn spawn_translate_request(
    gen: u64,
    messages: Vec<ChatMessage>,
    screenshot: Option<String>,
) {
    match CONFIG.translation.provider {
        TranslationProvider::Gemini => {
            // Gemini path: inject translation prompt into the last user message,
            // then use the standard API flow.
            let mut msgs = messages;
            if let Some(last) = msgs.last_mut() {
                if last.role == MessageRole::User {
                    last.content = build_translation_prompt();
                }
            }
            crate::spawn_api_request(gen, msgs, screenshot);
        }
        TranslationProvider::Local => {
            crate::RUNTIME.spawn(async move {
                let result = match screenshot {
                    Some(data) => send_local_translation(data).await,
                    None => Err("No screenshot captured for translation.".into()),
                };

                let mut state = STATE.lock();
                if state.request_generation != gen {
                    return; // cancelled
                }
                match result {
                    Ok(response) => {
                        let last_user = state
                            .messages
                            .iter()
                            .rev()
                            .find(|m| m.role == MessageRole::User)
                            .map(|m| m.content.clone())
                            .unwrap_or_default();
                        state.messages.push(ChatMessage {
                            role: MessageRole::Assistant,
                            content: response.clone(),
                        });
                        state.streaming_response.clear();
                        state.is_loading = false;
                        drop(state);
                        logging::log_exchange(&last_user, &response);
                    }
                    Err(err) => {
                        state.error = Some(err);
                        state.is_loading = false;
                    }
                }
            });
        }
    }
}
