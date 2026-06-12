use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(
    Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq, PartialOrd, Ord,
)]
pub enum AppType {
    #[serde(rename = "codex")]
    Codex,
    #[serde(rename = "opencode", alias = "open-code")]
    OpenCode,
}

impl AppType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "codex" => Some(Self::Codex),
            "opencode" | "open-code" => Some(Self::OpenCode),
            _ => None,
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
}

impl ProviderApps {
    pub fn enables(&self, app: &AppType) -> bool {
        match app {
            AppType::Codex => self.codex,
            AppType::OpenCode => self.open_code,
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
        values
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default, PartialEq, Eq)]
pub struct CodexModelConfig {
    #[serde(
        default,
        alias = "model",
        deserialize_with = "deserialize_models",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub models: Vec<String>,
    #[serde(rename = "reasoningEffort", skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default, PartialEq, Eq)]
pub struct OpenCodeModelConfig {
    #[serde(
        default,
        alias = "model",
        deserialize_with = "deserialize_models",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub models: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default, PartialEq, Eq)]
pub struct ProviderModels {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codex: Option<CodexModelConfig>,
    #[serde(rename = "opencode", skip_serializing_if = "Option::is_none")]
    pub open_code: Option<OpenCodeModelConfig>,
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
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
    #[serde(default)]
    pub models: ProviderModels,
    #[serde(default)]
    pub meta: ProviderMeta,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct KeybearerStore {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    #[serde(default)]
    pub profiles: BTreeMap<String, ProviderProfile>,
    #[serde(default)]
    pub defaults: BTreeMap<AppType, String>,
}

fn deserialize_models<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    match Option::<OneOrMany>::deserialize(deserializer)? {
        None => Ok(Vec::new()),
        Some(OneOrMany::One(s)) => Ok(vec![s]),
        Some(OneOrMany::Many(v)) => Ok(v),
    }
}
