use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default)]
pub struct ConfigOverrides {
    pub url: Option<String>,
    pub api_url: Option<String>,
    pub port: Option<u16>,
    pub email: Option<String>,
    pub app_password: Option<String>,
    pub root: Option<PathBuf>,
    pub state_dir: Option<PathBuf>,
    pub watch: Option<bool>,
    pub watch_interval: Option<u64>,
    pub force_upload: Option<bool>,
    pub dry_run: Option<bool>,
    pub route_prefix: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub url: String,
    pub port: u16,
    pub email: String,
    pub app_password: String,
    pub root: PathBuf,
    pub state_dir: PathBuf,
    pub watch: bool,
    pub watch_interval: u64,
    pub force_upload: bool,
    pub dry_run: bool,
    pub route_prefix: String,
    pub binary_dir: PathBuf,
    pub config_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct LoadedConfig {
    pub config: AppConfig,
    pub config_path: PathBuf,
}

impl LoadedConfig {
    pub fn load(
        config_path_override: Option<PathBuf>,
        overrides: &ConfigOverrides,
    ) -> Result<Self> {
        let binary_dir = default_binary_dir()?;
        let config_path = config_path_override.unwrap_or_else(|| binary_dir.join("config.ini"));
        let ini_values = parse_ini_file_if_exists(&config_path)?;
        let env_values = collect_env_values();

        let config = AppConfig::from_sources(
            overrides,
            &env_values,
            &ini_values,
            binary_dir,
            &config_path,
        )?;
        Ok(Self {
            config,
            config_path,
        })
    }
}

impl AppConfig {
    pub fn base_url(&self) -> String {
        format!("http://{}:{}", self.url, self.port)
    }

    pub fn resolved_root(&self) -> Result<PathBuf> {
        resolve_from_binary_dir(&self.binary_dir, &self.root)
    }

    pub fn resolved_state_dir(&self) -> Result<PathBuf> {
        resolve_from_binary_dir(&self.binary_dir, &self.state_dir)
    }

    fn from_sources(
        overrides: &ConfigOverrides,
        env_values: &HashMap<String, String>,
        ini_values: &HashMap<String, String>,
        binary_dir: PathBuf,
        config_path: &Path,
    ) -> Result<Self> {
        if let Some(api_url) = overrides
            .api_url
            .clone()
            .or_else(|| env_values.get("ONE_RETRO_API_URL").cloned())
            .or_else(|| env_values.get("API_URL").cloned())
            .or_else(|| ini_values.get("API_URL").cloned())
        {
            let (url, port) = parse_api_url_host_port(&api_url)?;
            let mut merged_overrides = overrides.clone();
            merged_overrides.url = Some(url);
            merged_overrides.port = Some(port);
            merged_overrides.api_url = None;
            return AppConfig::from_sources(
                &merged_overrides,
                env_values,
                ini_values,
                binary_dir,
                config_path,
            );
        }

        let url = choose_string(
            overrides.url.clone(),
            env_values,
            ini_values,
            "URL",
            "127.0.0.1",
        )?;
        validate_host_only(&url)?;

        let port = choose_u16(overrides.port, env_values, ini_values, "PORT", 3001)?;
        let email = choose_string(overrides.email.clone(), env_values, ini_values, "EMAIL", "")?;
        let app_password = choose_string(
            overrides.app_password.clone(),
            env_values,
            ini_values,
            "APP_PASSWORD",
            "",
        )?;
        let root = choose_path(
            overrides.root.clone(),
            env_values,
            ini_values,
            "ROOT",
            PathBuf::from("/home/deck/.steam/steam/steamapps/compatdata"),
        )?;
        let state_dir = choose_path(
            overrides.state_dir.clone(),
            env_values,
            ini_values,
            "STATE_DIR",
            PathBuf::from("./state"),
        )?;
        let watch = choose_bool(overrides.watch, env_values, ini_values, "WATCH", false)?;
        let watch_interval = choose_u64(
            overrides.watch_interval,
            env_values,
            ini_values,
            "WATCH_INTERVAL",
            30,
        )?;
        let force_upload = choose_bool(
            overrides.force_upload,
            env_values,
            ini_values,
            "FORCE_UPLOAD",
            false,
        )?;
        let dry_run = choose_bool(overrides.dry_run, env_values, ini_values, "DRY_RUN", false)?;
        let route_prefix = normalize_route_prefix(&choose_string(
            overrides.route_prefix.clone(),
            env_values,
            ini_values,
            "ROUTE_PREFIX",
            "",
        )?);

        if watch_interval == 0 {
            bail!("WATCH_INTERVAL moet >= 1 zijn");
        }

        Ok(Self {
            url,
            port,
            email,
            app_password,
            root,
            state_dir,
            watch,
            watch_interval,
            force_upload,
            dry_run,
            route_prefix,
            binary_dir,
            config_path: config_path.to_path_buf(),
        })
    }
}

fn default_binary_dir() -> Result<PathBuf> {
    let exe = env::current_exe().context("kan huidige executable pad niet bepalen")?;
    let dir = exe
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    Ok(dir)
}

fn resolve_from_binary_dir(binary_dir: &Path, candidate: &Path) -> Result<PathBuf> {
    if candidate.is_absolute() {
        return Ok(candidate.to_path_buf());
    }
    Ok(binary_dir.join(candidate))
}

fn parse_ini_file_if_exists(path: &Path) -> Result<HashMap<String, String>> {
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("kan config bestand niet lezen: {}", path.display()))?;
    parse_ini_content(&content)
}

