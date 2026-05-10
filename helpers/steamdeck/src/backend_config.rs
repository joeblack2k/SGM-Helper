use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Deserializer};
use serde_json::Value;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::api::ApiClient;
use crate::config::AppConfig;
use crate::sources::{
    EmulatorProfile, ResolvedSource, Source, SourceKind, SourceStore, default_profile_for_kind,
    default_systems_for_kind, load_source_store, save_source_store,
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeConfigOverrides {
    pub force_upload: Option<bool>,
    pub dry_run: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct BackendConfigSyncResponse {
    pub accepted: bool,
    pub policy: Option<BackendPolicy>,
    #[serde(rename = "desiredConfig", alias = "desired_config")]
    pub desired_config: Option<BackendPolicy>,
    #[serde(rename = "effectiveConfig", alias = "effective_config")]
    pub effective_config: Option<BackendPolicy>,
    pub sources: Vec<BackendSourcePolicy>,
    pub global: Option<BackendGlobalPolicy>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct BackendPolicy {
    pub sources: Vec<BackendSourcePolicy>,
    pub global: Option<BackendGlobalPolicy>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct BackendGlobalPolicy {
    pub url: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_u16")]
    pub port: Option<u16>,
    pub email: Option<String>,
    #[serde(rename = "root", alias = "ROOT")]
    pub root: Option<PathBuf>,
    #[serde(rename = "stateDir", alias = "state_dir", alias = "STATE_DIR")]
    pub state_dir: Option<PathBuf>,
    #[serde(default, deserialize_with = "deserialize_optional_bool")]
    pub watch: Option<bool>,
    #[serde(
        rename = "watchInterval",
        alias = "watch_interval",
        alias = "WATCH_INTERVAL",
        default,
        deserialize_with = "deserialize_optional_u64"
    )]
    pub watch_interval: Option<u64>,
    #[serde(
        rename = "forceUpload",
        alias = "force_upload",
        alias = "FORCE_UPLOAD",
        default,
        deserialize_with = "deserialize_optional_bool"
    )]
    pub force_upload: Option<bool>,
    #[serde(
        rename = "dryRun",
        alias = "dry_run",
        alias = "DRY_RUN",
        default,
        deserialize_with = "deserialize_optional_bool"
    )]
    pub dry_run: Option<bool>,
    #[serde(rename = "routePrefix", alias = "route_prefix", alias = "ROUTE_PREFIX")]
    pub route_prefix: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct BackendSourcePolicy {
    pub id: Option<String>,
    #[serde(rename = "sourceId", alias = "source_id")]
    pub source_id: Option<String>,
    pub name: Option<String>,
    pub label: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_bool")]
    pub enabled: Option<bool>,
    pub kind: Option<String>,
    pub profile: Option<String>,
    #[serde(
        alias = "savePaths",
        alias = "save_roots",
        alias = "SAVE_PATH",
        default,
        deserialize_with = "deserialize_path_vec"
    )]
    pub save_roots: Vec<PathBuf>,
    #[serde(
        alias = "savePath",
        default,
        deserialize_with = "deserialize_optional_path"
    )]
    pub save_path: Option<PathBuf>,
    #[serde(
        alias = "romPaths",
        alias = "rom_roots",
        alias = "ROM_PATH",
        default,
        deserialize_with = "deserialize_path_vec"
    )]
    pub rom_roots: Vec<PathBuf>,
    #[serde(
        alias = "romPath",
        default,
        deserialize_with = "deserialize_optional_path"
    )]
    pub rom_path: Option<PathBuf>,
    #[serde(default, deserialize_with = "deserialize_optional_bool")]
    pub recursive: Option<bool>,
    #[serde(default, deserialize_with = "deserialize_optional_string_list")]
    pub systems: Option<Vec<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_bool")]
    pub managed: Option<bool>,
    pub origin: Option<String>,
    #[serde(
        rename = "createMissingSystemDirs",
        alias = "create_missing_system_dirs",
        alias = "CREATE_MISSING_SYSTEM_DIRS",
        default,
        deserialize_with = "deserialize_optional_bool"
    )]
    pub create_missing_system_dirs: Option<bool>,
}

impl BackendSourcePolicy {
    fn id_value(&self) -> Option<&str> {
        self.id
            .as_deref()
            .or(self.source_id.as_deref())
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }

