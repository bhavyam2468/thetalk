use crate::theme::ThemeMode;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy)]
pub struct ProviderPreset {
    #[allow(dead_code)]
    pub id: &'static str,
    pub label: &'static str,
    pub base_url: &'static str,
    pub default_model: &'static str,
}

pub const PRESETS: &[ProviderPreset] = &[
    ProviderPreset {
        id: "ollama",
        label: "Ollama",
        base_url: "http://localhost:11434/v1",
        default_model: "",
    },
    ProviderPreset {
        id: "llamacpp",
        label: "llama.cpp",
        base_url: "http://localhost:8080/v1",
        default_model: "",
    },
    ProviderPreset {
        id: "lmstudio",
        label: "LM Studio",
        base_url: "http://localhost:1234/v1",
        default_model: "",
    },
    ProviderPreset {
        id: "vllm",
        label: "vLLM",
        base_url: "http://localhost:8000/v1",
        default_model: "",
    },
    ProviderPreset {
        id: "openrouter",
        label: "OpenRouter",
        base_url: "https://openrouter.ai/api/v1",
        default_model: "google/gemini-2.5-flash",
    },
    ProviderPreset {
        id: "openai",
        label: "OpenAI",
        base_url: "https://api.openai.com/v1",
        default_model: "gpt-4o-mini",
    },
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub base_url: String,
    /// Optional API key — leave empty for keyless local endpoints (Ollama, llama.cpp, LM Studio, vLLM).
    pub api_key: String,
    /// Optional model name — if empty, auto-discovered from `GET {base_url}/models`.
    pub model: String,
    pub theme: ThemeMode,
}

impl Default for AppConfig {
    fn default() -> Self {
        let base_url = std::env::var("LLM_BASE_URL")
            .or_else(|_| std::env::var("OPENAI_BASE_URL"))
            .unwrap_or_else(|_| "http://localhost:11434/v1".to_string());
        let api_key = std::env::var("LLM_API_KEY")
            .or_else(|_| std::env::var("OPENAI_API_KEY"))
            .unwrap_or_default();
        let model = std::env::var("LLM_MODEL").unwrap_or_default();
        Self {
            base_url,
            api_key,
            model,
            theme: ThemeMode::Dark,
        }
    }
}

impl AppConfig {
    pub fn config_path() -> PathBuf {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        base.join("thetalk").join("settings.json")
    }

    pub fn load() -> Self {
        let path = Self::config_path();
        if let Ok(raw) = fs::read_to_string(&path) {
            if let Ok(mut cfg) = serde_json::from_str::<Self>(&raw) {
                cfg.base_url = cfg.base_url.trim().to_string();
                if cfg.base_url.is_empty() {
                    cfg.base_url = "http://localhost:11434/v1".to_string();
                }
                return cfg;
            }
        }
        Self::default()
    }

    pub fn save(&self) -> Result<(), String> {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let body = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        let tmp_path = path.with_extension("json.tmp");
        fs::write(&tmp_path, body.as_bytes()).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o600));
        }
        fs::rename(&tmp_path, &path).map_err(|e| e.to_string())?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn masked_key(&self) -> String {
        let trimmed = self.api_key.trim();
        if trimmed.is_empty() {
            String::new()
        } else {
            let chars: Vec<char> = trimmed.chars().collect();
            if chars.len() <= 4 {
                "••••".to_string()
            } else {
                let tail: String = chars[chars.len() - 4..].iter().collect();
                format!("••••••••{}", tail)
            }
        }
    }

    pub fn normalized_base_url(&self) -> String {
        let u = self.base_url.trim().trim_end_matches('/');
        if u.is_empty() {
            "http://localhost:11434/v1".to_string()
        } else {
            u.to_string()
        }
    }
}