fn parse_ini_content(content: &str) -> Result<HashMap<String, String>> {
    let mut values = HashMap::new();
    let mut in_section = false;
    for (idx, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_section = true;
            continue;
        }

        if in_section {
            if trimmed.find('=').is_none() {
                continue;
            }
            // Source sections are parsed by sources.rs; global config intentionally
            // ignores values inside INI sections.
            continue;
        }

        let Some(eq_pos) = trimmed.find('=') else {
            bail!("ongeldige INI regel {}: ontbrekende '='", idx + 1);
        };

        let key = trimmed[..eq_pos].trim().to_uppercase();
        let mut value = trimmed[eq_pos + 1..].trim().to_string();
        if value.starts_with('"') && value.ends_with('"') && value.len() >= 2 {
            value = value[1..value.len() - 1].to_string();
        }
        values.insert(key, value);
    }

    Ok(values)
}

fn collect_env_values() -> HashMap<String, String> {
    let mut values = HashMap::new();
    for key in [
        "ONE_RETRO_API_URL",
        "API_URL",
        "URL",
        "PORT",
        "EMAIL",
        "APP_PASSWORD",
        "ROOT",
        "STATE_DIR",
        "WATCH",
        "WATCH_INTERVAL",
        "FORCE_UPLOAD",
        "DRY_RUN",
        "ROUTE_PREFIX",
    ] {
        if let Some(value) = read_env_aliases(key) {
            values.insert(key.to_string(), value);
        }
    }
    values
}

fn read_env_aliases(key: &str) -> Option<String> {
    let prefixed = format!("SGM_{}", key);
    env::var(prefixed)
        .ok()
        .or_else(|| env::var(key).ok())
        .map(|value| value.trim().to_string())
}

fn choose_string(
    cli: Option<String>,
    env_values: &HashMap<String, String>,
    ini_values: &HashMap<String, String>,
    key: &str,
    default: &str,
) -> Result<String> {
    if let Some(value) = cli {
        return Ok(value);
    }
    if let Some(value) = env_values.get(key) {
        return Ok(value.clone());
    }
    if let Some(value) = ini_values.get(key) {
        return Ok(value.clone());
    }
    Ok(default.to_string())
}

fn choose_path(
    cli: Option<PathBuf>,
    env_values: &HashMap<String, String>,
    ini_values: &HashMap<String, String>,
    key: &str,
    default: PathBuf,
) -> Result<PathBuf> {
    if let Some(value) = cli {
        return Ok(value);
    }
    if let Some(value) = env_values.get(key) {
        return Ok(PathBuf::from(value));
    }
    if let Some(value) = ini_values.get(key) {
        return Ok(PathBuf::from(value));
    }
    Ok(default)
}

fn choose_u16(
    cli: Option<u16>,
    env_values: &HashMap<String, String>,
    ini_values: &HashMap<String, String>,
    key: &str,
    default: u16,
) -> Result<u16> {
    if let Some(value) = cli {
        return Ok(value);
    }
    if let Some(value) = env_values.get(key).or_else(|| ini_values.get(key)) {
        return value
            .parse::<u16>()
            .with_context(|| format!("{} moet een geldige u16 zijn", key));
    }
    Ok(default)
}

fn choose_u64(
    cli: Option<u64>,
    env_values: &HashMap<String, String>,
    ini_values: &HashMap<String, String>,
    key: &str,
    default: u64,
) -> Result<u64> {
    if let Some(value) = cli {
        return Ok(value);
    }
    if let Some(value) = env_values.get(key).or_else(|| ini_values.get(key)) {
        return value
            .parse::<u64>()
            .with_context(|| format!("{} moet een geldige u64 zijn", key));
    }
    Ok(default)
}

fn choose_bool(
    cli: Option<bool>,
    env_values: &HashMap<String, String>,
    ini_values: &HashMap<String, String>,
    key: &str,
    default: bool,
) -> Result<bool> {
    if let Some(value) = cli {
        return Ok(value);
    }
    if let Some(value) = env_values.get(key).or_else(|| ini_values.get(key)) {
        return parse_bool(value)
            .with_context(|| format!("{} moet true/false, 1/0, yes/no of on/off zijn", key));
    }
    Ok(default)
}

fn parse_bool(value: &str) -> Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => bail!("ongeldige bool '{}'", value),
    }
}