    fn name_value(&self) -> Option<&str> {
        self.name
            .as_deref()
            .or(self.label.as_deref())
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }
}

pub fn sync_config_with_backend(
    api: &ApiClient,
    config: &AppConfig,
    sources: &mut [ResolvedSource],
    default_source_kind: &SourceKind,
    app_password: Option<&str>,
    verbose: bool,
) -> Result<RuntimeConfigOverrides> {
    let payload = build_config_snapshot(config, sources, default_source_kind)?;
    let response = api.sync_helper_config(&payload, app_password)?;
    let response: BackendConfigSyncResponse =
        serde_json::from_value(response).context("kan backend config sync response niet lezen")?;

    let overrides = apply_backend_response(sources, &response, verbose);
    write_backend_policy_to_config(config, &response, verbose)?;
    if verbose && response.accepted {
        eprintln!("Backend accepted helper config snapshot.");
    }
    Ok(overrides)
}

fn build_config_snapshot(
    config: &AppConfig,
    sources: &[ResolvedSource],
    default_source_kind: &SourceKind,
) -> Result<serde_json::Value> {
    let hostname = hostname::get()
        .ok()
        .and_then(|value| value.into_string().ok())
        .unwrap_or_else(|| default_source_kind.as_str().to_string());

    Ok(serde_json::json!({
        "schemaVersion": 1,
        "helper": {
            "name": env!("CARGO_PKG_NAME"),
            "version": env!("CARGO_PKG_VERSION"),
            "deviceType": default_source_kind.helper_device_type(),
            "defaultKind": default_source_kind.as_str(),
            "hostname": hostname,
            "platform": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "configPath": config.config_path,
            "binaryDir": config.binary_dir,
        },
        "config": {
            "url": config.url,
            "port": config.port,
            "baseUrl": config.base_url(),
            "email": config.email,
            "appPasswordConfigured": !config.app_password.trim().is_empty(),
            "root": config.root,
            "stateDir": config.state_dir,
            "watch": config.watch,
            "watchInterval": config.watch_interval,
            "forceUpload": config.force_upload,
            "dryRun": config.dry_run,
            "routePrefix": config.route_prefix,
            "sources": sources.iter().map(source_snapshot).collect::<Vec<_>>(),
        },
        "capabilities": capability_matrix(),
    }))
}

fn source_snapshot(source: &ResolvedSource) -> serde_json::Value {
    serde_json::json!({
        "id": source.id,
        "label": source.name,
        "kind": source.kind.as_str(),
        "profile": source.profile.as_str(),
        "savePaths": source.save_roots,
        "romPaths": source.rom_roots,
        "recursive": source.recursive,
        "systems": source.systems,
        "createMissingSystemDirs": source.create_missing_system_dirs,
        "managed": source.managed,
        "origin": source.origin,
    })
}

pub(crate) fn capability_matrix() -> serde_json::Value {
    let kinds = [
        SourceKind::MisterFpga,
        SourceKind::RetroArch,
        SourceKind::Ports,
        SourceKind::SteamDeck,
        SourceKind::Windows,
        SourceKind::OpenEmu,
        SourceKind::AnaloguePocket,
        SourceKind::Custom,
    ];
    serde_json::json!({
        "sourceKinds": kinds
            .iter()
            .map(|kind| serde_json::json!({
                "kind": kind.as_str(),
                "deviceType": kind.helper_device_type(),
                "defaultProfile": super::sources::default_profile_for_kind(kind).as_str(),
                "defaultSystems": default_systems_for_kind(kind),
            }))
            .collect::<Vec<_>>(),
        "profiles": [
            "mister",
            "retroarch",
            "snes9x",
            "zsnes",
            "everdrive",
            "project64",
            "mupen-family",
            "generic"
        ],
        "policy": {
            "supportsSystemsAllowList": true,
            "supportsCreateMissingSystemDirs": true,
            "supportsConfigWriteback": true,
            "manualManagedPolicy": "MANAGED indicates autoscan ownership only; backend policy can still write config.ini."
        },
        "service": {
            "supportsDaemonMode": true,
            "heartbeatEndpoint": "POST /helpers/heartbeat",
            "controlChannel": "GET /events",
            "controlEvents": [
                "sync.requested",
                "scan.requested",
                "deep_scan.requested",
                "config.changed",
                "save.changed",
                "save_created",
                "save_parsed",
                "save_deleted",
                "conflict_created",
                "conflict_resolved"
            ]
        }
    })
}

