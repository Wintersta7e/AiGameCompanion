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
    pub games: Vec<GameEntry>,
}

#[derive(Deserialize, Clone)]
pub struct GameEntry {
    #[serde(default)]
    pub name: Option<String>,
    pub process: String,
}

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