fn validate_host_only(host: &str) -> Result<()> {
    let trimmed = host.trim();
    if trimmed.is_empty() {
        bail!("URL mag niet leeg zijn");
    }
    if trimmed.contains("://") {
        bail!("URL moet alleen host/IP bevatten, zonder schema (bijv. 192.168.1.1)");
    }
    if trimmed.contains('/') {
        bail!("URL mag geen pad bevatten");
    }
    Ok(())
}

fn parse_api_url_host_port(value: &str) -> Result<(String, u16)> {
    let raw = value.trim();
    if raw.is_empty() {
        bail!("API_URL/ONE_RETRO_API_URL mag niet leeg zijn");
    }
    let without_scheme = raw
        .strip_prefix("http://")
        .or_else(|| raw.strip_prefix("https://"))
        .unwrap_or(raw);
    let authority = without_scheme
        .split('/')
        .next()
        .unwrap_or(without_scheme)
        .trim();
    if authority.is_empty() {
        bail!("ongeldige API_URL/ONE_RETRO_API_URL");
    }

    let (host, port) = if let Some((host, port)) = authority.rsplit_once(':') {
        let parsed_port = port
            .parse::<u16>()
            .with_context(|| "ongeldige poort in API_URL/ONE_RETRO_API_URL")?;
        (host.to_string(), parsed_port)
    } else {
        (authority.to_string(), 80)
    };
    validate_host_only(&host)?;
    Ok((host, port))
}

fn normalize_route_prefix(prefix: &str) -> String {
    let mut trimmed = prefix.trim().trim_end_matches('/').to_string();
    if trimmed == "/" {
        return String::new();
    }
    if trimmed.is_empty() {
        return String::new();
    }
    if !trimmed.starts_with('/') {
        trimmed.insert(0, '/');
    }
    trimmed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_sources(
        cli: ConfigOverrides,
        env: &[(&str, &str)],
        ini: &[(&str, &str)],
    ) -> Result<AppConfig> {
        let env_map: HashMap<String, String> = env
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        let ini_map: HashMap<String, String> = ini
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        AppConfig::from_sources(
            &cli,
            &env_map,
            &ini_map,
            PathBuf::from("/tmp/bin"),
            Path::new("/tmp/bin/config.ini"),
        )
    }

    #[test]
    fn ini_parser_accepts_quoted_values() {
        let parsed = parse_ini_content("URL=\"192.168.1.1\"\nPORT=\"9096\"\n").unwrap();
        assert_eq!(parsed.get("URL").unwrap(), "192.168.1.1");
        assert_eq!(parsed.get("PORT").unwrap(), "9096");
    }

    #[test]
    fn ini_parser_ignores_source_sections_for_global_values() {
        let parsed = parse_ini_content(
            r#"URL="192.168.1.1"
PORT="9096"
[source.snes]
LABEL="SNES"
SAVE_PATH="/home/snes9x/save"
"#,
        )
        .unwrap();
        assert_eq!(parsed.get("URL").unwrap(), "192.168.1.1");
        assert_eq!(parsed.get("PORT").unwrap(), "9096");
        assert!(!parsed.contains_key("SAVE_PATH"));
    }

    #[test]
    fn base_url_is_built_from_url_and_port() {
        let cfg = test_sources(
            ConfigOverrides::default(),
            &[("URL", "192.168.1.9"), ("PORT", "9096")],
            &[],
        )
        .unwrap();
        assert_eq!(cfg.base_url(), "http://192.168.1.9:9096");
    }

    #[test]
    fn precedence_cli_over_env_over_ini_over_default() {
        let cfg = test_sources(
            ConfigOverrides {
                url: Some("10.0.0.5".into()),
                api_url: None,
                ..ConfigOverrides::default()
            },
            &[("URL", "10.0.0.4")],
            &[("URL", "10.0.0.3")],
        )
        .unwrap();
        assert_eq!(cfg.url, "10.0.0.5");

        let cfg = test_sources(
            ConfigOverrides::default(),
            &[("URL", "10.0.0.4")],
            &[("URL", "10.0.0.3")],
        )
        .unwrap();
        assert_eq!(cfg.url, "10.0.0.4");

        let cfg = test_sources(ConfigOverrides::default(), &[], &[("URL", "10.0.0.3")]).unwrap();
        assert_eq!(cfg.url, "10.0.0.3");

        let cfg = test_sources(ConfigOverrides::default(), &[], &[]).unwrap();
        assert_eq!(cfg.url, "127.0.0.1");
    }

    #[test]
    fn bool_and_int_parsing() {
        let cfg = test_sources(
            ConfigOverrides::default(),
            &[
                ("WATCH", "true"),
                ("FORCE_UPLOAD", "1"),
                ("DRY_RUN", "off"),
                ("WATCH_INTERVAL", "45"),
            ],
            &[],
        )
        .unwrap();

        assert!(cfg.watch);
        assert!(cfg.force_upload);
        assert!(!cfg.dry_run);
        assert_eq!(cfg.watch_interval, 45);
    }
}
