use std::fs;
use std::path::Path;

use assert_cmd::Command;
use httpmock::Method::{GET, POST};
use httpmock::MockServer;
use tempfile::TempDir;

fn write_config(
    tmp: &TempDir,
    server: &MockServer,
    root: &Path,
    state_dir: &Path,
) -> std::path::PathBuf {
    let config_path = tmp.path().join("config.ini");
    let body = format!(
        "URL=\"127.0.0.1\"\nPORT=\"{}\"\nROOT=\"{}\"\nSTATE_DIR=\"{}\"\nWATCH=\"true\"\nWATCH_INTERVAL=\"1\"\n",
        server.port(),
        root.display(),
        state_dir.display()
    );
    fs::write(&config_path, body).unwrap();
    config_path
}

#[test]
fn watch_smoke_persists_state_and_exits_with_max_cycles() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_watch","expiresInDays":7}"#);
    });
    server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"watch@example.com"}}"#);
    });

    server.mock(|when, then| {
        when.method(GET).path("/rom/lookup");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"count":1,"rom":{"sha1":"watch-sha","md5":"watch-md5"}}"#);
    });

    let latest = server.mock(|when, then| {
        when.method(GET).path("/save/latest");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"exists":false,"sha256":null,"version":null,"id":null}"#);
    });

    let uploads = server.mock(|when, then| {
        when.method(POST).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"save":{"id":"save-watch","sha256":"watch-sha-local"}}"#);
    });

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    fs::create_dir_all(root.join("Nintendo")).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    fs::write(root.join("Nintendo/metroid.sav"), vec![0x00u8; 32768]).unwrap();

    let config = write_config(&tmp, &server, &root, &state_dir);

    Command::cargo_bin("sgm-steamdeck-helper")
        .unwrap()
        .arg("--config")
        .arg(&config)
        .arg("login")
        .arg("--email")
        .arg("watch@example.com")
        .arg("--app-password")
        .arg("pw")
        .assert()
        .success();

    Command::cargo_bin("sgm-steamdeck-helper")
        .unwrap()
        .arg("--config")
        .arg(&config)
        .arg("watch")
        .arg("--watch-interval")
        .arg("1")
        .arg("--max-cycles")
        .arg("2")
        .assert()
        .success();

    let sync_state_path = state_dir.join("sync_state.json");
    assert!(sync_state_path.exists());
    assert!(latest.calls() >= 1);
    assert!(uploads.calls() >= 1);
}

#[test]
fn login_fails_when_backend_is_unreachable() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&state_dir).unwrap();

    let config_path = tmp.path().join("config.ini");
    fs::write(
        &config_path,
        format!(
            "URL=\"127.0.0.1\"\nPORT=\"65531\"\nROOT=\"{}\"\nSTATE_DIR=\"{}\"\n",
            root.display(),
            state_dir.display()
        ),
    )
    .unwrap();

    Command::cargo_bin("sgm-steamdeck-helper")
        .unwrap()
        .arg("--config")
        .arg(config_path)
        .arg("login")
        .arg("--email")
        .arg("fail@example.com")
        .arg("--app-password")
        .arg("pw")
        .assert()
        .failure();
}
