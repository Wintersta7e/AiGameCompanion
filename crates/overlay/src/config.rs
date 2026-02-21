use std::path::PathBuf;
use std::sync::OnceLock;

use hudhook::windows::Win32::Foundation::{HINSTANCE, HMODULE};
use hudhook::windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use once_cell::sync::Lazy;
use serde::Deserialize;

/// Which graphics API to hook into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GraphicsApi {
    Dx12,
    Dx11,
    Dx9,
    #[serde(alias = "opengl")]
    Opengl,
}

impl std::fmt::Display for GraphicsApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Dx12 => write!(f, "DX12"),
            Self::Dx11 => write!(f, "DX11"),
            Self::Dx9 => write!(f, "DX9"),
            Self::Opengl => write!(f, "OpenGL"),
        }
    }
}

/// Saved in DllMain before spawning the hook thread.
pub static DLL_HINSTANCE: OnceLock<HINSTANCE> = OnceLock::new();

pub(crate) fn dll_directory() -> Option<PathBuf> {
    let hinstance = *DLL_HINSTANCE.get()?;
    let hmodule = HMODULE(hinstance.0);
    let mut buf = [0u16; 512];
    let len = unsafe { GetModuleFileNameW(hmodule, &mut buf) } as usize;
    if len == 0 {
        return None;
    }
    let path = PathBuf::from(String::from_utf16_lossy(&buf[..len]));
    path.parent().map(|p| p.to_path_buf())
}

#[derive(Deserialize, Clone, Default)]
pub struct Config {
    #[serde(default)]
    pub api: ApiConfig,
    #[serde(default)]
    pub overlay: OverlayConfig,
    #[serde(default)]
    pub capture: CaptureConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
    #[serde(default)]
    pub translation: TranslationConfig,
    #[serde(default)]
    pub games: Vec<GameEntry>,
}

#[derive(Deserialize, Clone)]
pub struct GameEntry {
    #[serde(default)]
    pub name: Option<String>,
    pub process: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SafetyFilter {
    /// Block nothing (BLOCK_NONE) -- least restrictive
    Off,
    /// Block only high-probability harmful content
    BlockHigh,
    /// Block medium and above (Gemini default for older models)
    BlockMedium,
    /// Block low and above -- most restrictive
    BlockLow,
}

impl SafetyFilter {
    /// Returns the Gemini API threshold string.
    pub fn as_api_str(self) -> &'static str {
        match self {
            Self::Off => "BLOCK_NONE",
            Self::BlockHigh => "BLOCK_ONLY_HIGH",
            Self::BlockMedium => "BLOCK_MEDIUM_AND_ABOVE",
            Self::BlockLow => "BLOCK_LOW_AND_ABOVE",
        }
    }
}

fn default_safety_filter() -> SafetyFilter { SafetyFilter::Off }

#[derive(Deserialize, Clone)]
pub struct ApiConfig {
    #[serde(default)]
    pub key: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    #[serde(default = "default_system_prompt")]
    pub system_prompt: String,
    #[serde(default = "default_safety_filter")]
    pub safety_filter: SafetyFilter,
}

#[derive(Deserialize, Clone)]
#[allow(dead_code)]
pub struct OverlayConfig {
    /// Force a specific graphics API. If omitted, auto-detects from loaded modules.
    pub graphics_api: Option<GraphicsApi>,
    #[serde(default = "default_hotkey")]
    pub hotkey: String,
    #[serde(default = "default_width")]
    pub width: f32,
    #[serde(default = "default_height")]
    pub height: f32,
    #[serde(default = "default_opacity")]
    pub opacity: f32,
    #[serde(default = "default_font_size")]
    pub font_size: f32,
    #[serde(default = "default_translate_hotkey")]
    pub translate_hotkey: String,
}

#[derive(Deserialize, Clone)]
#[allow(dead_code)]
pub struct CaptureConfig {
    #[serde(default = "default_capture_enabled")]
    pub enabled: bool,
    #[serde(default = "default_max_width")]
    pub max_width: u32,
    #[serde(default = "default_quality")]
    pub quality: u8,
}

#[derive(Deserialize, Clone)]
pub struct LoggingConfig {
    #[serde(default = "default_logging_enabled")]
    pub enabled: bool,
    /// Override log directory. Default: "logs/" next to the DLL.
    pub directory: Option<String>,
}

