use crate::model::{AppType, KeybearerStore, ProviderKind, ProviderProfile};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disable_response_storage: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_config: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub haiku_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sonnet_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opus_model: Option<String>,
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
    if !profile.apps.enables(&app) {
        return None;
    }
    match app {
        AppType::Codex => credential_for_codex(resolved_id, profile),
        AppType::OpenCode => credential_for_opencode(resolved_id, profile),
        AppType::ClaudeCode => credential_for_claude_code(resolved_id, profile),
    }
}

fn credential_for_codex(
    resolved_id: &str,
    profile: &ProviderProfile,
) -> Option<CredentialResponse> {
    if !AppType::Codex.supports_provider(&profile.provider_kind) {
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
        ProviderKind::Anthropic => unreachable!(),
    };
    let codex_config = profile.app_config.codex.as_ref();
    let model = codex_config
        .and_then(|c| c.model.clone())
        .unwrap_or_else(|| "gpt-5.5".to_string());
    Some(CredentialResponse {
        schema_version: CREDENTIAL_SCHEMA_VERSION,
        app: AppType::Codex,
        profile_id: resolved_id.to_string(),
        profile_name: profile.name.clone(),
        provider_kind: profile.provider_kind.clone(),
        base_url,
        api_key: profile.api_key.clone(),
        model: Some(model),
        reasoning_effort: Some(
            codex_config
                .and_then(|c| c.reasoning_effort.clone())
                .unwrap_or_else(|| "high".to_string()),
        ),
        disable_response_storage: Some(
            codex_config
                .and_then(|c| c.disable_response_storage)
                .unwrap_or(true),
        ),
        provider_config: None,
        haiku_model: None,
        sonnet_model: None,
        opus_model: None,
    })
}

fn credential_for_opencode(
    resolved_id: &str,
    profile: &ProviderProfile,
) -> Option<CredentialResponse> {
    let provider_config = profile
        .app_config
        .open_code
        .as_ref()
        .map(|c| c.0.clone())
        .filter(|v| !v.is_null());
    Some(CredentialResponse {
        schema_version: CREDENTIAL_SCHEMA_VERSION,
        app: AppType::OpenCode,
        profile_id: resolved_id.to_string(),
        profile_name: profile.name.clone(),
        provider_kind: profile.provider_kind.clone(),
        base_url: profile.base_url.clone(),
        api_key: profile.api_key.clone(),
        model: None,
        reasoning_effort: None,
        disable_response_storage: None,
        provider_config,
        haiku_model: None,
        sonnet_model: None,
        opus_model: None,
    })
}

fn credential_for_claude_code(
    resolved_id: &str,
    profile: &ProviderProfile,
) -> Option<CredentialResponse> {
    if !AppType::ClaudeCode.supports_provider(&profile.provider_kind) {
        return None;
    }
    let cc = profile.app_config.claude_code.as_ref();
    Some(CredentialResponse {
        schema_version: CREDENTIAL_SCHEMA_VERSION,
        app: AppType::ClaudeCode,
        profile_id: resolved_id.to_string(),
        profile_name: profile.name.clone(),
        provider_kind: profile.provider_kind.clone(),
        base_url: profile.base_url.clone(),
        api_key: profile.api_key.clone(),
        model: cc.and_then(|c| c.model.clone()),
        reasoning_effort: None,
        disable_response_storage: None,
        provider_config: None,
        haiku_model: cc.and_then(|c| c.haiku_model.clone()),
        sonnet_model: cc.and_then(|c| c.sonnet_model.clone()),
        opus_model: cc.and_then(|c| c.opus_model.clone()),
    })
}
