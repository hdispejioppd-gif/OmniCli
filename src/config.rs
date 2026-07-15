use std::{collections::BTreeMap, fs, path::PathBuf};

use crate::plugin::PluginConfig;

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppConfig {
    pub data_dir: PathBuf,
    pub workspace: PathBuf,
    pub max_turns: u32,
    pub max_tool_output_bytes: usize,
    pub max_file_bytes: usize,
    pub shell_timeout_seconds: u64,
    pub provider: ProviderKind,
    pub openai: OpenAiConfig,
    pub anthropic: AnthropicConfig,
    pub ollama: OllamaConfig,
    pub lm_studio: LmStudioConfig,
    pub llama_cpp: LlamaCppConfig,
    pub openai_compatible: OpenAiCompatibleConfig,
    pub custom_providers: BTreeMap<String, CustomProviderConfig>,
    pub mcp: McpConfig,
    pub plugins: BTreeMap<String, PluginConfig>,
    pub profiles: BTreeMap<String, ProfileConfig>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProfileConfig {
    pub provider: Option<ProviderKind>,
    pub openai: Option<OpenAiConfig>,
    pub anthropic: Option<AnthropicConfig>,
    pub ollama: Option<OllamaConfig>,
    pub lm_studio: Option<LmStudioConfig>,
    pub llama_cpp: Option<LlamaCppConfig>,
    pub openai_compatible: Option<OpenAiCompatibleConfig>,
    pub custom_providers: Option<BTreeMap<String, CustomProviderConfig>>,
    pub max_turns: Option<u32>,
    pub max_tool_output_bytes: Option<usize>,
    pub max_file_bytes: Option<usize>,
    pub shell_timeout_seconds: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    #[default]
    Fake,
    #[serde(rename = "openai")]
    OpenAi,
    #[serde(rename = "anthropic")]
    Anthropic,
    #[serde(rename = "ollama")]
    Ollama,
    #[serde(rename = "lm-studio")]
    LmStudio,
    #[serde(rename = "llama-cpp")]
    LlamaCpp,
    #[serde(rename = "openai-compatible")]
    OpenAiCompatible,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OpenAiConfig {
    pub base_url: String,
    pub model: String,
    pub timeout_seconds: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AnthropicConfig {
    pub base_url: String,
    pub model: String,
    pub timeout_seconds: u64,
    pub api_version: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OllamaConfig {
    pub base_url: String,
    pub model: String,
    pub timeout_seconds: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LmStudioConfig {
    pub base_url: String,
    pub model: String,
    pub timeout_seconds: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LlamaCppConfig {
    pub base_url: String,
    pub model: String,
    pub timeout_seconds: u64,
    pub temperature: f32,
    pub n_predict: i32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OpenAiCompatibleConfig {
    pub base_url: String,
    pub model: String,
    pub timeout_seconds: u64,
    pub api_key_env: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct McpConfig {
    pub max_message_bytes: usize,
    pub servers: BTreeMap<String, McpStdioServerConfig>,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            max_message_bytes: 1024 * 1024,
            servers: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct McpStdioServerConfig {
    pub command: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<String>,
    pub startup_timeout_seconds: u64,
    pub call_timeout_seconds: u64,
}

impl Default for McpStdioServerConfig {
    fn default() -> Self {
        Self {
            command: PathBuf::new(),
            args: Vec::new(),
            env: Vec::new(),
            startup_timeout_seconds: 10,
            call_timeout_seconds: 30,
        }
    }
}

impl Default for AnthropicConfig {
    fn default() -> Self {
        Self {
            base_url: "https://api.anthropic.com/v1/".into(),
            model: "claude-sonnet-4-20250514".into(),
            timeout_seconds: 120,
            api_version: "2023-06-01".into(),
        }
    }
}

impl Default for OpenAiConfig {
    fn default() -> Self {
        Self {
            base_url: "https://api.openai.com/v1/".into(),
            model: "gpt-4.1-mini".into(),
            timeout_seconds: 120,
        }
    }
}

impl Default for OllamaConfig {
    fn default() -> Self {
        Self {
            base_url: "http://localhost:11434".into(),
            model: "llama3.1".into(),
            timeout_seconds: 120,
        }
    }
}

impl Default for LmStudioConfig {
    fn default() -> Self {
        Self {
            base_url: "http://localhost:1234/v1/".into(),
            model: "default".into(),
            timeout_seconds: 120,
        }
    }
}

impl Default for LlamaCppConfig {
    fn default() -> Self {
        Self {
            base_url: "http://localhost:8080".into(),
            model: "local".into(),
            timeout_seconds: 120,
            temperature: 0.7,
            n_predict: -1,
        }
    }
}

impl Default for OpenAiCompatibleConfig {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            model: String::new(),
            timeout_seconds: 120,
            api_key_env: "OPENAI_COMPATIBLE_API_KEY".into(),
        }
    }
}

/// A named OpenAI-compatible endpoint (OpenRouter, DeepSeek, Groq, Together,
/// vLLM -- any server that speaks `/chat/completions`).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CustomProviderConfig {
    pub base_url: String,
    pub model: String,
    pub timeout_seconds: u64,
    pub api_key_env: String,
}

impl Default for CustomProviderConfig {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            model: String::new(),
            timeout_seconds: 120,
            api_key_env: String::new(),
        }
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        let data_dir = ProjectDirs::from("dev", "omnicli", "omni")
            .map(|dirs| dirs.data_local_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from(".omni"));
        Self {
            data_dir,
            workspace: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            max_turns: 8,
            max_tool_output_bytes: 64 * 1024,
            max_file_bytes: 8 * 1024 * 1024,
            shell_timeout_seconds: 30,
            provider: ProviderKind::Fake,
            openai: OpenAiConfig::default(),
            anthropic: AnthropicConfig::default(),
            ollama: OllamaConfig::default(),
            lm_studio: LmStudioConfig::default(),
            llama_cpp: LlamaCppConfig::default(),
            openai_compatible: OpenAiCompatibleConfig::default(),
            custom_providers: BTreeMap::new(),
            mcp: McpConfig::default(),
            plugins: BTreeMap::new(),
            profiles: BTreeMap::new(),
        }
    }
}

impl AppConfig {
    pub fn load(explicit: Option<PathBuf>) -> Result<Self, ConfigError> {
        let mut config = Self::default();

        // 1. Global config: data_dir/omni.toml (persists across all projects)
        let global_path = config.data_dir.join("omni.toml");
        if global_path.exists() {
            let raw = fs::read_to_string(&global_path).map_err(|source| ConfigError::Read {
                path: global_path.clone(),
                source,
            })?;
            let global: AppConfig = toml::from_str(&raw).map_err(|source| ConfigError::Parse {
                path: global_path.clone(),
                source,
            })?;
            config.merge(global);
        }

        // 2. Local project config: workspace/omni.toml (overrides global)
        let local_path = config.workspace.join("omni.toml");
        if local_path.exists() {
            let raw = fs::read_to_string(&local_path).map_err(|source| ConfigError::Read {
                path: local_path.clone(),
                source,
            })?;
            let local: AppConfig = toml::from_str(&raw).map_err(|source| ConfigError::Parse {
                path: local_path.clone(),
                source,
            })?;
            config.merge(local);
        }

        // 3. Explicit --config path (highest priority)
        if let Some(path) = explicit {
            let raw = fs::read_to_string(&path).map_err(|source| ConfigError::Read {
                path: path.clone(),
                source,
            })?;
            let explicit_cfg: AppConfig =
                toml::from_str(&raw).map_err(|source| ConfigError::Parse {
                    path: path.clone(),
                    source,
                })?;
            config.merge(explicit_cfg);
        }

        config.validate()?;
        Ok(config)
    }

    pub fn merge(&mut self, other: AppConfig) {
        if other.max_turns != Self::default().max_turns {
            self.max_turns = other.max_turns;
        }
        if other.max_tool_output_bytes != Self::default().max_tool_output_bytes {
            self.max_tool_output_bytes = other.max_tool_output_bytes;
        }
        if other.max_file_bytes != Self::default().max_file_bytes {
            self.max_file_bytes = other.max_file_bytes;
        }
        if other.shell_timeout_seconds != Self::default().shell_timeout_seconds {
            self.shell_timeout_seconds = other.shell_timeout_seconds;
        }
        if other.provider != ProviderKind::Fake {
            self.provider = other.provider;
        }
        if !other.openai.model.is_empty() || !other.openai.base_url.is_empty() {
            self.openai = other.openai;
        }
        if !other.anthropic.model.is_empty() || !other.anthropic.base_url.is_empty() {
            self.anthropic = other.anthropic;
        }
        if !other.ollama.model.is_empty() || !other.ollama.base_url.is_empty() {
            self.ollama = other.ollama;
        }
        if !other.lm_studio.model.is_empty() || !other.lm_studio.base_url.is_empty() {
            self.lm_studio = other.lm_studio;
        }
        if !other.llama_cpp.model.is_empty() || !other.llama_cpp.base_url.is_empty() {
            self.llama_cpp = other.llama_cpp;
        }
        if !other.openai_compatible.model.is_empty() || !other.openai_compatible.base_url.is_empty()
        {
            self.openai_compatible = other.openai_compatible;
        }
        for (name, provider) in other.custom_providers {
            self.custom_providers.insert(name, provider);
        }
        if other.shell_timeout_seconds != Self::default().shell_timeout_seconds {
            self.shell_timeout_seconds = other.shell_timeout_seconds;
        }
        if !other.mcp.servers.is_empty() {
            self.mcp = other.mcp;
        }
        for (name, plugin) in other.plugins {
            self.plugins.insert(name, plugin);
        }
        for (name, profile) in other.profiles {
            self.profiles.insert(name, profile);
        }
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.max_turns == 0 {
            return Err(ConfigError::Invalid(
                "max_turns must be greater than zero".into(),
            ));
        }
        if self.max_tool_output_bytes == 0 {
            return Err(ConfigError::Invalid(
                "max_tool_output_bytes must be greater than zero".into(),
            ));
        }
        if self.max_file_bytes == 0 {
            return Err(ConfigError::Invalid(
                "max_file_bytes must be greater than zero".into(),
            ));
        }
        if self.provider == ProviderKind::OpenAi && self.openai.model.trim().is_empty() {
            return Err(ConfigError::Invalid(
                "openai.model must not be empty".into(),
            ));
        }
        if self.provider == ProviderKind::Anthropic && self.anthropic.model.trim().is_empty() {
            return Err(ConfigError::Invalid(
                "anthropic.model must not be empty".into(),
            ));
        }
        if self.anthropic.timeout_seconds == 0 {
            return Err(ConfigError::Invalid(
                "anthropic.timeout_seconds must be greater than zero".into(),
            ));
        }
        if self.anthropic.api_version.trim().is_empty() {
            return Err(ConfigError::Invalid(
                "anthropic.api_version must not be empty".into(),
            ));
        }
        if self.openai.timeout_seconds == 0 {
            return Err(ConfigError::Invalid(
                "openai.timeout_seconds must be greater than zero".into(),
            ));
        }
        if self.provider == ProviderKind::Ollama && self.ollama.model.trim().is_empty() {
            return Err(ConfigError::Invalid(
                "ollama.model must not be empty".into(),
            ));
        }
        if self.ollama.timeout_seconds == 0 {
            return Err(ConfigError::Invalid(
                "ollama.timeout_seconds must be greater than zero".into(),
            ));
        }
        if self.provider == ProviderKind::LmStudio && self.lm_studio.model.trim().is_empty() {
            return Err(ConfigError::Invalid(
                "lm_studio.model must not be empty".into(),
            ));
        }
        if self.lm_studio.timeout_seconds == 0 {
            return Err(ConfigError::Invalid(
                "lm_studio.timeout_seconds must be greater than zero".into(),
            ));
        }
        if self.provider == ProviderKind::LlamaCpp && self.llama_cpp.model.trim().is_empty() {
            return Err(ConfigError::Invalid(
                "llama_cpp.model must not be empty".into(),
            ));
        }
        if self.llama_cpp.timeout_seconds == 0 {
            return Err(ConfigError::Invalid(
                "llama_cpp.timeout_seconds must be greater than zero".into(),
            ));
        }
        if self.llama_cpp.temperature < 0.0 {
            return Err(ConfigError::Invalid(
                "llama_cpp.temperature must not be negative".into(),
            ));
        }
        if self.provider == ProviderKind::OpenAiCompatible
            && self.openai_compatible.base_url.trim().is_empty()
        {
            return Err(ConfigError::Invalid(
                "openai_compatible.base_url must not be empty".into(),
            ));
        }
        if self.provider == ProviderKind::OpenAiCompatible
            && self.openai_compatible.model.trim().is_empty()
        {
            return Err(ConfigError::Invalid(
                "openai_compatible.model must not be empty".into(),
            ));
        }
        if self.openai_compatible.timeout_seconds == 0 {
            return Err(ConfigError::Invalid(
                "openai_compatible.timeout_seconds must be greater than zero".into(),
            ));
        }
        if self.openai_compatible.api_key_env.trim().is_empty() {
            return Err(ConfigError::Invalid(
                "openai_compatible.api_key_env must not be empty".into(),
            ));
        }
        for (name, custom) in &self.custom_providers {
            if name.trim().is_empty() {
                return Err(ConfigError::Invalid(
                    "custom_providers keys must not be empty".into(),
                ));
            }
            if custom.base_url.trim().is_empty() {
                return Err(ConfigError::Invalid(format!(
                    "custom_providers.{name}.base_url must not be empty"
                )));
            }
            if custom.model.trim().is_empty() {
                return Err(ConfigError::Invalid(format!(
                    "custom_providers.{name}.model must not be empty"
                )));
            }
            if custom.timeout_seconds == 0 {
                return Err(ConfigError::Invalid(format!(
                    "custom_providers.{name}.timeout_seconds must be greater than zero"
                )));
            }
        }
        if self.mcp.max_message_bytes == 0 {
            return Err(ConfigError::Invalid(
                "mcp.max_message_bytes must be greater than zero".into(),
            ));
        }
        for (name, server) in &self.mcp.servers {
            if !valid_component(name) {
                return Err(ConfigError::Invalid(format!(
                    "invalid MCP server id: {name}"
                )));
            }
            if !server.command.is_absolute() {
                return Err(ConfigError::Invalid(format!(
                    "MCP server {name} command must be absolute"
                )));
            }
            if server.startup_timeout_seconds == 0 || server.call_timeout_seconds == 0 {
                return Err(ConfigError::Invalid(format!(
                    "MCP server {name} timeouts must be greater than zero"
                )));
            }
            let mut environment = std::collections::HashSet::new();
            for variable in &server.env {
                if !valid_environment_name(variable) {
                    return Err(ConfigError::Invalid(format!(
                        "MCP server {name} has invalid environment name: {variable}"
                    )));
                }
                let key = if cfg!(windows) {
                    variable.to_ascii_uppercase()
                } else {
                    variable.clone()
                };
                if !environment.insert(key) {
                    return Err(ConfigError::Invalid(format!(
                        "MCP server {name} repeats environment name: {variable}"
                    )));
                }
            }
        }
        for (name, plugin) in &self.plugins {
            if !crate::plugin::valid_plugin_name(name) {
                return Err(ConfigError::Invalid(format!("invalid plugin id: {name}")));
            }
            if plugin.startup_timeout_seconds == 0 || plugin.call_timeout_seconds == 0 {
                return Err(ConfigError::Invalid(format!(
                    "plugin {name} timeouts must be greater than zero"
                )));
            }
            if plugin.max_output_bytes == 0 {
                return Err(ConfigError::Invalid(format!(
                    "plugin {name} max_output_bytes must be greater than zero"
                )));
            }
            let mut environment = std::collections::HashSet::new();
            for variable in &plugin.env {
                if !valid_environment_name(variable) {
                    return Err(ConfigError::Invalid(format!(
                        "plugin {name} has invalid environment name: {variable}"
                    )));
                }
                let key = if cfg!(windows) {
                    variable.to_ascii_uppercase()
                } else {
                    variable.clone()
                };
                if !environment.insert(key) {
                    return Err(ConfigError::Invalid(format!(
                        "plugin {name} repeats environment name: {variable}"
                    )));
                }
            }
        }
        Ok(())
    }

    pub fn database_path(&self) -> PathBuf {
        self.data_dir.join("sessions.db")
    }

    pub fn apply_profile(&mut self, name: &str) -> Result<(), ConfigError> {
        let mut profile = builtin_profile(name).unwrap_or_default();
        if let Some(defined) = self.profiles.get(name) {
            merge_profile(&mut profile, defined);
        }
        if profile.provider.is_none()
            && profile.openai.is_none()
            && profile.anthropic.is_none()
            && profile.ollama.is_none()
            && profile.lm_studio.is_none()
            && profile.llama_cpp.is_none()
            && profile.openai_compatible.is_none()
            && profile.custom_providers.is_none()
            && profile.max_turns.is_none()
            && profile.max_tool_output_bytes.is_none()
            && profile.max_file_bytes.is_none()
            && profile.shell_timeout_seconds.is_none()
        {
            return Err(ConfigError::Invalid(format!("unknown profile: {name}")));
        }
        if let Some(provider) = profile.provider {
            self.provider = provider;
        }
        if let Some(openai) = profile.openai {
            self.openai = openai;
        }
        if let Some(anthropic) = profile.anthropic {
            self.anthropic = anthropic;
        }
        if let Some(ollama) = profile.ollama {
            self.ollama = ollama;
        }
        if let Some(lm_studio) = profile.lm_studio {
            self.lm_studio = lm_studio;
        }
        if let Some(llama_cpp) = profile.llama_cpp {
            self.llama_cpp = llama_cpp;
        }
        if let Some(openai_compatible) = profile.openai_compatible {
            self.openai_compatible = openai_compatible;
        }
        if let Some(custom_providers) = profile.custom_providers {
            self.custom_providers = custom_providers;
        }
        if let Some(max_turns) = profile.max_turns {
            self.max_turns = max_turns;
        }
        if let Some(max_tool_output_bytes) = profile.max_tool_output_bytes {
            self.max_tool_output_bytes = max_tool_output_bytes;
        }
        if let Some(max_file_bytes) = profile.max_file_bytes {
            self.max_file_bytes = max_file_bytes;
        }
        if let Some(shell_timeout_seconds) = profile.shell_timeout_seconds {
            self.shell_timeout_seconds = shell_timeout_seconds;
        }
        Ok(())
    }
}

fn builtin_profile(name: &str) -> Option<ProfileConfig> {
    match name {
        "offline" => Some(ProfileConfig {
            provider: Some(ProviderKind::Fake),
            openai: None,
            anthropic: None,
            ollama: None,
            lm_studio: None,
            llama_cpp: None,
            openai_compatible: None,
            custom_providers: None,
            max_turns: Some(4),
            max_tool_output_bytes: Some(16 * 1024),
            max_file_bytes: Some(1024 * 1024),
            shell_timeout_seconds: Some(10),
        }),
        "ci" => Some(ProfileConfig {
            provider: Some(ProviderKind::Fake),
            openai: None,
            anthropic: None,
            ollama: None,
            lm_studio: None,
            llama_cpp: None,
            openai_compatible: None,
            custom_providers: None,
            max_turns: Some(2),
            max_tool_output_bytes: Some(8 * 1024),
            max_file_bytes: Some(512 * 1024),
            shell_timeout_seconds: Some(60),
        }),
        "secure" => Some(ProfileConfig {
            provider: None,
            openai: None,
            anthropic: None,
            ollama: None,
            lm_studio: None,
            llama_cpp: None,
            openai_compatible: None,
            custom_providers: None,
            max_turns: Some(4),
            max_tool_output_bytes: Some(16 * 1024),
            max_file_bytes: Some(1024 * 1024),
            shell_timeout_seconds: Some(15),
        }),
        "fast" => Some(ProfileConfig {
            provider: None,
            openai: None,
            anthropic: None,
            ollama: None,
            lm_studio: None,
            llama_cpp: None,
            openai_compatible: None,
            custom_providers: None,
            max_turns: Some(3),
            max_tool_output_bytes: Some(16 * 1024),
            max_file_bytes: Some(1024 * 1024),
            shell_timeout_seconds: Some(15),
        }),
        _ => None,
    }
}

fn merge_profile(base: &mut ProfileConfig, override_: &ProfileConfig) {
    if override_.provider.is_some() {
        base.provider = override_.provider;
    }
    if override_.openai.is_some() {
        base.openai = override_.openai.clone();
    }
    if override_.anthropic.is_some() {
        base.anthropic = override_.anthropic.clone();
    }
    if override_.ollama.is_some() {
        base.ollama = override_.ollama.clone();
    }
    if override_.lm_studio.is_some() {
        base.lm_studio = override_.lm_studio.clone();
    }
    if override_.llama_cpp.is_some() {
        base.llama_cpp = override_.llama_cpp.clone();
    }
    if override_.openai_compatible.is_some() {
        base.openai_compatible = override_.openai_compatible.clone();
    }
    if override_.custom_providers.is_some() {
        base.custom_providers = override_.custom_providers.clone();
    }
    if override_.max_turns.is_some() {
        base.max_turns = override_.max_turns;
    }
    if override_.max_tool_output_bytes.is_some() {
        base.max_tool_output_bytes = override_.max_tool_output_bytes;
    }
    if override_.max_file_bytes.is_some() {
        base.max_file_bytes = override_.max_file_bytes;
    }
    if override_.shell_timeout_seconds.is_some() {
        base.shell_timeout_seconds = override_.shell_timeout_seconds;
    }
}

fn valid_component(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn valid_environment_name(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

#[cfg(test)]
mod tests {
    #[test]
    fn custom_providers_parse_and_validate() {
        use super::*;
        let config: AppConfig = toml::from_str(
            r#"
[custom_providers.openrouter]
base_url = "https://openrouter.ai/api/v1"
model = "anthropic/claude-sonnet-4"
api_key_env = "OPENROUTER_API_KEY"

[custom_providers.vllm]
base_url = "http://localhost:8000/v1"
model = "local"
"#,
        )
        .unwrap();
        assert_eq!(config.custom_providers.len(), 2);
        let vllm = &config.custom_providers["vllm"];
        assert_eq!(vllm.timeout_seconds, 120);
        assert!(vllm.api_key_env.is_empty());
        assert!(config.validate().is_ok());
    }

    #[test]
    fn custom_provider_missing_model_fails_validation() {
        use super::*;
        let config: AppConfig =
            toml::from_str("[custom_providers.broken]\nbase_url = \"http://localhost:8000/v1\"\n")
                .unwrap();
        assert!(config.validate().is_err());
    }

    use super::*;

    #[test]
    fn config_rejects_embedded_api_keys() {
        let error = toml::from_str::<AppConfig>(
            r#"
provider = "openai"

[openai]
model = "test"
api_key = "must-not-live-here"
"#,
        )
        .expect_err("unknown secret field must be rejected");
        assert!(error.to_string().contains("api_key"));
    }

    #[test]
    fn config_accepts_openai_provider_name() {
        let config = toml::from_str::<AppConfig>(
            r#"
provider = "openai"

[openai]
model = "test-model"
"#,
        )
        .unwrap();
        assert_eq!(config.provider, ProviderKind::OpenAi);
    }

    #[test]
    fn config_accepts_lm_studio_provider_name() {
        let config = toml::from_str::<AppConfig>(
            r#"
provider = "lm-studio"

[lm_studio]
model = "test-model"
"#,
        )
        .unwrap();
        assert_eq!(config.provider, ProviderKind::LmStudio);
        assert_eq!(config.lm_studio.base_url, "http://localhost:1234/v1/");
    }

    #[test]
    fn config_accepts_llama_cpp_provider_name() {
        let config = toml::from_str::<AppConfig>(
            r#"
provider = "llama-cpp"

[llama_cpp]
model = "test-model"
temperature = 0.5
n_predict = 512
"#,
        )
        .unwrap();
        assert_eq!(config.provider, ProviderKind::LlamaCpp);
        assert_eq!(config.llama_cpp.base_url, "http://localhost:8080");
        assert_eq!(config.llama_cpp.temperature, 0.5);
        assert_eq!(config.llama_cpp.n_predict, 512);
    }

    #[test]
    fn mcp_config_rejects_relative_commands_and_secret_maps() {
        let relative = toml::from_str::<AppConfig>(
            r#"
[mcp.servers.bad]
command = "server"
"#,
        )
        .unwrap();
        assert!(
            relative
                .validate()
                .unwrap_err()
                .to_string()
                .contains("absolute")
        );

        let secret = toml::from_str::<AppConfig>(
            r#"
[mcp.servers.bad]
command = "C:/server.exe"
environment = { TOKEN = "secret" }
"#,
        )
        .expect_err("secret value map must be rejected");
        assert!(secret.to_string().contains("environment"));
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read config {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse config {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("invalid configuration: {0}")]
    Invalid(String),
}