fn deserialize_optional_u16<'de, D>(deserializer: D) -> std::result::Result<Option<u16>, D::Error>
where
    D: Deserializer<'de>,
{
    let Some(value) = Option::<Value>::deserialize(deserializer)? else {
        return Ok(None);
    };
    match value {
        Value::Null => Ok(None),
        Value::Number(number) => number
            .as_u64()
            .and_then(|value| u16::try_from(value).ok())
            .map(Some)
            .ok_or_else(|| serde::de::Error::custom("expected u16-compatible number")),
        Value::String(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                trimmed
                    .parse::<u16>()
                    .map(Some)
                    .map_err(serde::de::Error::custom)
            }
        }
        _ => Err(serde::de::Error::custom("expected number, string, or null")),
    }
}

fn deserialize_optional_u64<'de, D>(deserializer: D) -> std::result::Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    let Some(value) = Option::<Value>::deserialize(deserializer)? else {
        return Ok(None);
    };
    match value {
        Value::Null => Ok(None),
        Value::Number(number) => number
            .as_u64()
            .map(Some)
            .ok_or_else(|| serde::de::Error::custom("expected u64-compatible number")),
        Value::String(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                trimmed
                    .parse::<u64>()
                    .map(Some)
                    .map_err(serde::de::Error::custom)
            }
        }
        _ => Err(serde::de::Error::custom("expected number, string, or null")),
    }
}

fn deserialize_optional_bool<'de, D>(deserializer: D) -> std::result::Result<Option<bool>, D::Error>
where
    D: Deserializer<'de>,
{
    let Some(value) = Option::<Value>::deserialize(deserializer)? else {
        return Ok(None);
    };
    match value {
        Value::Null => Ok(None),
        Value::Bool(value) => Ok(Some(value)),
        Value::String(text) => {
            let normalized = text.trim().to_ascii_lowercase();
            match normalized.as_str() {
                "" => Ok(None),
                "true" | "1" | "yes" | "on" => Ok(Some(true)),
                "false" | "0" | "no" | "off" => Ok(Some(false)),
                _ => Err(serde::de::Error::custom("expected bool-compatible string")),
            }
        }
        _ => Err(serde::de::Error::custom("expected bool, string, or null")),
    }
}

fn deserialize_optional_path<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<PathBuf>, D::Error>
where
    D: Deserializer<'de>,
{
    let Some(value) = Option::<Value>::deserialize(deserializer)? else {
        return Ok(None);
    };
    match value {
        Value::Null => Ok(None),
        Value::String(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(PathBuf::from(trimmed)))
            }
        }
        _ => Err(serde::de::Error::custom("expected path string or null")),
    }
}

fn deserialize_path_vec<'de, D>(deserializer: D) -> std::result::Result<Vec<PathBuf>, D::Error>
where
    D: Deserializer<'de>,
{
    let Some(value) = Option::<Value>::deserialize(deserializer)? else {
        return Ok(Vec::new());
    };
    match value {
        Value::Null => Ok(Vec::new()),
        Value::String(text) => Ok(split_policy_list(&text)
            .into_iter()
            .map(PathBuf::from)
            .collect()),
        Value::Array(items) => items
            .into_iter()
            .map(|item| match item {
                Value::String(text) => Ok(text.trim().to_string()),
                _ => Err(serde::de::Error::custom("expected string path item")),
            })
            .filter_map(|item| match item {
                Ok(text) if text.is_empty() => None,
                other => Some(other.map(PathBuf::from)),
            })
            .collect(),
        _ => Err(serde::de::Error::custom(
            "expected path string, path array, or null",
        )),
    }
}

fn deserialize_optional_string_list<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<Vec<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    let Some(value) = Option::<Value>::deserialize(deserializer)? else {
        return Ok(None);
    };
    match value {
        Value::Null => Ok(None),
        Value::String(text) => {
            let values = split_policy_list(&text);
            if values.is_empty() {
                Ok(None)
            } else {
                Ok(Some(values))
            }
        }
        Value::Array(items) => {
            let mut out = Vec::new();
            for item in items {
                match item {
                    Value::String(text) => {
                        let trimmed = text.trim();
                        if !trimmed.is_empty() {
                            out.push(trimmed.to_string());
                        }
                    }
                    _ => return Err(serde::de::Error::custom("expected string list item")),
                }
            }
            if out.is_empty() {
                Ok(None)
            } else {
                Ok(Some(out))
            }
        }
        _ => Err(serde::de::Error::custom(
            "expected string, string array, or null",
        )),
    }
}

