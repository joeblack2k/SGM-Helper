use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::api::ApiClient;
use crate::config::AppConfig;
use crate::sources::{EmulatorProfile, ResolvedSource, SourceKind, default_systems_for_kind};

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
    #[serde(rename = "forceUpload", alias = "force_upload", alias = "FORCE_UPLOAD")]
    pub force_upload: Option<bool>,
    #[serde(rename = "dryRun", alias = "dry_run", alias = "DRY_RUN")]
    pub dry_run: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct BackendSourcePolicy {
    #[serde(alias = "sourceId", alias = "source_id")]
    pub id: Option<String>,
    #[serde(alias = "label", alias = "name")]
    pub name: Option<String>,
    pub enabled: Option<bool>,
    pub kind: Option<String>,
    pub profile: Option<String>,
    #[serde(alias = "savePaths", alias = "save_roots", alias = "SAVE_PATH")]
    pub save_roots: Vec<PathBuf>,
    #[serde(alias = "savePath")]
    pub save_path: Option<PathBuf>,
    #[serde(alias = "romPaths", alias = "rom_roots", alias = "ROM_PATH")]
    pub rom_roots: Vec<PathBuf>,
    #[serde(alias = "romPath")]
    pub rom_path: Option<PathBuf>,
    pub recursive: Option<bool>,
    pub systems: Option<Vec<String>>,
    #[serde(
        rename = "createMissingSystemDirs",
        alias = "create_missing_system_dirs",
        alias = "CREATE_MISSING_SYSTEM_DIRS"
    )]
    pub create_missing_system_dirs: Option<bool>,
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

fn capability_matrix() -> serde_json::Value {
    let kinds = [
        SourceKind::MisterFpga,
        SourceKind::RetroArch,
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
            "manualManagedPolicy": "MANAGED=false prevents config-file writeback only; backend policy still applies at runtime."
        }
    })
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
    if let Some(id) = policy.id.as_deref()
        && source.id == id
    {
        return true;
    }
    if let Some(name) = policy.name.as_deref()
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
    fn backend_policy_applies_to_manual_sources_without_writeback() {
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
}
