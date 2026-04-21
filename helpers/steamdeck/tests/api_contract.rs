use std::fs;
use std::path::Path;

use assert_cmd::Command;
use httpmock::Method::{GET, POST};
use httpmock::MockServer;
use serde_json::Value;
use tempfile::TempDir;

fn write_config(
    tmp: &TempDir,
    server: &MockServer,
    root: &Path,
    state_dir: &Path,
) -> std::path::PathBuf {
    let config_path = tmp.path().join("config.ini");
    let body = format!(
        "URL=\"127.0.0.1\"\nPORT=\"{}\"\nROOT=\"{}\"\nSTATE_DIR=\"{}\"\nWATCH=\"false\"\nWATCH_INTERVAL=\"1\"\n",
        server.port(),
        root.display(),
        state_dir.display()
    );
    fs::write(&config_path, body).unwrap();
    config_path
}

#[test]
fn login_with_app_password_persists_token() {
    let server = MockServer::start();
    let _token = server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_test","expiresInDays":7}"#);
    });
    let _me = server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"mister@example.com"}}"#);
    });

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    let config = write_config(&tmp, &server, &root, &state_dir);

    Command::cargo_bin("sgm-steamdeck-helper")
        .unwrap()
        .arg("--config")
        .arg(config)
        .arg("login")
        .arg("--email")
        .arg("mister@example.com")
        .arg("--app-password")
        .arg("secret")
        .assert()
        .success();

    let auth_path = state_dir.join("auth.json");
    assert!(auth_path.exists());
    let auth: Value = serde_json::from_str(&fs::read_to_string(auth_path).unwrap()).unwrap();
    assert_eq!(auth["token"], "tok_test");
}

#[test]
fn login_with_password_uses_login_and_token_endpoints() {
    let server = MockServer::start();
    let login = server.mock(|when, then| {
        when.method(POST).path("/auth/login");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"password@example.com"}}"#);
    });
    let token = server.mock(|when, then| {
        when.method(POST).path("/auth/token");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_password","expiresInDays":7}"#);
    });
    let me = server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"password@example.com"}}"#);
    });

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    let config = write_config(&tmp, &server, &root, &state_dir);

    Command::cargo_bin("sgm-steamdeck-helper")
        .unwrap()
        .arg("--config")
        .arg(config)
        .arg("login")
        .arg("--email")
        .arg("password@example.com")
        .arg("--password")
        .arg("secret")
        .assert()
        .success();

    assert_eq!(login.calls(), 1);
    assert_eq!(token.calls(), 1);
    assert_eq!(me.calls(), 1);

    let auth_path = state_dir.join("auth.json");
    assert!(auth_path.exists());
    let auth: Value = serde_json::from_str(&fs::read_to_string(auth_path).unwrap()).unwrap();
    assert_eq!(auth["token"], "tok_password");
}

#[test]
fn sync_uploads_when_no_cloud_save_exists() {
    let server = MockServer::start();
    let _token = server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_sync","expiresInDays":7}"#);
    });
    let _me = server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"sync@example.com"}}"#);
    });
    let rom_lookup = server.mock(|when, then| {
        when.method(GET).path("/rom/lookup");
        then.status(200)
            .header("content-type", "application/json")
            .body(
                r#"{"success":true,"count":1,"rom":{"sha1":"abc123","md5":"md5v","fileName":"wario.sav"}}"#,
            );
    });
    let save_latest = server.mock(|when, then| {
        when.method(GET).path("/save/latest");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"exists":false,"sha256":null,"version":null,"id":null}"#);
    });
    let upload = server.mock(|when, then| {
        when.method(POST).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"save":{"id":"save-1","sha256":"sha-up"}}"#);
    });

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    fs::write(root.join("wario.sav"), b"hello save").unwrap();
    let config = write_config(&tmp, &server, &root, &state_dir);

    Command::cargo_bin("sgm-steamdeck-helper")
        .unwrap()
        .arg("--config")
        .arg(&config)
        .arg("login")
        .arg("--email")
        .arg("sync@example.com")
        .arg("--app-password")
        .arg("pw")
        .assert()
        .success();

    Command::cargo_bin("sgm-steamdeck-helper")
        .unwrap()
        .arg("--config")
        .arg(config)
        .arg("sync")
        .assert()
        .success();

    assert_eq!(rom_lookup.calls(), 1);
    assert_eq!(save_latest.calls(), 1);
    assert_eq!(upload.calls(), 1);
}

#[test]
fn sync_reports_conflict_when_backend_marks_conflict() {
    let server = MockServer::start();
    let _token = server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_conflict","expiresInDays":7}"#);
    });
    let _me = server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"conflict@example.com"}}"#);
    });

    let _rom_lookup = server.mock(|when, then| {
        when.method(GET).path("/rom/lookup");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"count":1,"rom":{"sha1":"rom-sha1","md5":"rom-md5"}}"#);
    });
    let _save_latest = server.mock(|when, then| {
        when.method(GET).path("/save/latest");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"exists":true,"sha256":"cloud-different","version":2,"id":"save-cloud"}"#);
    });
    let _conflict_check = server.mock(|when, then| {
        when.method(GET).path("/conflicts/check");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"exists":true,"conflictId":"conf-1","cloudSha256":"cloud-different","cloudVersion":2,"cloudSaveId":"save-cloud"}"#);
    });
    let conflict_report = server.mock(|when, then| {
        when.method(POST).path("/conflicts/report");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"created":true,"conflictId":"conf-1"}"#);
    });

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    fs::write(root.join("chrono.sav"), b"local-content").unwrap();
    let config = write_config(&tmp, &server, &root, &state_dir);

    Command::cargo_bin("sgm-steamdeck-helper")
        .unwrap()
        .arg("--config")
        .arg(&config)
        .arg("login")
        .arg("--email")
        .arg("conflict@example.com")
        .arg("--app-password")
        .arg("pw")
        .assert()
        .success();

    Command::cargo_bin("sgm-steamdeck-helper")
        .unwrap()
        .arg("--config")
        .arg(config)
        .arg("sync")
        .assert()
        .success();

    assert!(conflict_report.calls() >= 1);
}
