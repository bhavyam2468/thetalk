use crate::theme::ThemeMode;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApiFormat {
    OpenAi,
    Anthropic,
}

impl Default for ApiFormat {
    fn default() -> Self {
        Self::OpenAi
    }
}

impl ApiFormat {
    pub fn label(self) -> &'static str {
        match self {
            Self::OpenAi => "OpenAI-compatible",
            Self::Anthropic => "Anthropic Messages",
        }
    }

    pub fn short_label(self) -> &'static str {
        match self {
            Self::OpenAi => "OpenAI",
            Self::Anthropic => "Anthropic",
        }
    }

    pub fn toggle(self) -> Self {
        match self {
            Self::OpenAi => Self::Anthropic,
            Self::Anthropic => Self::OpenAi,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ProviderPreset {
    pub id: &'static str,
    pub label: &'static str,
    pub format: ApiFormat,
    pub base_url: &'static str,
    pub default_model: &'static str,
}

pub const PRESETS: &[ProviderPreset] = &[
    ProviderPreset {
        id: "ollama",
        label: "Ollama",
        format: ApiFormat::OpenAi,
        base_url: "http://localhost:11434/v1",
        default_model: "",
    },
    ProviderPreset {
        id: "llamacpp",
        label: "llama.cpp",
        format: ApiFormat::OpenAi,
        base_url: "http://localhost:8080/v1",
        default_model: "",
    },
    ProviderPreset {
        id: "lmstudio",
        label: "LM Studio",
        format: ApiFormat::OpenAi,
        base_url: "http://localhost:1234/v1",
        default_model: "",
    },
    ProviderPreset {
        id: "vllm",
        label: "vLLM",
        format: ApiFormat::OpenAi,
        base_url: "http://localhost:8000/v1",
        default_model: "",
    },
    ProviderPreset {
        id: "openai",
        label: "OpenAI",
        format: ApiFormat::OpenAi,
        base_url: "https://api.openai.com/v1",
        default_model: "gpt-4o-mini",
    },
    ProviderPreset {
        id: "anthropic",
        label: "Anthropic",
        format: ApiFormat::Anthropic,
        base_url: "https://api.anthropic.com/v1",
        default_model: "claude-sonnet-4-5",
    },
    ProviderPreset {
        id: "openrouter",
        label: "OpenRouter",
        format: ApiFormat::OpenAi,
        base_url: "https://openrouter.ai/api/v1",
        default_model: "google/gemini-2.5-flash",
    },
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ProviderProfile {
    pub id: String,
    pub name: String,
    pub format: ApiFormat,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub vision: bool,
    pub builtin: bool,
}

impl Default for ProviderProfile {
    fn default() -> Self {
        Self {
            id: "ollama".to_string(),
            name: "Ollama".to_string(),
            format: ApiFormat::OpenAi,
            base_url: "http://localhost:11434/v1".to_string(),
            api_key: String::new(),
            model: String::new(),
            vision: true,
            builtin: true,
        }
    }
}

impl ProviderProfile {
    pub fn from_preset(preset: &ProviderPreset) -> Self {
        Self {
            id: preset.id.to_string(),
            name: preset.label.to_string(),
            format: preset.format,
            base_url: preset.base_url.to_string(),
            api_key: String::new(),
            model: preset.default_model.to_string(),
            vision: true,
            builtin: true,
        }
    }
}

fn default_base_url() -> String {
    "http://localhost:11434/v1".to_string()
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub active_profile_id: String,
    #[serde(default)]
    pub profiles: Vec<ProviderProfile>,
    #[serde(default = "default_base_url")]
    pub base_url: String,
    /// Optional API key — leave empty for keyless local endpoints (Ollama, llama.cpp, LM Studio, vLLM).
    #[serde(default)]
    pub api_key: String,
    /// Optional model name — if empty, auto-discovered from `GET {base_url}/models`.
    #[serde(default)]
    pub model: String,
    /// Whether image attachments / scanned PDF pages are sent visually to the model.
    #[serde(default = "default_true")]
    pub vision: bool,
    #[serde(default)]
    pub theme: ThemeMode,
}

impl Default for AppConfig {
    fn default() -> Self {
        let env_url = std::env::var("LLM_BASE_URL")
            .or_else(|_| std::env::var("OPENAI_BASE_URL"))
            .unwrap_or_else(|_| "http://localhost:11434/v1".to_string());
        let env_key = std::env::var("LLM_API_KEY")
            .or_else(|_| std::env::var("OPENAI_API_KEY"))
            .unwrap_or_default();
        let env_model = std::env::var("LLM_MODEL").unwrap_or_default();

        let mut cfg = Self {
            active_profile_id: String::new(),
            profiles: Vec::new(),
            base_url: env_url,
            api_key: env_key,
            model: env_model,
            vision: true,
            theme: ThemeMode::Dark,
        };
        cfg.normalize_profiles();
        cfg
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
                if cfg.base_url.is_empty() && cfg.profiles.is_empty() {
                    cfg.base_url = "http://localhost:11434/v1".to_string();
                }
                cfg.normalize_profiles();
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

    /// Ensures all built-in profiles exist and keeps legacy top-level fields
    /// synchronized with `active_profile_id`.
    pub fn normalize_profiles(&mut self) {
        let had_profiles = !self.profiles.is_empty();

        // Ensure every built-in preset exists (appended only if missing).
        for preset in PRESETS {
            if !self.profiles.iter().any(|p| p.id == preset.id) {
                self.profiles.push(ProviderProfile::from_preset(preset));
            }
        }

        if !had_profiles {
            // Migrate legacy flat settings.json or env overrides onto the matching profile
            // (or a new custom profile if URL doesn't match any preset).
            let norm_url = normalize_url(&self.base_url);
            if let Some(matched) = self
                .profiles
                .iter_mut()
                .find(|p| normalize_url(&p.base_url) == norm_url)
            {
                if !self.api_key.is_empty() {
                    matched.api_key = self.api_key.clone();
                }
                if !self.model.is_empty() {
                    matched.model = self.model.clone();
                }
                matched.vision = self.vision;
                self.active_profile_id = matched.id.clone();
            } else if !norm_url.is_empty() {
                let format = if norm_url.contains("anthropic.com") {
                    ApiFormat::Anthropic
                } else {
                    ApiFormat::OpenAi
                };
                self.profiles.push(ProviderProfile {
                    id: "custom-1".to_string(),
                    name: "Custom".to_string(),
                    format,
                    base_url: norm_url,
                    api_key: self.api_key.clone(),
                    model: self.model.clone(),
                    vision: self.vision,
                    builtin: false,
                });
                self.active_profile_id = "custom-1".to_string();
            }
        }

        if self.active_profile_id.trim().is_empty()
            || !self.profiles.iter().any(|p| p.id == self.active_profile_id)
        {
            self.active_profile_id = self
                .profiles
                .first()
                .map(|p| p.id.clone())
                .unwrap_or_else(|| "ollama".to_string());
        }

        if had_profiles {
            // Mirror active profile fields cleanly; if caller mutated top-level fields directly
            // before calling normalize_profiles, keep active profile as source of truth when non-empty.
            self.sync_from_active_profile();
        } else {
            self.sync_from_active_profile();
        }
    }

    pub fn active_profile(&self) -> &ProviderProfile {
        self.profiles
            .iter()
            .find(|p| p.id == self.active_profile_id)
            .or_else(|| self.profiles.first())
            .expect("profiles always seeded")
    }

    pub fn active_profile_mut(&mut self) -> &mut ProviderProfile {
        let idx = self
            .profiles
            .iter()
            .position(|p| p.id == self.active_profile_id)
            .unwrap_or(0);
        &mut self.profiles[idx]
    }

    pub fn active_format(&self) -> ApiFormat {
        self.active_profile().format
    }

    pub fn sync_from_active_profile(&mut self) {
        let p = self.active_profile().clone();
        self.base_url = p.base_url;
        self.api_key = p.api_key;
        self.model = p.model;
        self.vision = p.vision;
    }

    pub fn sync_to_active_profile(&mut self) {
        let base_url = self.base_url.trim().to_string();
        let api_key = self.api_key.trim().to_string();
        let model = self.model.trim().to_string();
        let vision = self.vision;
        let prof = self.active_profile_mut();
        prof.base_url = if base_url.is_empty() && prof.id == "ollama" {
            "http://localhost:11434/v1".to_string()
        } else {
            base_url
        };
        prof.api_key = api_key;
        prof.model = model;
        prof.vision = vision;
    }

    pub fn select_profile(&mut self, id: &str) -> bool {
        if self.profiles.iter().any(|p| p.id == id) {
            self.active_profile_id = id.to_string();
            self.sync_from_active_profile();
            true
        } else {
            false
        }
    }

    pub fn set_active_format(&mut self, format: ApiFormat) {
        self.active_profile_mut().format = format;
    }

    pub fn set_active_vision(&mut self, vision: bool) {
        self.vision = vision;
        self.active_profile_mut().vision = vision;
    }

    pub fn create_custom_profile(
        &mut self,
        name: &str,
        base_url: &str,
        api_key: &str,
        format: ApiFormat,
        model: &str,
        vision: bool,
    ) -> String {
        let mut n = 1usize;
        let id = loop {
            let candidate = format!("custom-{}", n);
            if !self.profiles.iter().any(|p| p.id == candidate) {
                break candidate;
            }
            n += 1;
        };
        let clean_name = if name.trim().is_empty() {
            format!("Custom {}", n)
        } else {
            name.trim().to_string()
        };
        self.profiles.push(ProviderProfile {
            id: id.clone(),
            name: clean_name,
            format,
            base_url: base_url.trim().to_string(),
            api_key: api_key.trim().to_string(),
            model: model.trim().to_string(),
            vision,
            builtin: false,
        });
        self.select_profile(&id);
        id
    }

    pub fn rename_profile(&mut self, id: &str, new_name: &str) -> bool {
        let trimmed = new_name.trim();
        if trimmed.is_empty() {
            return false;
        }
        if let Some(p) = self.profiles.iter_mut().find(|p| p.id == id && !p.builtin) {
            p.name = trimmed.to_string();
            true
        } else {
            false
        }
    }

    pub fn delete_profile(&mut self, id: &str) -> bool {
        let Some(pos) = self.profiles.iter().position(|p| p.id == id && !p.builtin) else {
            return false;
        };
        self.profiles.remove(pos);
        if self.active_profile_id == id {
            let fallback = self
                .profiles
                .first()
                .map(|p| p.id.clone())
                .unwrap_or_else(|| "ollama".to_string());
            self.select_profile(&fallback);
        }
        true
    }

    pub fn custom_profiles(&self) -> Vec<&ProviderProfile> {
        self.profiles.iter().filter(|p| !p.builtin).collect()
    }

    pub fn active_profile_label(&self) -> &str {
        &self.active_profile().name
    }

    pub fn active_profile_is_custom(&self) -> bool {
        !self.active_profile().builtin
    }

    pub fn all_profiles(&self) -> &[ProviderProfile] {
        &self.profiles
    }

    pub fn add_custom_profile(
        &mut self,
        name: &str,
        format: ApiFormat,
        base_url: &str,
        api_key: &str,
    ) -> ProviderProfile {
        self.create_custom_profile(name, base_url, api_key, format, "", self.vision);
        self.active_profile().clone()
    }

    pub fn delete_custom_profile(&mut self, id: &str) -> bool {
        self.delete_profile(id)
    }

    pub fn update_active_profile(
        &mut self,
        name: &str,
        format: ApiFormat,
        base_url: &str,
        api_key: &str,
        model: &str,
    ) {
        let trimmed_name = name.trim();
        let trimmed_url = base_url.trim();
        let trimmed_key = api_key.trim();
        let trimmed_model = model.trim();
        {
            let prof = self.active_profile_mut();
            if !prof.builtin && !trimmed_name.is_empty() {
                prof.name = trimmed_name.to_string();
            }
            prof.format = format;
            prof.base_url = if trimmed_url.is_empty() && prof.id == "ollama" {
                "http://localhost:11434/v1".to_string()
            } else {
                trimmed_url.to_string()
            };
            prof.api_key = trimmed_key.to_string();
            prof.model = trimmed_model.to_string();
        }
        self.sync_from_active_profile();
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
        let u = normalize_url(&self.base_url);
        if u.is_empty() {
            match self.active_format() {
                ApiFormat::Anthropic => "https://api.anthropic.com/v1".to_string(),
                ApiFormat::OpenAi => "http://localhost:11434/v1".to_string(),
            }
        } else {
            u
        }
    }
}

fn normalize_url(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switching_between_openai_anthropic_and_custom_profiles_preserves_each_profile() {
        let mut cfg = AppConfig::default();
        assert!(cfg.select_profile("openai"));
        cfg.api_key = "sk-openai-1234".into();
        cfg.sync_to_active_profile();

        assert!(cfg.select_profile("anthropic"));
        assert_eq!(cfg.active_format(), ApiFormat::Anthropic);
        assert_eq!(cfg.api_key, "");
        cfg.api_key = "sk-ant-9999".into();
        cfg.sync_to_active_profile();

        let cid1 = cfg.create_custom_profile(
            "Work Proxy",
            "https://proxy.example.com/v1",
            "sk-work-1",
            ApiFormat::OpenAi,
            "deepseek-chat",
            false,
        );
        let cid2 = cfg.create_custom_profile(
            "Claude Mirror",
            "https://claude-proxy.local/v1",
            "sk-mirror-2",
            ApiFormat::Anthropic,
            "claude-opus-4-5",
            true,
        );
        assert_eq!(cfg.active_profile_id, cid2);
        assert_eq!(cfg.base_url, "https://claude-proxy.local/v1");
        assert_eq!(cfg.api_key, "sk-mirror-2");
        assert_eq!(cfg.active_format(), ApiFormat::Anthropic);

        assert!(cfg.select_profile(&cid1));
        assert_eq!(cfg.base_url, "https://proxy.example.com/v1");
        assert_eq!(cfg.api_key, "sk-work-1");
        assert!(!cfg.vision);

        assert!(cfg.select_profile("openai"));
        assert_eq!(cfg.api_key, "sk-openai-1234");

        assert!(cfg.select_profile("anthropic"));
        assert_eq!(cfg.api_key, "sk-ant-9999");

        assert!(cfg.rename_profile(&cid1, "Company Gateway"));
        assert_eq!(
            cfg.profiles.iter().find(|p| p.id == cid1).unwrap().name,
            "Company Gateway"
        );
        assert!(cfg.delete_profile(&cid1));
        assert!(!cfg.delete_profile("openai")); // built-in protected
    }

    #[test]
    fn migrates_legacy_flat_json_cleanly() {
        let legacy = r#"{
            "base_url": "https://my-local-gateway.internal/v1",
            "api_key": "secret-legacy",
            "model": "qwen3:32b",
            "theme": "light"
        }"#;
        let mut cfg: AppConfig = serde_json::from_str(legacy).unwrap();
        cfg.normalize_profiles();
        assert_eq!(cfg.theme, ThemeMode::Light);
        assert_eq!(cfg.base_url, "https://my-local-gateway.internal/v1");
        assert_eq!(cfg.api_key, "secret-legacy");
        assert_eq!(cfg.model, "qwen3:32b");
        assert_eq!(cfg.custom_profiles().len(), 1);
    }
}