fn split_policy_list(value: &str) -> Vec<String> {
    value
        .split([',', ';'])
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn write_backend_policy_to_config(
    config: &AppConfig,
    response: &BackendConfigSyncResponse,
    verbose: bool,
) -> Result<()> {
    if !response_has_writeback(response) {
        return Ok(());
    }

    let backup = backup_config_if_exists(&config.config_path)?;
    let existing = read_file_if_exists(&config.config_path)?;
    let global_updates = collect_global_writeback(response);
    let with_globals = apply_global_writeback_to_ini(&existing, &global_updates);
    write_file(&config.config_path, &with_globals)?;

    let mut store = load_source_store(&config.config_path)?;
    apply_source_writeback(&mut store, response);
    save_source_store(&config.config_path, &store)?;

    if verbose {
        if let Some(backup) = backup {
            eprintln!(
                "Backend config policy written to {}; backup: {}",
                config.config_path.display(),
                backup.display()
            );
        } else {
            eprintln!(
                "Backend config policy written to {}",
                config.config_path.display()
            );
        }
    }

    Ok(())
}

fn response_has_writeback(response: &BackendConfigSyncResponse) -> bool {
    !source_policies(response).is_empty()
        || collect_global_writeback(response)
            .values()
            .any(|value| value.is_some())
}

fn collect_global_writeback(
    response: &BackendConfigSyncResponse,
) -> BTreeMap<&'static str, Option<String>> {
    let mut out = BTreeMap::new();
    for policy in global_policies(response) {
        merge_global_policy(&mut out, policy);
    }
    out
}

fn global_policies(response: &BackendConfigSyncResponse) -> Vec<&BackendGlobalPolicy> {
    let mut out = Vec::new();
    if let Some(global) = response.global.as_ref() {
        out.push(global);
    }
    if let Some(policy) = response
        .policy
        .as_ref()
        .and_then(|value| value.global.as_ref())
    {
        out.push(policy);
    }
    if let Some(policy) = response
        .desired_config
        .as_ref()
        .and_then(|value| value.global.as_ref())
    {
        out.push(policy);
    }
    if let Some(policy) = response
        .effective_config
        .as_ref()
        .and_then(|value| value.global.as_ref())
    {
        out.push(policy);
    }
    out
}

fn merge_global_policy(
    out: &mut BTreeMap<&'static str, Option<String>>,
    policy: &BackendGlobalPolicy,
) {
    if let Some(value) = policy.url.as_ref() {
        out.insert("URL", Some(value.clone()));
    }
    if let Some(value) = policy.port {
        out.insert("PORT", Some(value.to_string()));
    }
    if let Some(value) = policy.email.as_ref() {
        out.insert("EMAIL", Some(value.clone()));
    }
    if let Some(value) = policy.root.as_ref() {
        out.insert("ROOT", Some(value.to_string_lossy().to_string()));
    }
    if let Some(value) = policy.state_dir.as_ref() {
        out.insert("STATE_DIR", Some(value.to_string_lossy().to_string()));
    }
    if let Some(value) = policy.watch {
        out.insert("WATCH", Some(value.to_string()));
    }
    if let Some(value) = policy.watch_interval {
        out.insert("WATCH_INTERVAL", Some(value.to_string()));
    }
    if let Some(value) = policy.force_upload {
        out.insert("FORCE_UPLOAD", Some(value.to_string()));
    }
    if let Some(value) = policy.dry_run {
        out.insert("DRY_RUN", Some(value.to_string()));
    }
    if let Some(value) = policy.route_prefix.as_ref() {
        out.insert("ROUTE_PREFIX", Some(value.clone()));
    }
}

