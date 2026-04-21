use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::scanner::{SaveAdapterProfile, SaveContainerFormat};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthState {
    pub token: String,
    pub email: String,
    pub base_url: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuthSessionStore {
    pub sessions: HashMap<String, AuthState>,
}

impl AuthState {
    pub fn new(token: String, email: String, base_url: String) -> Self {
        Self {
            token,
            email,
            base_url,
            created_at: now_rfc3339(),
        }
    }

    pub fn token_suffix(&self, n: usize) -> String {
        let chars: Vec<char> = self.token.chars().collect();
        if chars.len() <= n {
            return self.token.clone();
        }
        chars[chars.len() - n..].iter().collect()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyncState {
    pub entries: HashMap<String, SyncedEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncedEntry {
    pub sha256: String,
    pub rom_sha1: Option<String>,
    pub version: Option<i64>,
    #[serde(default)]
    pub system_slug: Option<String>,
    #[serde(default)]
    pub local_container: Option<SaveContainerFormat>,
    #[serde(default)]
    pub adapter_profile: Option<SaveAdapterProfile>,
    #[serde(default)]
    pub source_kind: Option<String>,
    #[serde(default)]
    pub source_name: Option<String>,
    pub updated_at: String,
}

pub fn auth_path(state_dir: &Path) -> PathBuf {
    state_dir.join("auth.json")
}

pub fn auth_sessions_path(state_dir: &Path) -> PathBuf {
    state_dir.join("auth_sessions.json")
}

pub fn sync_state_path(state_dir: &Path) -> PathBuf {
    state_dir.join("sync_state.json")
}

pub fn load_auth_state(state_dir: &Path) -> Result<Option<AuthState>> {
    load_json_or_reset::<AuthState>(&auth_path(state_dir))
}

pub fn load_auth_state_for_base_url(state_dir: &Path, base_url: &str) -> Result<Option<AuthState>> {
    let target = normalize_base_url(base_url);

    if let Some(store) = load_json_or_reset::<AuthSessionStore>(&auth_sessions_path(state_dir))?
        && let Some(found) = store.sessions.get(&target)
    {
        return Ok(Some(found.clone()));
    }

    if let Some(auth) = load_auth_state(state_dir)?
        && normalize_base_url(&auth.base_url) == target
    {
        return Ok(Some(auth));
    }

    Ok(None)
}

pub fn save_auth_state(state_dir: &Path, auth: &AuthState) -> Result<()> {
    save_auth_state_for_base_url(state_dir, auth)
}

pub fn save_auth_state_for_base_url(state_dir: &Path, auth: &AuthState) -> Result<()> {
    let mut store =
        load_json_or_reset::<AuthSessionStore>(&auth_sessions_path(state_dir))?.unwrap_or_default();
    store
        .sessions
        .insert(normalize_base_url(&auth.base_url), auth.clone());

    save_json(&auth_sessions_path(state_dir), &store)?;
    save_json(&auth_path(state_dir), auth)
}

pub fn clear_auth_state(state_dir: &Path) -> Result<()> {
    let auth = auth_path(state_dir);
    if auth.exists() {
        fs::remove_file(&auth)
            .with_context(|| format!("kan auth bestand niet verwijderen: {}", auth.display()))?;
    }

    let sessions = auth_sessions_path(state_dir);
    if sessions.exists() {
        fs::remove_file(&sessions).with_context(|| {
            format!(
                "kan auth sessions bestand niet verwijderen: {}",
                sessions.display()
            )
        })?;
    }

    Ok(())
}

pub fn clear_auth_state_for_base_url(state_dir: &Path, base_url: &str) -> Result<()> {
    let target = normalize_base_url(base_url);

    if let Some(mut store) = load_json_or_reset::<AuthSessionStore>(&auth_sessions_path(state_dir))?
    {
        store.sessions.remove(&target);
        if store.sessions.is_empty() {
            let path = auth_sessions_path(state_dir);
            if path.exists() {
                fs::remove_file(&path).with_context(|| {
                    format!("kan auth sessions niet verwijderen: {}", path.display())
                })?;
            }
        } else {
            save_json(&auth_sessions_path(state_dir), &store)?;
        }
    }

    if let Some(auth) = load_auth_state(state_dir)?
        && normalize_base_url(&auth.base_url) == target
    {
        let path = auth_path(state_dir);
        if path.exists() {
            fs::remove_file(&path).with_context(|| {
                format!("kan auth bestand niet verwijderen: {}", path.display())
            })?;
        }
    }

    Ok(())
}

pub fn load_sync_state(state_dir: &Path) -> Result<SyncState> {
    Ok(load_json_or_reset::<SyncState>(&sync_state_path(state_dir))?.unwrap_or_default())
}

pub fn save_sync_state(state_dir: &Path, state: &SyncState) -> Result<()> {
    save_json(&sync_state_path(state_dir), state)
}

fn load_json_or_reset<T>(path: &Path) -> Result<Option<T>>
where
    T: for<'de> Deserialize<'de>,
{
    if !path.exists() {
        return Ok(None);
    }

    let content = fs::read_to_string(path)
        .with_context(|| format!("kan statebestand niet lezen: {}", path.display()))?;

    match serde_json::from_str::<T>(&content) {
        Ok(value) => Ok(Some(value)),
        Err(err) => {
            backup_corrupt(path)?;
            eprintln!(
                "Waarschuwing: corrupt statebestand gereset ({}): {}",
                path.display(),
                err
            );
            Ok(None)
        }
    }
}

fn save_json<T>(path: &Path, value: &T) -> Result<()>
where
    T: Serialize,
{
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("kan map niet maken: {}", parent.display()))?;
    }

    let serialized = serde_json::to_string_pretty(value)?;
    fs::write(path, serialized)
        .with_context(|| format!("kan statebestand niet schrijven: {}", path.display()))?;
    Ok(())
}

fn backup_corrupt(path: &Path) -> Result<()> {
    let suffix = OffsetDateTime::now_utc().unix_timestamp();
    let backup = path.with_extension(format!("corrupt.{}", suffix));
    fs::rename(path, &backup).with_context(|| {
        format!(
            "kan corrupt statebestand niet back-uppen: {} -> {}",
            path.display(),
            backup.display()
        )
    })?;
    Ok(())
}

fn normalize_base_url(value: &str) -> String {
    value.trim().trim_end_matches('/').to_string()
}

pub fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrupt_state_is_reset_and_backed_up() {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().join("state");
        fs::create_dir_all(&state_dir).unwrap();
        fs::write(auth_path(&state_dir), "not-json").unwrap();

        let auth = load_auth_state(&state_dir).unwrap();
        assert!(auth.is_none());

        let backups = fs::read_dir(&state_dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name.starts_with("auth.corrupt."))
            .count();
        assert_eq!(backups, 1);
    }

    #[test]
    fn can_store_sessions_per_base_url() {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().join("state");
        fs::create_dir_all(&state_dir).unwrap();

        let auth1 = AuthState::new(
            "tok-1".to_string(),
            "a@a".to_string(),
            "http://one:3001".to_string(),
        );
        let auth2 = AuthState::new(
            "tok-2".to_string(),
            "b@b".to_string(),
            "http://two:3001".to_string(),
        );

        save_auth_state_for_base_url(&state_dir, &auth1).unwrap();
        save_auth_state_for_base_url(&state_dir, &auth2).unwrap();

        let loaded1 = load_auth_state_for_base_url(&state_dir, "http://one:3001").unwrap();
        let loaded2 = load_auth_state_for_base_url(&state_dir, "http://two:3001").unwrap();
        assert_eq!(loaded1.unwrap().token, "tok-1");
        assert_eq!(loaded2.unwrap().token, "tok-2");
    }

    #[test]
    fn legacy_sync_state_without_adapter_fields_still_loads() {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().join("state");
        fs::create_dir_all(&state_dir).unwrap();
        fs::write(
            sync_state_path(&state_dir),
            r#"{
  "entries": {
    "/tmp/example.sav": {
      "sha256": "abc",
      "rom_sha1": "def",
      "version": 1,
      "updated_at": "2026-01-01T00:00:00Z"
    }
  }
}"#,
        )
        .unwrap();

        let state = load_sync_state(&state_dir).unwrap();
        let entry = state.entries.get("/tmp/example.sav").unwrap();
        assert_eq!(entry.sha256, "abc");
        assert_eq!(entry.rom_sha1.as_deref(), Some("def"));
        assert!(entry.system_slug.is_none());
        assert!(entry.local_container.is_none());
        assert!(entry.adapter_profile.is_none());
    }
}
