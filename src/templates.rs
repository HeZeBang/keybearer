use crate::credentials::CredentialResponse;
use crate::model::{AppType, ProviderKind};
use serde_json::{Map, Value, json};
use toml_edit::{DocumentMut, Item, Table, value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppConfigMode {
    Replace,
    Merge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppConfig {
    pub virtual_path: &'static str,
    pub app: AppType,
    pub mode: AppConfigMode,
}

pub fn app_config_for_path(path: &str) -> Option<AppConfig> {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    if matches_path(path, home.as_ref(), ".codex/auth.json") {
        return Some(AppConfig {
            virtual_path: "codex/auth.json",
            app: AppType::Codex,
            mode: AppConfigMode::Replace,
        });
    }
    if matches_path(path, home.as_ref(), ".codex/config.toml") {
        return Some(AppConfig {
            virtual_path: "codex/config.toml",
            app: AppType::Codex,
            mode: AppConfigMode::Merge,
        });
    }
    if matches_path(path, home.as_ref(), ".config/opencode/opencode.json") {
        return Some(AppConfig {
            virtual_path: "opencode/opencode.json",
            app: AppType::OpenCode,
            mode: AppConfigMode::Merge,
        });
    }
    if matches_path(path, home.as_ref(), ".claude/settings.json") {
        return Some(AppConfig {
            virtual_path: "claude/settings.json",
            app: AppType::ClaudeCode,
            mode: AppConfigMode::Merge,
        });
    }
    None
}

pub fn render_app_config(
    config: &AppConfig,
    credential: &CredentialResponse,
    remote_base: Option<&[u8]>,
) -> Option<Vec<u8>> {
    if credential.app != config.app {
        return None;
    }
    match config.virtual_path {
        "codex/auth.json" => {
            serde_json::to_vec(&json!({ "OPENAI_API_KEY": credential.api_key })).ok()
        }
        "codex/config.toml" => merge_codex_config(credential, remote_base),
        "opencode/opencode.json" => merge_opencode_config(credential, remote_base),
        "claude/settings.json" => merge_claude_code_config(credential, remote_base),
        _ => None,
    }
}

fn matches_path(path: &str, home: Option<&std::path::PathBuf>, suffix: &str) -> bool {
    if let Some(home) = home {
        if path == home.join(suffix).to_string_lossy() {
            return true;
        }
    }
    let suffix = format!("/{suffix}");
    path.ends_with(&suffix)
}

fn merge_codex_config(
    credential: &CredentialResponse,
    remote_base: Option<&[u8]>,
) -> Option<Vec<u8>> {
    let mut doc = match remote_base.filter(|bytes| !bytes.is_empty()) {
        Some(bytes) => std::str::from_utf8(bytes)
            .ok()?
            .parse::<DocumentMut>()
            .ok()?,
        None => DocumentMut::new(),
    };
    let model = credential.model.as_deref().unwrap_or("gpt-5.5");
    let reasoning_effort = credential.reasoning_effort.as_deref().unwrap_or("high");
    let provider_id = format!("keybearer-{}", credential.profile_id);

    doc["model_provider"] = value(&provider_id);
    doc["model"] = value(model);
    doc["model_reasoning_effort"] = value(reasoning_effort);
    doc["disable_response_storage"] = value(credential.disable_response_storage.unwrap_or(true));

    if doc.get("model_providers").map_or(true, |v| !v.is_table()) {
        doc["model_providers"] = Item::Table(Table::new());
    }
    let provider_table = doc["model_providers"].as_table_mut()?;
    let mut table = Table::new();
    table["name"] = value(&credential.profile_name);
    if let Some(base_url) = &credential.base_url {
        table["base_url"] = value(base_url);
    }
    table["wire_api"] = value("responses");
    table["requires_openai_auth"] = value(true);
    provider_table[&provider_id] = Item::Table(table);

    Some(doc.to_string().into_bytes())
}

fn merge_opencode_config(
    credential: &CredentialResponse,
    remote_base: Option<&[u8]>,
) -> Option<Vec<u8>> {
    let mut root = match remote_base.filter(|bytes| !bytes.is_empty()) {
        Some(bytes) => json5::from_str::<Value>(std::str::from_utf8(bytes).ok()?).ok()?,
        None => json!({ "$schema": "https://opencode.ai/config.json" }),
    };
    let object = root.as_object_mut()?;
    object
        .entry("$schema".to_string())
        .or_insert_with(|| Value::String("https://opencode.ai/config.json".to_string()));
    if let Some(provider) = object.get("provider") {
        if !provider.is_object() {
            return None;
        }
    } else {
        object.insert("provider".to_string(), Value::Object(Map::new()));
    }

    let npm = match credential.provider_kind {
        ProviderKind::Anthropic => "@ai-sdk/anthropic",
        ProviderKind::OpenAI => "@ai-sdk/openai",
        ProviderKind::OpenAICompatible => "@ai-sdk/openai-compatible",
    };

    let mut options = Map::new();
    if let Some(base_url) = &credential.base_url {
        options.insert("baseURL".to_string(), Value::String(base_url.clone()));
    }
    options.insert("apiKey".to_string(), Value::String(credential.api_key.clone()));

    let mut models = Map::new();
    for m in &credential.models {
        models.insert(m.clone(), json!({ "name": m }));
    }

    let provider_id = format!("keybearer-{}", credential.profile_id);
    let provider = object.get_mut("provider")?.as_object_mut()?;
    provider.insert(
        provider_id,
        json!({
            "npm": npm,
            "name": credential.profile_name,
            "options": options,
            "models": models,
        }),
    );

    serde_json::to_vec(&root).ok()
}

fn merge_claude_code_config(
    credential: &CredentialResponse,
    remote_base: Option<&[u8]>,
) -> Option<Vec<u8>> {
    let mut root = match remote_base.filter(|bytes| !bytes.is_empty()) {
        Some(bytes) => serde_json::from_slice::<Value>(bytes).ok()?,
        None => json!({}),
    };
    let object = root.as_object_mut()?;

    let mut env = Map::new();
    env.insert(
        "ANTHROPIC_AUTH_TOKEN".to_string(),
        Value::String(credential.api_key.clone()),
    );
    if let Some(base_url) = &credential.base_url {
        env.insert(
            "ANTHROPIC_BASE_URL".to_string(),
            Value::String(base_url.clone()),
        );
    }
    if let Some(model) = &credential.model {
        env.insert(
            "ANTHROPIC_MODEL".to_string(),
            Value::String(model.clone()),
        );
    }
    if let Some(m) = &credential.haiku_model {
        env.insert("ANTHROPIC_DEFAULT_HAIKU_MODEL".to_string(), Value::String(m.clone()));
    }
    if let Some(m) = &credential.sonnet_model {
        env.insert("ANTHROPIC_DEFAULT_SONNET_MODEL".to_string(), Value::String(m.clone()));
    }
    if let Some(m) = &credential.opus_model {
        env.insert("ANTHROPIC_DEFAULT_OPUS_MODEL".to_string(), Value::String(m.clone()));
    }

    object.insert("env".to_string(), Value::Object(env));
    serde_json::to_vec(&root).ok()
}

#[cfg(test)]
mod tests {
    use super::{AppConfigMode, app_config_for_path};
    use crate::model::AppType;

    #[test]
    fn app_config_matches_known_paths() {
        let config = app_config_for_path("/home/me/.codex/config.toml").unwrap();
        assert_eq!(config.virtual_path, "codex/config.toml");
        assert_eq!(config.app, AppType::Codex);
        assert_eq!(config.mode, AppConfigMode::Merge);
    }
}