fn apply_global_writeback_to_ini(
    existing: &str,
    updates: &BTreeMap<&'static str, Option<String>>,
) -> String {
    if updates.is_empty() {
        return existing.to_string();
    }

    let mut consumed: BTreeSet<String> = BTreeSet::new();
    let mut in_section = false;
    let mut lines = Vec::new();

    for line in existing.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            if !in_section {
                append_missing_globals(&mut lines, updates, &consumed);
                consumed.extend(updates.keys().map(|key| (*key).to_string()));
            }
            in_section = true;
            lines.push(line.to_string());
            continue;
        }

        if !in_section && let Some(eq_pos) = trimmed.find('=') {
            let key = trimmed[..eq_pos].trim().to_ascii_uppercase();
            if let Some(value) = updates.get(key.as_str()).and_then(|value| value.as_ref()) {
                lines.push(format!("{}=\"{}\"", key, escape_ini(value)));
                consumed.insert(key);
                continue;
            }
        }

        lines.push(line.to_string());
    }

    if !in_section {
        append_missing_globals(&mut lines, updates, &consumed);
    }

    while lines
        .last()
        .map(|line| line.trim().is_empty())
        .unwrap_or(false)
    {
        lines.pop();
    }

    format!("{}\n", lines.join("\n"))
}

fn append_missing_globals(
    lines: &mut Vec<String>,
    updates: &BTreeMap<&'static str, Option<String>>,
    consumed: &BTreeSet<String>,
) {
    for (key, value) in updates {
        if consumed.contains(*key) {
            continue;
        }
        if let Some(value) = value {
            lines.push(format!("{}=\"{}\"", key, escape_ini(value)));
        }
    }
}

fn apply_source_writeback(store: &mut SourceStore, response: &BackendConfigSyncResponse) {
    for policy in source_policies(response) {
        if let Some(source) = store
            .sources
            .iter_mut()
            .find(|source| source_policy_matches_source(source, policy))
        {
            apply_policy_to_stored_source(source, policy);
        } else if let Some(source) = source_from_policy(policy) {
            store.sources.push(source);
        }
    }
}

fn source_policy_matches_source(source: &Source, policy: &BackendSourcePolicy) -> bool {
    if let Some(id) = policy.id_value()
        && source.id == id
    {
        return true;
    }
    if let Some(name) = policy.name_value()
        && source.name == name
    {
        return true;
    }
    false
}

fn source_from_policy(policy: &BackendSourcePolicy) -> Option<Source> {
    let name = policy
        .name_value()
        .map(ToString::to_string)
        .or_else(|| policy.id_value().map(ToString::to_string))
        .unwrap_or_else(|| "Backend Source".to_string());
    let kind = policy
        .kind
        .as_deref()
        .and_then(SourceKind::parse)
        .unwrap_or(SourceKind::Custom);
    let save_roots = source_policy_save_roots(policy);
    if save_roots.is_empty() {
        return None;
    }
    let rom_roots = source_policy_rom_roots(policy, &save_roots);
    let recursive = policy.recursive.unwrap_or(true);
    let mut source = Source::new(name, kind, save_roots, rom_roots, recursive);
    if let Some(id) = policy.id_value() {
        source.id = normalize_source_id(id);
    }
    if let Some(profile) = policy.profile.as_deref().and_then(EmulatorProfile::parse) {
        source.profile = profile;
    } else {
        source.profile = default_profile_for_kind(&source.kind);
    }
    if let Some(systems) = policy.systems.as_ref() {
        source.systems = normalize_systems(systems);
    }
    if policy.enabled == Some(false) {
        source.systems.clear();
    }
    if let Some(value) = policy.create_missing_system_dirs {
        source.create_missing_system_dirs = value;
    }
    source.managed = policy.managed.unwrap_or(false);
    source.origin = policy
        .origin
        .clone()
        .unwrap_or_else(|| "backend-policy".to_string());
    Some(source)
}

fn apply_policy_to_stored_source(source: &mut Source, policy: &BackendSourcePolicy) {
    if let Some(name) = policy.name_value() {
        source.name = name.to_string();
    }
    if let Some(kind) = policy.kind.as_deref().and_then(SourceKind::parse) {
        source.kind = kind;
    }
    if let Some(profile) = policy.profile.as_deref().and_then(EmulatorProfile::parse) {
        source.profile = profile;
    }
    let save_roots = source_policy_save_roots(policy);
    if !save_roots.is_empty() {
        source.save_roots = save_roots;
    }
    let rom_roots = source_policy_rom_roots(policy, &source.save_roots);
    if !rom_roots.is_empty() {
        source.rom_roots = rom_roots;
    }
    if let Some(recursive) = policy.recursive {
        source.recursive = recursive;
    }
    if let Some(systems) = policy.systems.as_ref() {
        source.systems = normalize_systems(systems);
    }
    if policy.enabled == Some(false) {
        source.systems.clear();
    }
    if let Some(value) = policy.create_missing_system_dirs {
        source.create_missing_system_dirs = value;
    }
    if let Some(value) = policy.managed {
        source.managed = value;
    }
    if let Some(value) = policy.origin.as_ref() {
        source.origin = value.clone();
    } else if source.origin.trim().is_empty() {
        source.origin = "backend-policy".to_string();
    }
}

