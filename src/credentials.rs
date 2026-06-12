use crate::model::{AppType, KeybearerStore, ProviderKind, ProviderProfile};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CredentialResponse {
    pub schema_version: u32,
    pub app: AppType,
    pub profile_id: String,
    pub profile_name: String,
    pub provider_kind: ProviderKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    pub api_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}

pub const CREDENTIAL_SCHEMA_VERSION: u32 = 1;

pub fn credential_for_app(
    store: &KeybearerStore,
    app: AppType,
    profile_id: Option<&str>,
) -> Option<CredentialResponse> {
    let resolved_id = match profile_id {
        Some(id) => id,
        None => store.defaults.get(&app)?.as_str(),
    };
    let profile = store.profiles.get(resolved_id)?;
    if !profile.apps.enables(&app) || !renderer_supports_profile(profile) {
        return None;
    }
    let base_url = match profile.provider_kind {
        ProviderKind::OpenAI => Some(
            profile
                .base_url
                .clone()
                .unwrap_or_else(|| "https://api.openai.com/v1".to_string()),
        ),
        ProviderKind::OpenAICompatible => Some(profile.base_url.clone()?),
        ProviderKind::Anthropic => return None,
    };
    let model = match app {
        AppType::Codex => profile
            .models
            .codex
            .as_ref()
            .and_then(|config| config.models.first())
            .cloned()
            .unwrap_or_else(|| "gpt-4o".to_string()),
        AppType::OpenCode => profile
            .models
            .open_code
            .as_ref()
            .and_then(|config| config.models.first())
            .cloned()
            .unwrap_or_else(|| "gpt-4o".to_string()),
    };
    let reasoning_effort = match app {
        AppType::Codex => Some(
            profile
                .models
                .codex
                .as_ref()
                .and_then(|config| config.reasoning_effort.clone())
                .unwrap_or_else(|| "high".to_string()),
        ),
        AppType::OpenCode => None,
    };
    Some(CredentialResponse {
        schema_version: CREDENTIAL_SCHEMA_VERSION,
        app,
        profile_id: resolved_id.to_string(),
        profile_name: profile.name.clone(),
        provider_kind: profile.provider_kind.clone(),
        base_url,
        api_key: profile.api_key.clone(),
        model: Some(model),
        reasoning_effort,
    })
}

fn renderer_supports_profile(profile: &ProviderProfile) -> bool {
    matches!(
        profile.provider_kind,
        ProviderKind::OpenAI | ProviderKind::OpenAICompatible
    )
}
