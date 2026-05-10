use std::io::Read;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::StatusCode;
use reqwest::blocking::{Client, RequestBuilder, Response};
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue, USER_AGENT};
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct ApiClient {
    base_url: String,
    route_prefix: String,
    token: Option<String>,
    client: Client,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TokenResponse {
    pub token: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LoginResponse {
    pub user: Option<AuthUser>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AuthMeResponse {
    pub user: Option<AuthUser>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AuthUser {
    pub email: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AutoEnrollStatusResponse {
    pub active: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AutoProvisionAppPasswordResponse {
    pub token: Option<String>,
    #[serde(rename = "plainTextKey")]
    pub plain_text_key: Option<String>,
    #[serde(rename = "plainTextToken")]
    pub plain_text_token: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AutoProvisionRequest {
    pub name: String,
    #[serde(rename = "deviceType")]
    pub device_type: String,
    pub fingerprint: String,
    pub hostname: String,
    #[serde(rename = "helperName")]
    pub helper_name: String,
    #[serde(rename = "helperVersion")]
    pub helper_version: String,
    pub platform: String,
    #[serde(rename = "syncPaths", skip_serializing_if = "Vec::is_empty")]
    pub sync_paths: Vec<String>,
    #[serde(rename = "systems", skip_serializing_if = "Vec::is_empty")]
    pub systems: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LatestSaveResponse {
    pub exists: bool,
    pub sha256: Option<String>,
    pub version: Option<i64>,
    pub id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct LatestSaveContext {
    pub filename: String,
    pub system_slug: String,
    pub display_title: String,
    pub region_code: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct LatestSaveRequest<'a> {
    pub rom_sha1: &'a str,
    pub slot_name: &'a str,
    pub device_type: &'a str,
    pub fingerprint: &'a str,
    pub app_password: Option<&'a str>,
    pub runtime_target: Option<&'a RuntimeTarget>,
    pub context: Option<&'a LatestSaveContext>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LookupRomResponse {
    pub count: Option<u64>,
    pub rom: Option<RomRecord>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RomRecord {
    pub sha1: Option<String>,
    pub md5: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UploadSaveResponse {
    pub save: Option<UploadedSave>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ListSavesResponse {
    #[serde(default)]
    pub success: bool,
    #[serde(default)]
    pub total: usize,
    #[serde(default)]
    pub limit: usize,
    #[serde(default)]
    pub offset: usize,
    #[serde(default)]
    pub saves: Vec<CloudSaveSummary>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CloudSaveSummary {
    pub id: String,
    #[serde(default)]
    pub filename: String,
    #[serde(default, rename = "displayTitle")]
    pub display_title: String,
    #[serde(default, rename = "systemSlug")]
    pub system_slug: String,
    #[serde(default)]
    pub game: Option<CloudSaveGame>,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub version: Option<i64>,
    #[serde(default, rename = "fileSize")]
    pub file_size: Option<u64>,
    #[serde(default, rename = "latestSizeBytes")]
    pub latest_size_bytes: Option<u64>,
    #[serde(default, rename = "mediaType")]
    pub media_type: Option<String>,
    #[serde(default, rename = "runtimeProfile")]
    pub runtime_profile: Option<String>,
    #[serde(default, rename = "sourceArtifactProfile")]
    pub source_artifact_profile: Option<String>,
    #[serde(default, rename = "logicalKey")]
    pub logical_key: Option<String>,
    #[serde(default, rename = "cardSlot")]
    pub card_slot: Option<String>,
    #[serde(default, rename = "downloadProfiles")]
    pub download_profiles: Vec<DownloadProfile>,
    #[serde(default)]
    pub inspection: Option<serde_json::Value>,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
    #[serde(default, rename = "romSha1")]
    pub rom_sha1: Option<String>,
    #[serde(default, rename = "romMd5")]
    pub rom_md5: Option<String>,
}

impl CloudSaveSummary {
    pub fn system_slug(&self) -> Option<&str> {
        let direct = self.system_slug.trim();
        if !direct.is_empty() {
            return Some(direct);
        }
        self.game
            .as_ref()
            .and_then(|game| game.system.as_ref())
            .map(|system| system.slug.trim())
            .filter(|slug| !slug.is_empty())
    }

    pub fn display_name(&self) -> &str {
        let title = self.display_title.trim();
        if !title.is_empty() {
            return title;
        }
        if let Some(game) = self.game.as_ref() {
            let display_title = game.display_title.trim();
            if !display_title.is_empty() {
                return display_title;
            }
            let name = game.name.trim();
            if !name.is_empty() {
                return name;
            }
        }
        self.filename.trim()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct CloudSaveGame {
    #[serde(default)]
    pub name: String,
    #[serde(default, rename = "displayTitle")]
    pub display_title: String,
    #[serde(default)]
    pub system: Option<CloudSaveSystem>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CloudSaveSystem {
    #[serde(default)]
    pub slug: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DownloadProfile {
    pub id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default, rename = "targetExtension")]
    pub target_extension: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UploadedSave {
    pub id: Option<String>,
    pub sha256: Option<String>,
    pub version: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConflictCheckResponse {
    pub exists: bool,
    #[serde(rename = "conflictId")]
    pub conflict_id: Option<String>,
    #[serde(rename = "cloudSha256")]
    pub cloud_sha256: Option<String>,
    #[serde(rename = "cloudVersion")]
    pub cloud_version: Option<i64>,
    #[serde(rename = "cloudSaveId")]
    pub cloud_save_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConflictReportResponse {
    pub success: Option<bool>,
    #[serde(rename = "conflictId")]
    pub conflict_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct RuntimeTarget {
    pub runtime_profile: Option<String>,
    pub emulator_profile: Option<String>,
    pub system_profile_key: Option<String>,
    pub system_profile_value: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PortUploadMetadata {
    pub port_id: String,
    pub port_name: String,
    pub origin_system_slug: String,
    pub port_save_kind: String,
    pub relative_path: String,
    pub root_relative_path: String,
    pub slot_id: String,
    pub display_title: String,
}

impl RuntimeTarget {
    fn query_pairs(&self) -> Vec<(String, String)> {
        let mut pairs: Vec<(String, String)> = Vec::new();

        if let Some(runtime_profile) = self.runtime_profile.as_deref() {
            push_runtime_pair(&mut pairs, "runtimeProfile", runtime_profile);
            push_runtime_pair(&mut pairs, "runtime_profile", runtime_profile);
        }
        if let Some(emulator_profile) = self.emulator_profile.as_deref() {
            push_runtime_pair(&mut pairs, "emulatorProfile", emulator_profile);
            push_runtime_pair(&mut pairs, "emulator_profile", emulator_profile);
        }
        if let (Some(system_key), Some(system_value)) = (
            self.system_profile_key.as_deref(),
            self.system_profile_value.as_deref(),
        ) {
            push_runtime_pair(&mut pairs, system_key, system_value);
            let snake_key = camel_case_to_snake_case(system_key);
            push_runtime_pair(&mut pairs, &snake_key, system_value);
        }

        pairs
    }

    fn apply_query(&self, request: RequestBuilder) -> RequestBuilder {
        let pairs = self.query_pairs();
        if pairs.is_empty() {
            request
        } else {
            request.query(&pairs)
        }
    }

    fn apply_multipart_form(
        &self,
        mut form: reqwest::blocking::multipart::Form,
    ) -> reqwest::blocking::multipart::Form {
        for (key, value) in self.query_pairs() {
            form = form.text(key, value);
        }
        form
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeviceAuthResponse {
    #[serde(rename = "deviceCode")]
    pub device_code: String,
    #[serde(rename = "userCode")]
    pub user_code: String,
    #[serde(rename = "verificationUri")]
    pub verification_uri: String,
    #[serde(rename = "expiresInSeconds")]
    pub expires_in_seconds: u64,
}

#[derive(Debug, Clone)]
pub enum DeviceTokenPoll {
    Pending,
    Success(TokenResponse),
}

#[derive(Debug, Clone, Serialize)]
struct AppPasswordRequest<'a> {
    email: &'a str,
    #[serde(rename = "appPassword")]
    app_password: &'a str,
}

#[derive(Debug, Clone, Serialize)]
struct SignupRequest<'a> {
    email: &'a str,
    #[serde(rename = "displayName")]
    display_name: &'a str,
    password: &'a str,
    #[serde(rename = "skipVerification")]
    skip_verification: bool,
}

#[derive(Debug, Clone, Serialize)]
struct ResendVerificationRequest<'a> {
    email: &'a str,
}

#[derive(Debug, Clone, Serialize)]
struct LoginRequest<'a> {
    email: &'a str,
    password: &'a str,
    #[serde(rename = "deviceType")]
    device_type: &'a str,
    fingerprint: &'a str,
}

#[derive(Debug, Clone, Serialize)]
struct DeviceTokenRequest<'a> {
    #[serde(rename = "deviceCode")]
    device_code: &'a str,
}

impl ApiClient {
    pub fn new(base_url: String, route_prefix: String, token: Option<String>) -> Result<Self> {
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        let user_agent = format!(
            "{}/{} SGM-Helper",
            env!("CARGO_PKG_NAME"),
            env!("CARGO_PKG_VERSION")
        );
        headers.insert(
            USER_AGENT,
            HeaderValue::from_str(&user_agent).context("ongeldige User-Agent header")?,
        );

        if let Some(value) = token.as_ref() {
            let auth = format!("Bearer {}", value);
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&auth).context("ongeldige Authorization header")?,
            );
        }

        let client = Client::builder()
            .default_headers(headers)
            .timeout(Duration::from_secs(30))
            .build()
            .context("kan HTTP client niet bouwen")?;

        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            route_prefix: normalize_prefix(&route_prefix),
            token,
            client,
        })
    }

    pub fn with_token(&self, token: Option<String>) -> Result<Self> {
        Self::new(self.base_url.clone(), self.route_prefix.clone(), token)
    }

    pub fn token_app_password(&self, email: &str, app_password: &str) -> Result<TokenResponse> {
        let url = self.url("/auth/token/app-password");
        let response = self
            .client
            .post(url)
            .header("X-CSRF-Protection", "1")
            .json(&AppPasswordRequest {
                email,
                app_password,
            })
            .send()
            .context("request naar /auth/token/app-password faalde")?;

        parse_json_response(response)
    }

    pub fn auto_enroll_status(&self) -> Result<AutoEnrollStatusResponse> {
        let url = self.url("/auth/app-passwords/auto-enroll");
        let response = self
            .client
            .get(url)
            .send()
            .context("request naar /auth/app-passwords/auto-enroll faalde")?;
        parse_json_response(response)
    }

    pub fn token_app_password_auto_provision(
        &self,
        payload: &AutoProvisionRequest,
    ) -> Result<TokenResponse> {
        let url = self.url("/auth/token/app-password");
        let response = self
            .client
            .post(url)
            .header("X-CSRF-Protection", "1")
            .json(payload)
            .send()
            .context("request naar /auth/token/app-password (auto-provision) faalde")?;

        let payload: AutoProvisionAppPasswordResponse = parse_json_response(response)?;
        let token = payload
            .token
            .or(payload.plain_text_key)
            .or(payload.plain_text_token)
            .filter(|value| !value.trim().is_empty())
            .context("/auth/token/app-password response bevat geen token/plainTextKey")?;

        Ok(TokenResponse { token })
    }

    pub fn login_password(
        &self,
        email: &str,
        password: &str,
        device_type: &str,
        fingerprint: &str,
    ) -> Result<LoginResponse> {
        let url = self.url("/auth/login");
        let response = self
            .client
            .post(url)
            .header("X-CSRF-Protection", "1")
            .json(&LoginRequest {
                email,
                password,
                device_type,
                fingerprint,
            })
            .send()
            .context("request naar /auth/login faalde")?;
        parse_json_response(response)
    }

    pub fn mint_token(&self) -> Result<TokenResponse> {
        let url = self.url("/auth/token");
        let response = self
            .client
            .post(url)
            .header("X-CSRF-Protection", "1")
            .json(&serde_json::json!({}))
            .send()
            .context("request naar /auth/token faalde")?;
        parse_json_response(response)
    }

    pub fn signup(
        &self,
        email: &str,
        display_name: &str,
        password: &str,
        skip_verification: bool,
    ) -> Result<serde_json::Value> {
        let url = self.url("/auth/signup");
        let response = self
            .client
            .post(url)
            .header("X-CSRF-Protection", "1")
            .json(&SignupRequest {
                email,
                display_name,
                password,
                skip_verification,
            })
            .send()
            .context("request naar /auth/signup faalde")?;
        parse_json_response(response)
    }

    pub fn resend_verification(&self, email: &str) -> Result<serde_json::Value> {
        let url = self.url("/auth/resend-verification");
        let response = self
            .client
            .post(url)
            .header("X-CSRF-Protection", "1")
            .json(&ResendVerificationRequest { email })
            .send()
            .context("request naar /auth/resend-verification faalde")?;
        parse_json_response(response)
    }

    pub fn auth_me(&self) -> Result<AuthUser> {
        let url = self.url("/auth/me");
        let response = self
            .client
            .get(url)
            .send()
            .context("request naar /auth/me faalde")?;
        let payload: AuthMeResponse = parse_json_response(response)?;
        payload.user.context("/auth/me response bevat geen user")
    }

    pub fn lookup_rom(&self, filename_stem: &str) -> Result<LookupRomResponse> {
        let url = self.url("/rom/lookup");
        let response = self
            .client
            .get(url)
            .query(&[("filenameStem", filename_stem)])
            .send()
            .context("request naar /rom/lookup faalde")?;
        parse_json_response(response)
    }

    pub fn latest_save(&self, params: LatestSaveRequest<'_>) -> Result<LatestSaveResponse> {
        let url = self.url("/save/latest");
        let mut request = self.client.get(url).query(&[
            ("romSha1", params.rom_sha1),
            ("slotName", params.slot_name),
            ("device_type", params.device_type),
            ("fingerprint", params.fingerprint),
        ]);
        if let Some(runtime_target) = params.runtime_target {
            request = runtime_target.apply_query(request);
        }
        if let Some(context) = params.context {
            if !context.filename.trim().is_empty() {
                request = request.query(&[("filename", context.filename.trim())]);
            }
            if !context.system_slug.trim().is_empty() {
                request = request.query(&[("system", context.system_slug.trim())]);
            }
            if !context.display_title.trim().is_empty() {
                request = request.query(&[("displayTitle", context.display_title.trim())]);
            }
            if let Some(region_code) = context.region_code.as_deref()
                && !region_code.trim().is_empty()
            {
                request = request.query(&[("regionCode", region_code.trim())]);
            }
        }
        if let Some(app_password) = params.app_password
            && !app_password.trim().is_empty()
        {
            request = request.header("X-RSM-App-Password", app_password.trim().to_string());
        }
        let response = request.send().context("request naar /save/latest faalde")?;
        parse_json_response(response)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn upload_save(
        &self,
        filename: &str,
        bytes: Vec<u8>,
        rom_sha1: &str,
        rom_md5: Option<&str>,
        slot_name: &str,
        device_type: &str,
        fingerprint: &str,
        app_password: Option<&str>,
        system_slug: Option<&str>,
        wii_title_id: Option<&str>,
        runtime_target: Option<&RuntimeTarget>,
        port_metadata: Option<&PortUploadMetadata>,
    ) -> Result<UploadSaveResponse> {
        let url = self.url("/saves");
        let part = reqwest::blocking::multipart::Part::bytes(bytes).file_name(filename.to_string());

        let mut form = reqwest::blocking::multipart::Form::new()
            .part("file", part)
            .text("rom_sha1", rom_sha1.to_string())
            .text("slotName", slot_name.to_string())
            .text("device_type", device_type.to_string())
            .text("fingerprint", fingerprint.to_string());

        if let Some(md5) = rom_md5
            && !md5.trim().is_empty()
        {
            form = form.text("rom_md5", md5.to_string());
        }
        if let Some(app_password) = app_password
            && !app_password.trim().is_empty()
        {
            form = form.text("app_password", app_password.to_string());
        }
        if let Some(system_slug) = system_slug
            && !system_slug.trim().is_empty()
        {
            form = form.text("system", system_slug.to_string());
        }
        if let Some(wii_title_id) = wii_title_id
            && !wii_title_id.trim().is_empty()
        {
            form = form.text("wiiTitleId", wii_title_id.trim().to_ascii_uppercase());
        }
        if let Some(runtime_target) = runtime_target {
            form = runtime_target.apply_multipart_form(form);
        }
        if let Some(port) = port_metadata {
            form = form
                .text("portId", port.port_id.clone())
                .text("portName", port.port_name.clone())
                .text("originSystemSlug", port.origin_system_slug.clone())
                .text("portSaveKind", port.port_save_kind.clone())
                .text("relativePath", port.relative_path.clone())
                .text("rootRelativePath", port.root_relative_path.clone())
                .text("slotId", port.slot_id.clone())
                .text("displayTitle", port.display_title.clone());
        }

        let response = self
            .client
            .post(url)
            .header("X-CSRF-Protection", "1")
            .multipart(form)
            .send()
            .context("request naar /saves (multipart) faalde")?;

        parse_json_response(response)
    }

    pub fn list_saves(
        &self,
        limit: usize,
        offset: usize,
        app_password: Option<&str>,
    ) -> Result<ListSavesResponse> {
        let url = self.url("/saves");
        let mut request = self
            .client
            .get(url)
            .query(&[("limit", limit.to_string()), ("offset", offset.to_string())]);
        if let Some(app_password) = app_password
            && !app_password.trim().is_empty()
        {
            request = request.header("X-RSM-App-Password", app_password.trim().to_string());
        }
        let response = request.send().context("request naar /saves faalde")?;
        parse_json_response(response)
    }

    pub fn start_device_auth(&self) -> Result<DeviceAuthResponse> {
        let url = self.url("/auth/device");
        let response = self
            .client
            .post(url)
            .header("X-CSRF-Protection", "1")
            .send()
            .context("request naar /auth/device faalde")?;
        parse_json_response(response)
    }

    pub fn poll_device_token(&self, device_code: &str) -> Result<DeviceTokenPoll> {
        let url = self.url("/auth/device/token");
        let response = self
            .client
            .post(url)
            .header("X-CSRF-Protection", "1")
            .json(&DeviceTokenRequest { device_code })
            .send()
            .context("request naar /auth/device/token faalde")?;

        if response.status() == reqwest::StatusCode::ACCEPTED {
            return Ok(DeviceTokenPoll::Pending);
        }
        if response.status() == reqwest::StatusCode::OK {
            return Ok(DeviceTokenPoll::Success(
                response
                    .json::<TokenResponse>()
                    .context("kan device token response niet deserializen")?,
            ));
        }

        let status = response.status();
        let body = response.text().unwrap_or_else(|_| String::new());
        bail!(
            "device-token polling faalde: status={} body={}",
            status,
            truncate(&body, 300)
        );
    }

    pub fn download_save(
        &self,
        save_id: &str,
        device_type: &str,
        fingerprint: &str,
        app_password: Option<&str>,
        runtime_target: Option<&RuntimeTarget>,
    ) -> Result<Vec<u8>> {
        let url = self.url("/saves/download");
        let mut request = self.client.get(url).query(&[
            ("id", save_id),
            ("device_type", device_type),
            ("fingerprint", fingerprint),
        ]);
        if let Some(runtime_target) = runtime_target {
            request = runtime_target.apply_query(request);
        }
        if let Some(app_password) = app_password
            && !app_password.trim().is_empty()
        {
            request = request.header("X-RSM-App-Password", app_password.trim().to_string());
        }
        let mut response = request
            .send()
            .context("request naar /saves/download faalde")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().unwrap_or_else(|_| String::new());
            bail!(
                "download faalde: status={} body={}",
                status,
                truncate(&body, 300)
            );
        }

        let mut body = Vec::new();
        response
            .read_to_end(&mut body)
            .context("kan download response niet lezen")?;
        Ok(body)
    }

    pub fn conflict_check(
        &self,
        rom_sha1: &str,
        slot_name: &str,
        device_type: &str,
        fingerprint: &str,
        app_password: Option<&str>,
        runtime_target: Option<&RuntimeTarget>,
    ) -> Result<ConflictCheckResponse> {
        let url = self.url("/conflicts/check");
        let mut request = self.client.get(url).query(&[
            ("romSha1", rom_sha1),
            ("slotName", slot_name),
            ("device_type", device_type),
            ("fingerprint", fingerprint),
        ]);
        if let Some(runtime_target) = runtime_target {
            request = runtime_target.apply_query(request);
        }
        if let Some(app_password) = app_password
            && !app_password.trim().is_empty()
        {
            request = request.header("X-RSM-App-Password", app_password.trim().to_string());
        }
        let response = request
            .send()
            .context("request naar /conflicts/check faalde")?;
        parse_json_response(response)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn conflict_report(
        &self,
        file_name: &str,
        bytes: Vec<u8>,
        rom_sha1: &str,
        slot_name: &str,
        local_sha256: &str,
        cloud_sha256: &str,
        device_name: &str,
        device_type: &str,
        fingerprint: &str,
        app_password: Option<&str>,
        runtime_target: Option<&RuntimeTarget>,
    ) -> Result<ConflictReportResponse> {
        let url = self.url("/conflicts/report");
        let part =
            reqwest::blocking::multipart::Part::bytes(bytes).file_name(file_name.to_string());

        let mut form = reqwest::blocking::multipart::Form::new()
            .part("file", part)
            .text("romSha1", rom_sha1.to_string())
            .text("slotName", slot_name.to_string())
            .text("localSha256", local_sha256.to_string())
            .text("cloudSha256", cloud_sha256.to_string())
            .text("deviceName", device_name.to_string())
            .text("device_type", device_type.to_string())
            .text("fingerprint", fingerprint.to_string());
        if let Some(app_password) = app_password
            && !app_password.trim().is_empty()
        {
            form = form.text("app_password", app_password.to_string());
        }
        if let Some(runtime_target) = runtime_target {
            form = runtime_target.apply_multipart_form(form);
        }

        let response = self
            .client
            .post(url)
            .header("X-CSRF-Protection", "1")
            .multipart(form)
            .send()
            .context("request naar /conflicts/report faalde")?;

        parse_json_response(response)
    }

    pub fn sync_helper_config(
        &self,
        payload: &serde_json::Value,
        app_password: Option<&str>,
    ) -> Result<serde_json::Value> {
        let url = self.url("/helpers/config/sync");
        let mut request = self
            .client
            .post(url)
            .header("X-CSRF-Protection", "1")
            .json(payload);
        if let Some(app_password) = app_password
            && !app_password.trim().is_empty()
        {
            request = request.header("X-RSM-App-Password", app_password.trim().to_string());
        }
        let response = request
            .send()
            .context("request naar /helpers/config/sync faalde")?;

        if response.status() == StatusCode::NO_CONTENT {
            return Ok(serde_json::json!({ "accepted": true }));
        }

        parse_json_response(response)
    }

    pub fn helper_heartbeat(
        &self,
        payload: &serde_json::Value,
        app_password: Option<&str>,
    ) -> Result<serde_json::Value> {
        let url = self.url("/helpers/heartbeat");
        let mut request = self
            .client
            .post(url)
            .header("X-CSRF-Protection", "1")
            .json(payload);
        if let Some(app_password) = app_password
            && !app_password.trim().is_empty()
        {
            request = request.header("X-RSM-App-Password", app_password.trim().to_string());
        }
        let response = request
            .send()
            .context("request naar /helpers/heartbeat faalde")?;

        if response.status() == StatusCode::NO_CONTENT {
            return Ok(serde_json::json!({ "accepted": true }));
        }
        if matches!(
            response.status(),
            StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED | StatusCode::NOT_IMPLEMENTED
        ) {
            return Ok(serde_json::json!({
                "accepted": false,
                "unsupported": true
            }));
        }

        parse_json_response(response)
    }

    pub fn open_events(&self) -> Result<Response> {
        let url = self.url("/events");
        let response = self
            .client
            .get(url)
            .header(ACCEPT, "text/event-stream")
            .send()
            .context("request naar /events faalde")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().unwrap_or_else(|_| String::new());
            bail!(
                "SSE connectie mislukt: status={} body={}",
                status,
                truncate(&body, 300)
            );
        }

        Ok(response)
    }

    pub fn has_token(&self) -> bool {
        self.token
            .as_ref()
            .map(|value| !value.is_empty())
            .unwrap_or(false)
    }

    fn url(&self, path: &str) -> String {
        let normalized_path = if path.starts_with('/') {
            path.to_string()
        } else {
            format!("/{}", path)
        };
        format!("{}{}{}", self.base_url, self.route_prefix, normalized_path)
    }
}

fn parse_json_response<T>(response: Response) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().unwrap_or_else(|_| String::new());
        bail!(
            "HTTP request faalde: status={} body={}",
            status,
            truncate(&body, 300)
        );
    }

    response
        .json::<T>()
        .context("kan JSON response niet deserializen")
}

fn push_runtime_pair(pairs: &mut Vec<(String, String)>, key: &str, value: &str) {
    if key.trim().is_empty() || value.trim().is_empty() {
        return;
    }
    if pairs.iter().any(|(k, v)| k == key && v == value) {
        return;
    }
    pairs.push((key.to_string(), value.to_string()));
}

fn camel_case_to_snake_case(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 4);
    for (index, ch) in value.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if index > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else if ch == '-' || ch == ' ' {
            out.push('_');
        } else {
            out.push(ch.to_ascii_lowercase());
        }
    }
    out
}

fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    value.chars().take(max).collect::<String>() + "..."
}

fn normalize_prefix(prefix: &str) -> String {
    let mut value = prefix.trim().trim_end_matches('/').to_string();
    if value == "/" || value.is_empty() {
        return String::new();
    }
    if !value.starts_with('/') {
        value.insert(0, '/');
    }
    value
}
