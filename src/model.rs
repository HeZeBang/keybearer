use std::collections::BTreeMap;

#[derive(
    Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq, PartialOrd, Ord,
)]
pub enum AppType {
    #[serde(rename = "codex")]
    Codex,
    #[serde(rename = "opencode", alias = "open-code")]
    OpenCode,
    #[serde(rename = "claudeCode", alias = "claude-code")]
    ClaudeCode,
}

impl AppType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
            Self::ClaudeCode => "claudeCode",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "codex" => Some(Self::Codex),
            "opencode" | "open-code" => Some(Self::OpenCode),
            "claudeCode" | "claude-code" => Some(Self::ClaudeCode),
            _ => None,
        }
    }

    pub fn supports_provider(&self, provider: &ProviderKind) -> bool {
        match self {
            Self::Codex => matches!(provider, ProviderKind::OpenAI | ProviderKind::OpenAICompatible),
            Self::ClaudeCode => matches!(provider, ProviderKind::Anthropic),
            Self::OpenCode => true,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderKind {
    #[serde(rename = "openai")]
    OpenAI,
    #[serde(rename = "anthropic")]
    Anthropic,
    #[serde(rename = "openai-compatible")]
    OpenAICompatible,
}

impl ProviderKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::OpenAI => "openai",
            Self::Anthropic => "anthropic",
            Self::OpenAICompatible => "openai-compatible",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "openai" => Some(Self::OpenAI),
            "anthropic" => Some(Self::Anthropic),
            "openai-compatible" => Some(Self::OpenAICompatible),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default, PartialEq, Eq)]
#[serde(from = "Vec<AppType>", into = "Vec<AppType>")]
pub struct ProviderApps {
    #[serde(default)]
    pub codex: bool,
    #[serde(default, rename = "opencode")]
    pub open_code: bool,
    #[serde(default, rename = "claudeCode")]
    pub claude_code: bool,
}

impl ProviderApps {
    pub fn enables(&self, app: &AppType) -> bool {
        match app {
            AppType::Codex => self.codex,
            AppType::OpenCode => self.open_code,
            AppType::ClaudeCode => self.claude_code,
        }
    }

    pub fn csv(&self) -> String {
        let mut apps = Vec::new();
        if self.codex {
            apps.push("codex");
        }
        if self.open_code {
            apps.push("opencode");
        }
        if self.claude_code {
            apps.push("claudeCode");
        }
        apps.join(",")
    }
}

impl From<Vec<AppType>> for ProviderApps {
    fn from(apps: Vec<AppType>) -> Self {
        let mut provider_apps = ProviderApps::default();
        for app in apps {
            match app {
                AppType::Codex => provider_apps.codex = true,
                AppType::OpenCode => provider_apps.open_code = true,
                AppType::ClaudeCode => provider_apps.claude_code = true,
            }
        }
        provider_apps
    }
}

impl From<ProviderApps> for Vec<AppType> {
    fn from(apps: ProviderApps) -> Self {
        let mut values = Vec::new();
        if apps.codex {
            values.push(AppType::Codex);
        }
        if apps.open_code {
            values.push(AppType::OpenCode);
        }
        if apps.claude_code {
            values.push(AppType::ClaudeCode);
        }
        values
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default, PartialEq, Eq)]
pub struct CodexModelConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(rename = "reasoningEffort", skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(
        rename = "disableResponseStorage",
        skip_serializing_if = "Option::is_none"
    )]
    pub disable_response_storage: Option<bool>,
}

/// Transparent passthrough for the OpenCode ProviderConfig schema.
/// See https://opencode.ai/config.json — fields like `models`, `options`,
/// `npm`, `api`, `whitelist`, `blacklist` etc. are passed through as-is.
/// At render time keybearer injects `npm`, `name`, `options.apiKey`,
/// `options.baseURL` from the profile-level fields.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default, PartialEq)]
#[serde(transparent)]
pub struct OpenCodeModelConfig(pub serde_json::Value);

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default, PartialEq, Eq)]
pub struct ClaudeCodeModelConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(rename = "haikuModel", skip_serializing_if = "Option::is_none")]
    pub haiku_model: Option<String>,
    #[serde(rename = "sonnetModel", skip_serializing_if = "Option::is_none")]
    pub sonnet_model: Option<String>,
    #[serde(rename = "opusModel", skip_serializing_if = "Option::is_none")]
    pub opus_model: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default, PartialEq)]
pub struct ProviderAppConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codex: Option<CodexModelConfig>,
    #[serde(rename = "opencode", skip_serializing_if = "Option::is_none")]
    pub open_code: Option<OpenCodeModelConfig>,
    #[serde(rename = "claudeCode", skip_serializing_if = "Option::is_none")]
    pub claude_code: Option<ClaudeCodeModelConfig>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default, PartialEq, Eq)]
pub struct ProviderMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(rename = "createdAt", skip_serializing_if = "Option::is_none")]
    pub created_at: Option<i64>,
    #[serde(rename = "sortIndex", skip_serializing_if = "Option::is_none")]
    pub sort_index: Option<usize>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct ProviderProfile {
    pub name: String,
    #[serde(rename = "providerKind")]
    pub provider_kind: ProviderKind,
    #[serde(default)]
    pub apps: ProviderApps,
    #[serde(rename = "baseUrl", skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(rename = "apiKey")]
    pub api_key: String,
    #[serde(default, rename = "appConfig")]
    pub app_config: ProviderAppConfig,
    #[serde(default)]
    pub meta: ProviderMeta,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct KeybearerStore {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    #[serde(default)]
    pub profiles: BTreeMap<String, ProviderProfile>,
    #[serde(default)]
    pub defaults: BTreeMap<AppType, String>,
}

