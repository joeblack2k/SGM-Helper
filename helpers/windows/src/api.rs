use std::io::Read;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::blocking::{Client, Response};
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue};
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
pub struct LatestSaveResponse {
    pub exists: bool,
    pub sha256: Option<String>,
    pub version: Option<i64>,
    pub id: Option<String>,
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
pub struct UploadedSave {
    pub id: Option<String>,
    pub sha256: Option<String>,
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

    pub fn latest_save(
        &self,
        rom_sha1: &str,
        slot_name: &str,
        device_type: &str,
        fingerprint: &str,
        app_password: Option<&str>,
    ) -> Result<LatestSaveResponse> {
        let url = self.url("/save/latest");
        let mut request = self.client.get(url).query(&[
            ("romSha1", rom_sha1),
            ("slotName", slot_name),
            ("device_type", device_type),
            ("fingerprint", fingerprint),
        ]);
        if let Some(app_password) = app_password
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

        let response = self
            .client
            .post(url)
            .header("X-CSRF-Protection", "1")
            .multipart(form)
            .send()
            .context("request naar /saves (multipart) faalde")?;

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
    ) -> Result<Vec<u8>> {
        let url = self.url("/saves/download");
        let mut request = self.client.get(url).query(&[
            ("id", save_id),
            ("device_type", device_type),
            ("fingerprint", fingerprint),
        ]);
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
    ) -> Result<ConflictCheckResponse> {
        let url = self.url("/conflicts/check");
        let mut request = self.client.get(url).query(&[
            ("romSha1", rom_sha1),
            ("slotName", slot_name),
            ("device_type", device_type),
            ("fingerprint", fingerprint),
        ]);
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

        let response = self
            .client
            .post(url)
            .header("X-CSRF-Protection", "1")
            .multipart(form)
            .send()
            .context("request naar /conflicts/report faalde")?;

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
