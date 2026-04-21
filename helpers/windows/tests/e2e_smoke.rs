use std::fs;
use std::path::Path;

use assert_cmd::Command;
use httpmock::Method::{GET, POST};
use httpmock::MockServer;
use tempfile::TempDir;

const PS1_MEMCARD_SIZE: usize = 131_072;
const PS1_FRAME_SIZE: usize = 128;

fn set_frame_checksum(bytes: &mut [u8], frame_index: usize) {
    let start = frame_index * PS1_FRAME_SIZE;
    let end = start + PS1_FRAME_SIZE;
    let frame = &mut bytes[start..end];
    let checksum = frame[..PS1_FRAME_SIZE - 1]
        .iter()
        .fold(0u8, |acc, value| acc ^ value);
    frame[PS1_FRAME_SIZE - 1] = checksum;
}

fn build_valid_ps1_memcard() -> Vec<u8> {
    let mut bytes = vec![0u8; PS1_MEMCARD_SIZE];
    bytes[0] = b'M';
    bytes[1] = b'C';

    for frame_index in 1..=15 {
        let start = frame_index * PS1_FRAME_SIZE;
        bytes[start] = 0xA0;
        bytes[start + 8] = 0xFF;
        bytes[start + 9] = 0xFF;
        set_frame_checksum(&mut bytes, frame_index);
    }

    let trailing_start = 63 * PS1_FRAME_SIZE;
    bytes[trailing_start] = b'M';
    bytes[trailing_start + 1] = b'C';
    set_frame_checksum(&mut bytes, 0);
    set_frame_checksum(&mut bytes, 63);
    bytes
}

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

    Command::cargo_bin("sgm-windows-helper")
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

    Command::cargo_bin("sgm-windows-helper")
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

    Command::cargo_bin("sgm-windows-helper")
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

#[test]
fn convert_ps1_raw_to_gme_and_back_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let raw_path = tmp.path().join("card.mcr");
    let gme_path = tmp.path().join("card.gme");
    let roundtrip_raw = tmp.path().join("card-roundtrip.mcr");
    let raw = build_valid_ps1_memcard();
    fs::write(&raw_path, &raw).unwrap();

    Command::cargo_bin("sgm-windows-helper")
        .unwrap()
        .arg("convert")
        .arg("--input")
        .arg(&raw_path)
        .arg("--output")
        .arg(&gme_path)
        .arg("--from")
        .arg("raw")
        .arg("--to")
        .arg("gme")
        .assert()
        .success();

    Command::cargo_bin("sgm-windows-helper")
        .unwrap()
        .arg("convert")
        .arg("--input")
        .arg(&gme_path)
        .arg("--output")
        .arg(&roundtrip_raw)
        .arg("--to")
        .arg("raw")
        .assert()
        .success();

    let output = fs::read(&roundtrip_raw).unwrap();
    assert_eq!(output, raw);
}

#[test]
fn sync_restores_missing_file_from_cloud_using_saved_adapter_metadata() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_restore","expiresInDays":7}"#);
    });
    server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"restore@example.com"}}"#);
    });
    server.mock(|when, then| {
        when.method(GET).path("/save/latest");
        then.status(200).header("content-type", "application/json").body(
            r#"{"success":true,"exists":true,"sha256":"remote-sha","version":2,"id":"save-remote"}"#,
        );
    });

    let restored_bytes = vec![0x55u8; 32768];
    let download_mock = server.mock(|when, then| {
        when.method(GET).path("/saves/download");
        then.status(200).body(restored_bytes.clone());
    });

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    fs::create_dir_all(root.join("Nintendo")).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    let missing_save_path = root.join("Nintendo/metroid.sav");
    assert!(!missing_save_path.exists());

    let sync_state = format!(
        r#"{{
  "entries": {{
    "{}": {{
      "sha256": "old-sha",
      "rom_sha1": "restore-rom-sha",
      "version": 1,
      "system_slug": "snes",
      "local_container": "native",
      "adapter_profile": "identity",
      "source_kind": "windows",
      "source_name": "default-windows",
      "updated_at": "2026-01-01T00:00:00Z"
    }}
  }}
}}"#,
        missing_save_path.display()
    );
    fs::write(state_dir.join("sync_state.json"), sync_state).unwrap();

    let config = write_config(&tmp, &server, &root, &state_dir);

    Command::cargo_bin("sgm-windows-helper")
        .unwrap()
        .arg("--config")
        .arg(&config)
        .arg("login")
        .arg("--email")
        .arg("restore@example.com")
        .arg("--app-password")
        .arg("pw")
        .assert()
        .success();

    Command::cargo_bin("sgm-windows-helper")
        .unwrap()
        .arg("--config")
        .arg(&config)
        .arg("sync")
        .assert()
        .success();

    assert!(missing_save_path.exists());
    assert_eq!(fs::read(&missing_save_path).unwrap(), restored_bytes);
    assert!(download_mock.calls() >= 1);
}
