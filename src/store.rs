use crate::model::{AppType, KeybearerStore, ProviderKind, ProviderProfile};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;

pub const STORE_SCHEMA_VERSION: u32 = 1;

pub fn config_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("KEYBEARER_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(dir).join("keybearer");
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".config/keybearer");
    }
    let uid = unsafe { libc::geteuid() };
    std::env::temp_dir().join(format!("keybearer-config-{uid}"))
}

pub fn store_path() -> PathBuf {
    config_dir().join("config.yaml")
}

pub fn load_store() -> io::Result<KeybearerStore> {
    let path = store_path();
    if !path.exists() {
        return Ok(empty_store());
    }

    let bytes = fs::read(&path)?;
    let store: KeybearerStore = serde_yaml::from_slice(&bytes).map_err(invalid_data)?;
    validate_store(&store)?;
    Ok(store)
}

pub fn save_store(store: &KeybearerStore) -> io::Result<()> {
    validate_store(store)?;
    let dir = config_dir();
    fs::create_dir_all(&dir)?;
    set_permissions(&dir, 0o700)?;

    let path = store_path();
    let tmp = config_dir().join("config.yaml.tmp");
    let yaml = serde_yaml::to_string(store).map_err(invalid_data)?;

    {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&tmp)?;
        file.write_all(yaml.as_bytes())?;
        file.sync_all()?;
    }
    set_permissions(&tmp, 0o600)?;
    fs::rename(tmp, path)
}

pub fn validate_store(store: &KeybearerStore) -> io::Result<()> {
    if store.schema_version != STORE_SCHEMA_VERSION {
        return invalid_config(format!(
            "unsupported schemaVersion {}",
            store.schema_version
        ));
    }
    if store.profiles.is_empty() {
        return invalid_config("profiles must not be empty");
    }
    for (profile_id, profile) in &store.profiles {
        if !valid_profile_id(profile_id) {
            return invalid_config(format!("invalid profile id {profile_id}"));
        }
        if profile.name.is_empty() {
            return invalid_config(format!("profile {profile_id} name must not be empty"));
        }
        if profile.api_key.is_empty() {
            return invalid_config(format!("profile {profile_id} apiKey must not be empty"));
        }
        if !profile.apps.codex && !profile.apps.open_code {
            return invalid_config(format!("profile {profile_id} must enable at least one app"));
        }
        if matches!(profile.provider_kind, ProviderKind::OpenAICompatible)
            && (profile.apps.codex || profile.apps.open_code)
            && profile.base_url.as_deref().unwrap_or_default().is_empty()
        {
            return invalid_config(format!(
                "profile {profile_id} openai-compatible requires baseUrl"
            ));
        }
    }
    for (app, profile_id) in &store.defaults {
        let Some(profile) = store.profiles.get(profile_id) else {
            return invalid_config(format!(
                "default {} references missing profile {profile_id}",
                app.as_str()
            ));
        };
        if !profile.apps.enables(app) {
            return invalid_config(format!(
                "default {} profile {profile_id} is not enabled for app",
                app.as_str()
            ));
        }
    }
    Ok(())
}

pub fn upsert_provider(store: &mut KeybearerStore, profile_id: String, profile: ProviderProfile) {
    store.profiles.insert(profile_id, profile);
}

pub fn remove_provider(store: &mut KeybearerStore, profile_id: &str) -> bool {
    let removed = store.profiles.remove(profile_id).is_some();
    if removed {
        store.defaults.retain(|_, id| id != profile_id);
    }
    removed
}

pub fn set_default_profile(store: &mut KeybearerStore, app: AppType, profile_id: String) {
    store.defaults.insert(app, profile_id);
}

pub fn empty_store() -> KeybearerStore {
    KeybearerStore {
        schema_version: STORE_SCHEMA_VERSION,
        profiles: BTreeMap::new(),
        defaults: BTreeMap::new(),
    }
}

fn valid_profile_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn invalid_config<T>(message: impl Into<String>) -> io::Result<T> {
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!("invalid keybearer config: {}", message.into()),
    ))
}

#[cfg(unix)]
fn set_permissions(path: &std::path::Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(mode);
    fs::set_permissions(path, permissions)
}

#[cfg(not(unix))]
fn set_permissions(_path: &std::path::Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

fn invalid_data(error: impl std::error::Error + Send + Sync + 'static) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}