fn source_policy_save_roots(policy: &BackendSourcePolicy) -> Vec<PathBuf> {
    if !policy.save_roots.is_empty() {
        return policy.save_roots.clone();
    }
    policy.save_path.iter().cloned().collect()
}

fn source_policy_rom_roots(policy: &BackendSourcePolicy, save_roots: &[PathBuf]) -> Vec<PathBuf> {
    if !policy.rom_roots.is_empty() {
        return policy.rom_roots.clone();
    }
    if let Some(path) = policy.rom_path.clone() {
        return vec![path];
    }
    save_roots.to_vec()
}

fn read_file_if_exists(path: &Path) -> Result<String> {
    if !path.exists() {
        return Ok(String::new());
    }
    fs::read_to_string(path).with_context(|| format!("kan config niet lezen: {}", path.display()))
}

fn write_file(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("kan config map niet maken: {}", parent.display()))?;
    }
    fs::write(path, content)
        .with_context(|| format!("kan config niet schrijven: {}", path.display()))
}

fn backup_config_if_exists(path: &Path) -> Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    let backup = path.with_extension(format!("ini.backend.{}", timestamp_suffix()));
    fs::copy(path, &backup).with_context(|| {
        format!(
            "kan config backup niet schrijven: {} -> {}",
            path.display(),
            backup.display()
        )
    })?;
    Ok(Some(backup))
}

fn timestamp_suffix() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| OffsetDateTime::now_utc().unix_timestamp().to_string())
        .replace([':', '-'], "")
        .replace(['T', 'Z'], "")
}

fn escape_ini(value: &str) -> String {
    value.replace('"', "\\\"")
}

fn normalize_source_id(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if (ch.is_ascii_whitespace() || ch == '-' || ch == '_' || ch == '/')
            && !out.ends_with('_')
        {
            out.push('_');
        }
    }
    let normalized = out.trim_matches('_').to_string();
    if normalized.is_empty() {
        "backend_source".to_string()
    } else {
        normalized
    }
}

fn apply_backend_response(
    sources: &mut [ResolvedSource],
    response: &BackendConfigSyncResponse,
    verbose: bool,
) -> RuntimeConfigOverrides {
    let mut overrides = RuntimeConfigOverrides::default();
    apply_global_policy(&mut overrides, response.global.as_ref());
    apply_global_policy(
        &mut overrides,
        response
            .policy
            .as_ref()
            .and_then(|value| value.global.as_ref()),
    );
    apply_global_policy(
        &mut overrides,
        response
            .desired_config
            .as_ref()
            .and_then(|value| value.global.as_ref()),
    );
    apply_global_policy(
        &mut overrides,
        response
            .effective_config
            .as_ref()
            .and_then(|value| value.global.as_ref()),
    );

    for policy in source_policies(response) {
        for source in sources
            .iter_mut()
            .filter(|source| policy_matches(source, policy))
        {
            apply_source_policy(source, policy, verbose);
        }
    }
    overrides
}

fn apply_global_policy(
    overrides: &mut RuntimeConfigOverrides,
    policy: Option<&BackendGlobalPolicy>,
) {
    let Some(policy) = policy else {
        return;
    };
    if policy.force_upload.is_some() {
        overrides.force_upload = policy.force_upload;
    }
    if policy.dry_run.is_some() {
        overrides.dry_run = policy.dry_run;
    }
}

fn source_policies(response: &BackendConfigSyncResponse) -> Vec<&BackendSourcePolicy> {
    let mut out = Vec::new();
    out.extend(response.sources.iter());
    if let Some(policy) = response.policy.as_ref() {
        out.extend(policy.sources.iter());
    }
    if let Some(policy) = response.desired_config.as_ref() {
        out.extend(policy.sources.iter());
    }
    if let Some(policy) = response.effective_config.as_ref() {
        out.extend(policy.sources.iter());
    }
    out
}

fn policy_matches(source: &ResolvedSource, policy: &BackendSourcePolicy) -> bool {
    if let Some(id) = policy.id_value()
        && source.id == id
    {
        return true;
    }
    if let Some(name) = policy.name_value()
        && source.name == name
    {
        return true;
    }
    false
}

