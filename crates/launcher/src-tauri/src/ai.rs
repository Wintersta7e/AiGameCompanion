//! Direct AI-provider clients used by the external overlay companion.

use std::time::Duration;

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

const GEMINI_ENDPOINT: &str = "https://generativelanguage.googleapis.com/v1beta/models";
const MAX_STREAM_BYTES: usize = 2 * 1024 * 1024;
const MAX_OUTPUT_TOKENS: u32 = 4_096;

#[derive(Debug)]
pub struct GeminiConfig {
    pub api_key: String,
    pub model: String,
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiRequest {
    contents: Vec<Content>,
    generation_config: GenerationConfig,
    tools: Vec<Tool>,
}

#[derive(Serialize)]
struct Content {
    role: &'static str,
    parts: Vec<RequestPart>,
}

#[derive(Serialize)]
struct RequestPart {
    text: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GenerationConfig {
    max_output_tokens: u32,
}

#[derive(Serialize)]
struct Tool {
    google_search: GoogleSearch,
}

#[derive(Serialize)]
struct GoogleSearch {}

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

/// Read the transitional Gemini configuration stored next to the executable.
pub fn load_gemini_config() -> Result<GeminiConfig, String> {
    let executable = std::env::current_exe()
        .map_err(|error| format!("failed to locate launcher executable: {error}"))?;
    let directory = executable
        .parent()
        .ok_or_else(|| "launcher executable has no parent directory".to_owned())?;
    let path = directory.join("config.toml");
    let source = std::fs::read_to_string(&path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let config: LauncherConfig = toml::from_str(&source)
        .map_err(|error| format!("failed to parse {}: {error}", path.display()))?;
    let api_key = config.api.gemini.api_key.trim().to_owned();
    let model = config.api.gemini.model.trim().to_owned();

    if api_key.is_empty() {
        return Err("Gemini API key is missing. Set api.gemini.api_key in config.toml.".to_owned());
    }
    if model.is_empty() {
        return Err("Gemini model is missing. Set api.gemini.model in config.toml.".to_owned());
    }

    Ok(GeminiConfig { api_key, model })
}

/// Stream a Gemini response, passing each complete Gemini text chunk to `on_chunk`.
pub async fn stream_gemini<F>(
    prompt: String,
    api_key: String,
    model: String,
    mut on_chunk: F,
) -> Result<(), String>
where
    F: FnMut(String) -> Result<(), String>,
{
    if prompt.trim().is_empty() {
        return Err("Question cannot be empty.".to_owned());
    }
    validate_model(&model)?;

    let request = GeminiRequest {
        contents: vec![Content {
            role: "user",
            parts: vec![RequestPart { text: prompt }],
        }],
        generation_config: GenerationConfig {
            max_output_tokens: MAX_OUTPUT_TOKENS,
        },
        tools: vec![Tool {
            google_search: GoogleSearch {},
        }],
    };
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
        return Err(match status.as_u16() {
            400 => "Bad request. Try a shorter message.".to_owned(),
            403 => "Invalid API key. Check config.toml.".to_owned(),
            429 => "Rate limited. Try again later.".to_owned(),
            500 | 503 => "API server error. Try again.".to_owned(),
            code => format!("API error (HTTP {code})."),
        });
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

fn validate_model(model: &str) -> Result<(), String> {
    if model.is_empty()
        || !model.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '.' | '_')
        })
    {
        return Err("Invalid model name in config.toml. Use ASCII alphanumeric, hyphens, dots, and underscores only.".to_owned());
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
    use super::{process_sse_lines, stream_error_message, validate_model};

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