/// Which backend to use for translation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TranslationProvider {
    Gemini,
    Local,
}

fn default_translation_provider() -> TranslationProvider { TranslationProvider::Gemini }

#[derive(Deserialize, Clone)]
pub struct LocalModelConfig {
    #[serde(default = "default_local_endpoint")]
    pub endpoint: String,
    #[serde(default = "default_local_model")]
    pub model: String,
}

fn default_local_endpoint() -> String { "http://localhost:11434/v1/chat/completions".into() }
fn default_local_model() -> String { "minicpm-v".into() }

impl Default for LocalModelConfig {
    fn default() -> Self {
        Self {
            endpoint: default_local_endpoint(),
            model: default_local_model(),
        }
    }
}

#[derive(Deserialize, Clone)]
pub struct TranslationConfig {
    #[serde(default = "default_translation_enabled")]
    pub enabled: bool,
    #[serde(default = "default_target_language")]
    pub target_language: String,
    #[serde(default = "default_translation_provider")]
    pub provider: TranslationProvider,
    #[serde(default)]
    pub local: LocalModelConfig,
}

fn default_translation_enabled() -> bool { true }
fn default_target_language() -> String { "English".into() }

impl Default for TranslationConfig {
    fn default() -> Self {
        Self {
            enabled: default_translation_enabled(),
            target_language: default_target_language(),
            provider: default_translation_provider(),
            local: LocalModelConfig::default(),
        }
    }
}

fn default_logging_enabled() -> bool { true }

fn default_model() -> String { "gemini-2.5-flash".into() }
fn default_max_tokens() -> u32 { 1024 }
fn default_system_prompt() -> String {
    "You are a helpful game companion. Be concise and direct. When you see a screenshot, \
     describe what you observe and provide actionable advice."
        .into()
}
fn default_hotkey() -> String { "F9".into() }
fn default_width() -> f32 { 500.0 }
fn default_height() -> f32 { 400.0 }
fn default_opacity() -> f32 { 0.85 }
fn default_font_size() -> f32 { 16.0 }
fn default_translate_hotkey() -> String { "F10".into() }
fn default_capture_enabled() -> bool { true }
fn default_max_width() -> u32 { 1920 }
fn default_quality() -> u8 { 85 }

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            key: String::new(),
            model: default_model(),
            max_tokens: default_max_tokens(),
            system_prompt: default_system_prompt(),
            safety_filter: default_safety_filter(),
        }
    }
}

impl Default for OverlayConfig {
    fn default() -> Self {
        Self {
            graphics_api: None,
            hotkey: default_hotkey(),
            width: default_width(),
            height: default_height(),
            opacity: default_opacity(),
            font_size: default_font_size(),
            translate_hotkey: default_translate_hotkey(),
        }
    }
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            enabled: default_capture_enabled(),
            max_width: default_max_width(),
            quality: default_quality(),
        }
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            enabled: default_logging_enabled(),
            directory: None,
        }
    }
}

/// Parse a hotkey string (e.g. "F9", "F10") into a Windows virtual key code.
pub fn parse_vk_code(hotkey: &str) -> Option<i32> {
    match hotkey.to_uppercase().as_str() {
        "F1" => Some(0x70),
        "F2" => Some(0x71),
        "F3" => Some(0x72),
        "F4" => Some(0x73),
        "F5" => Some(0x74),
        "F6" => Some(0x75),
        "F7" => Some(0x76),
        "F8" => Some(0x77),
        "F9" => Some(0x78),
        "F10" => Some(0x79),
        "F11" => Some(0x7A),
        "F12" => Some(0x7B),
        _ => None,
    }
}

pub static CONFIG: Lazy<Config> = Lazy::new(|| {
    let Some(dir) = dll_directory() else {
        eprintln!("[companion] Could not determine DLL directory, using defaults");
        return Config::default();
    };

    let config_path = dir.join("config.toml");
    match std::fs::read_to_string(&config_path) {
        Ok(contents) => match toml::from_str(&contents) {
            Ok(config) => config,
            Err(e) => {
                eprintln!("[companion] Failed to parse config.toml: {e}");
                Config::default()
            }
        },
        Err(_) => {
            eprintln!(
                "[companion] config.toml not found at {}, using defaults",
                config_path.display()
            );
            Config::default()
        }
    }
});