fn apply_source_policy(source: &mut ResolvedSource, policy: &BackendSourcePolicy, verbose: bool) {
    if policy.enabled == Some(false) {
        source.systems.clear();
    }
    if let Some(kind) = policy.kind.as_deref().and_then(SourceKind::parse) {
        source.kind = kind;
    }
    if let Some(profile) = policy.profile.as_deref().and_then(EmulatorProfile::parse) {
        source.profile = profile;
    }
    if !policy.save_roots.is_empty() {
        source.save_roots = policy.save_roots.clone();
    } else if let Some(path) = policy.save_path.clone() {
        source.save_roots = vec![path];
    }
    if !policy.rom_roots.is_empty() {
        source.rom_roots = policy.rom_roots.clone();
    } else if let Some(path) = policy.rom_path.clone() {
        source.rom_roots = vec![path];
    }
    if let Some(recursive) = policy.recursive {
        source.recursive = recursive;
    }
    if let Some(systems) = policy.systems.as_ref() {
        source.systems = normalize_systems(systems);
    }
    if let Some(value) = policy.create_missing_system_dirs {
        source.create_missing_system_dirs = value;
    }
    if verbose {
        eprintln!(
            "Applied backend config policy for source '{}' (managed={}, systems={})",
            source.name,
            source.managed,
            if source.systems.is_empty() {
                "none".to_string()
            } else {
                source.systems.join(",")
            }
        );
    }
}

