use std::fmt;

use serde::Deserialize;

/// Which AI provider to use for the main chat.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    #[default]
    Gemini,
    Claude,
    Openai,
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Gemini => write!(f, "Gemini"),
            Self::Claude => write!(f, "Claude"),
            Self::Openai => write!(f, "OpenAI"),
        }
    }
}