fn normalize_systems(values: &[String]) -> Vec<String> {
    let mut out = values
        .iter()
        .map(|value| value.trim().replace(['_', ' '], "-").to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use crate::sources::load_source_store;
    use std::fs;

    fn source() -> ResolvedSource {
        ResolvedSource {
            id: "mister_default".to_string(),
            name: "MiSTer Default".to_string(),
            kind: SourceKind::MisterFpga,
            profile: EmulatorProfile::Mister,
            save_roots: vec![PathBuf::from("/media/fat/saves")],
            rom_roots: vec![PathBuf::from("/media/fat/games")],
            recursive: true,
            systems: vec!["snes".to_string(), "n64".to_string()],
            create_missing_system_dirs: false,
            managed: false,
            origin: "manual".to_string(),
        }
    }

    #[test]
    fn backend_policy_applies_to_manual_sources() {
        let mut sources = vec![source()];
        let response: BackendConfigSyncResponse = serde_json::from_value(serde_json::json!({
            "policy": {
                "sources": [{
                    "id": "mister_default",
                    "systems": ["snes"],
                    "createMissingSystemDirs": true
                }]
            }
        }))
        .unwrap();

        let overrides = apply_backend_response(&mut sources, &response, false);
        assert_eq!(overrides, RuntimeConfigOverrides::default());
        assert_eq!(sources[0].systems, vec!["snes"]);
        assert!(sources[0].create_missing_system_dirs);
        assert!(!sources[0].managed);
    }

    #[test]
    fn backend_policy_can_disable_source_and_override_runtime_flags() {
        let mut sources = vec![source()];
        let response: BackendConfigSyncResponse = serde_json::from_value(serde_json::json!({
            "global": {
                "forceUpload": true,
                "dryRun": false
            },
            "sources": [{
                "sourceId": "mister_default",
                "enabled": false
            }]
        }))
        .unwrap();

        let overrides = apply_backend_response(&mut sources, &response, false);
        assert_eq!(overrides.force_upload, Some(true));
        assert_eq!(overrides.dry_run, Some(false));
        assert!(sources[0].systems.is_empty());
    }

    #[test]
    fn backend_policy_accepts_ui_string_and_null_values() {
        let mut sources = vec![source()];
        let response: BackendConfigSyncResponse = serde_json::from_value(serde_json::json!({
            "policy": {
                "global": {
                    "port": "80",
                    "forceUpload": "true",
                    "dryRun": "false",
                    "watch": null,
                    "watchInterval": "45"
                },
                "sources": [{
                    "sourceId": "mister_default",
                    "enabled": "true",
                    "savePaths": "/media/fat/saves",
                    "romPaths": null,
                    "recursive": "false",
                    "systems": "snes,n64",
                    "createMissingSystemDirs": "true"
                }]
            }
        }))
        .unwrap();

        let overrides = apply_backend_response(&mut sources, &response, false);
        assert_eq!(overrides.force_upload, Some(true));
        assert_eq!(overrides.dry_run, Some(false));
        assert_eq!(
            sources[0].save_roots,
            vec![PathBuf::from("/media/fat/saves")]
        );
        assert!(!sources[0].recursive);
        assert_eq!(sources[0].systems, vec!["n64", "snes"]);
        assert!(sources[0].create_missing_system_dirs);
    }

    #[test]
    fn backend_policy_writes_new_source_and_globals_to_config_ini() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.ini");
        fs::write(
            &config_path,
            "URL=\"192.168.2.10\"\nPORT=\"80\"\n\n[source.old]\nLABEL=\"Old\"\nKIND=\"custom\"\nSAVE_PATH=\"/old/saves\"\nROM_PATH=\"/old/roms\"\nRECURSIVE=\"true\"\nSYSTEMS=\"snes\"\nMANAGED=\"false\"\nORIGIN=\"manual\"\n",
        )
        .unwrap();
        let config = test_config(&config_path, tmp.path());
        let response: BackendConfigSyncResponse = serde_json::from_value(serde_json::json!({
            "policy": {
                "global": {
                    "forceUpload": true,
                    "dryRun": false,
                    "watchInterval": 15
                },
                "sources": [{
                    "id": "super_nintendo_snes9x",
                    "label": "Super Nintendo Snes9x",
                    "kind": "retroarch",
                    "profile": "snes9x",
                    "savePath": "/media/snes9x/saves",
                    "romPath": "/media/snes9x/roms",
                    "recursive": true,
                    "systems": ["snes"],
                    "createMissingSystemDirs": false,
                    "origin": "backend-ui"
                }]
            }
        }))
        .unwrap();

        write_backend_policy_to_config(&config, &response, false).unwrap();

        let body = fs::read_to_string(&config_path).unwrap();
        assert!(body.contains("FORCE_UPLOAD=\"true\""));
        assert!(body.contains("DRY_RUN=\"false\""));
        assert!(body.contains("WATCH_INTERVAL=\"15\""));
        assert!(body.contains("[source.super_nintendo_snes9x]"));
        assert!(body.contains("PROFILE=\"snes9x\""));
        assert!(body.contains("SAVE_PATH=\"/media/snes9x/saves\""));
        assert!(tmp.path().read_dir().unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("backend")
        }));

        let store = load_source_store(&config_path).unwrap();
        let added = store
            .sources
            .iter()
            .find(|source| source.id == "super_nintendo_snes9x")
            .unwrap();
        assert_eq!(added.systems, vec!["snes"]);
        assert_eq!(added.profile, EmulatorProfile::Snes9x);
        assert_eq!(added.origin, "backend-ui");
    }

    #[test]
    fn backend_disabled_source_is_persisted_as_none_systems() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.ini");
        fs::write(
            &config_path,
            "URL=\"192.168.2.10\"\nPORT=\"80\"\n\n[source.snes]\nLABEL=\"SNES\"\nKIND=\"retroarch\"\nSAVE_PATH=\"/saves\"\nROM_PATH=\"/roms\"\nRECURSIVE=\"true\"\nSYSTEMS=\"snes\"\nMANAGED=\"false\"\nORIGIN=\"manual\"\n",
        )
        .unwrap();
        let config = test_config(&config_path, tmp.path());
        let response: BackendConfigSyncResponse = serde_json::from_value(serde_json::json!({
            "sources": [{
                "id": "snes",
                "enabled": false
            }]
        }))
        .unwrap();

        write_backend_policy_to_config(&config, &response, false).unwrap();

        let body = fs::read_to_string(&config_path).unwrap();
        assert!(body.contains("SYSTEMS=\"none\""));
        let store = load_source_store(&config_path).unwrap();
        assert!(store.sources[0].systems.is_empty());
    }

    fn test_config(config_path: &Path, root: &Path) -> AppConfig {
        AppConfig {
            url: "192.168.2.10".to_string(),
            port: 80,
            email: String::new(),
            app_password: String::new(),
            root: root.to_path_buf(),
            state_dir: root.join("state"),
            watch: false,
            watch_interval: 30,
            force_upload: false,
            dry_run: false,
            route_prefix: String::new(),
            binary_dir: root.to_path_buf(),
            config_path: config_path.to_path_buf(),
        }
    }
}
