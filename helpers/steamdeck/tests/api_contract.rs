use std::fs;
use std::path::Path;

use assert_cmd::Command;
use httpmock::Method::{GET, POST};
use httpmock::MockServer;
use serde_json::Value;
use tempfile::TempDir;

const PS1_MEMCARD_SIZE: usize = 131_072;
const PS1_FRAME_SIZE: usize = 128;
const PS1_DEXDRIVE_HEADER_LENGTH: usize = 3904;
const PS1_DEXDRIVE_MAGIC: &[u8] = b"123-456-STD";

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

fn encode_ps1_dexdrive(raw: &[u8]) -> Vec<u8> {
    let mut header = vec![0u8; PS1_DEXDRIVE_HEADER_LENGTH];
    header[..PS1_DEXDRIVE_MAGIC.len()].copy_from_slice(PS1_DEXDRIVE_MAGIC);
    header[18] = 0x01;
    header[20] = 0x01;
    header[21] = b'M';
    [header, raw.to_vec()].concat()
}

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
    fs::create_dir_all(root.join("Nintendo/Game Boy")).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    fs::write(
        root.join("Nintendo/Game Boy/wario.sav"),
        vec![0x00u8; 32768],
    )
    .unwrap();
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
    fs::create_dir_all(root.join("Nintendo/Super Nintendo")).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    fs::write(
        root.join("Nintendo/Super Nintendo/chrono.sav"),
        vec![0x00u8; 32768],
    )
    .unwrap();
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

#[test]
fn sync_accepts_ps1_gme_and_uploads_normalized_payload() {
    let expected_line = "ps-line:psx:retroarch:memory-card-1";
    let server = MockServer::start();
    let _token = server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_ps1","expiresInDays":7}"#);
    });
    let _me = server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"ps1@example.com"}}"#);
    });
    let rom_lookup = server.mock(|when, then| {
        when.method(GET).path("/rom/lookup");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"count":1,"rom":{"sha1":"ps1sha","md5":"ps1md5"}}"#);
    });
    let save_latest = server.mock(|when, then| {
        when.method(GET)
            .path("/save/latest")
            .query_param("romSha1", expected_line)
            .query_param("slotName", "Memory Card 1");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"exists":false,"sha256":null,"version":null,"id":null}"#);
    });
    let upload = server.mock(|when, then| {
        when.method(POST)
            .path("/saves")
            .body_includes("name=\"rom_sha1\"")
            .body_includes(expected_line)
            .body_includes("name=\"slotName\"")
            .body_includes("Memory Card 1")
            .body_includes("name=\"device_type\"")
            .body_includes("retroarch");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"save":{"id":"save-ps1","sha256":"sha-up"}}"#);
    });

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    fs::create_dir_all(root.join("Sony/PSX")).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    let raw = build_valid_ps1_memcard();
    fs::write(root.join("Sony/PSX/ff7.gme"), encode_ps1_dexdrive(&raw)).unwrap();
    let config = write_config(&tmp, &server, &root, &state_dir);

    Command::cargo_bin("sgm-steamdeck-helper")
        .unwrap()
        .arg("--config")
        .arg(&config)
        .arg("login")
        .arg("--email")
        .arg("ps1@example.com")
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

    assert_eq!(rom_lookup.calls(), 0);
    assert_eq!(save_latest.calls(), 1);
    assert_eq!(upload.calls(), 1);
}

#[test]
fn sync_auto_enrolls_when_gate_is_open_without_login() {
    let server = MockServer::start();
    let auto_status = server.mock(|when, then| {
        when.method(GET).path("/auth/app-passwords/auto-enroll");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"active":true}"#);
    });
    let auto_token = server.mock(|when, then| {
        when.method(POST)
            .path("/auth/token/app-password")
            .body_includes("\"deviceType\":\"steamdeck\"")
            .body_includes("\"helperName\":\"sgm-steamdeck-helper\"");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"RSM-AAAA-BBBB","plainTextKey":"RSM-AAAA-BBBB"}"#);
    });
    let rom_lookup = server.mock(|when, then| {
        when.method(GET).path("/rom/lookup");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"count":1,"rom":{"sha1":"abc123","md5":"md5v"}}"#);
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
    fs::create_dir_all(root.join("Nintendo/Game Boy")).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    fs::write(
        root.join("Nintendo/Game Boy/wario.sav"),
        vec![0x00u8; 32768],
    )
    .unwrap();
    let config = write_config(&tmp, &server, &root, &state_dir);

    Command::cargo_bin("sgm-steamdeck-helper")
        .unwrap()
        .arg("--config")
        .arg(config)
        .arg("sync")
        .assert()
        .success();

    assert_eq!(auto_status.calls(), 1);
    assert_eq!(auto_token.calls(), 1);
    assert_eq!(rom_lookup.calls(), 1);
    assert_eq!(save_latest.calls(), 1);
    assert_eq!(upload.calls(), 1);

    let auth_path = state_dir.join("auth.json");
    assert!(auth_path.exists());
    let auth: Value = serde_json::from_str(&fs::read_to_string(auth_path).unwrap()).unwrap();
    assert_eq!(auth["token"], "RSM-AAAA-BBBB");
}

/// Genesis Plus GX's shared Sega CD backup RAM: 8 KiB, a directory of saves at
/// the end, closed by the format's trailer (`SEGA_CD_ROM`, `RAM_CARTRIDGE`).
fn sega_cd_internal_bram() -> Vec<u8> {
    let mut bytes = vec![0u8; 8192];
    // One saved game's data and directory entry, so the image is not blank.
    for (index, value) in bytes[0x40..0x240].iter_mut().enumerate() {
        *value = (index as u8).wrapping_mul(29) | 0x01;
    }
    bytes[0x1F80..0x1F8B].copy_from_slice(b"POPFUL_MAIL");
    bytes[0x1FC0..0x1FCB].copy_from_slice(b"___________");
    bytes[0x1FCF] = 0x40;
    bytes[0x1FE0..0x1FEB].copy_from_slice(b"SEGA_CD_ROM");
    bytes[0x1FEC] = 0x01;
    bytes[0x1FF0..0x2000].copy_from_slice(b"RAM_CARTRIDGE___");
    bytes
}

// Genesis Plus GX keeps every Sega CD game's save in one backup RAM per device
// (segacd/scd_U.brm), not a file per game, so there is no ROM to name it by.
// It syncs as one device-level save under a line key, as a PS1 card does.
#[test]
fn sync_uploads_a_shared_sega_cd_backup_ram_under_its_line_key() {
    let expected_line = "segacd-bram:retroarch:scd_u";
    let server = MockServer::start();
    let _token = server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_scd","expiresInDays":7}"#);
    });
    let _me = server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"scd@example.com"}}"#);
    });
    let _latest = server.mock(|when, then| {
        when.method(GET)
            .path("/save/latest")
            .query_param("romSha1", expected_line);
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"exists":false,"sha256":null,"version":null,"id":null}"#);
    });
    let upload = server.mock(|when, then| {
        when.method(POST)
            .path("/saves")
            .body_includes("name=\"rom_sha1\"")
            .body_includes(expected_line);
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"save":{"id":"save-scd","sha256":"sha-scd"}}"#);
    });

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    fs::create_dir_all(root.join("segacd")).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    fs::write(root.join("segacd/scd_U.brm"), sega_cd_internal_bram()).unwrap();
    let config = tmp.path().join("config.ini");
    fs::write(
        &config,
        format!(
            "URL=\"127.0.0.1\"\nPORT=\"{}\"\nROOT=\"{}\"\nSTATE_DIR=\"{}\"\nWATCH=\"false\"\nWATCH_INTERVAL=\"1\"\n\n[source.retroarch]\nLABEL=\"RetroArch\"\nKIND=\"retroarch\"\nPROFILE=\"retroarch\"\nSAVE_PATH=\"{}\"\nROM_PATH=\"{}\"\nRECURSIVE=\"true\"\nSYSTEMS=\"sega-cd\"\n",
            server.port(),
            root.display(),
            state_dir.display(),
            root.display(),
            tmp.path().join("roms").display()
        ),
    )
    .unwrap();

    Command::cargo_bin("sgm-steamdeck-helper")
        .unwrap()
        .args(["--config"])
        .arg(&config)
        .args([
            "login",
            "--email",
            "scd@example.com",
            "--app-password",
            "pw",
        ])
        .assert()
        .success();
    let output = Command::cargo_bin("sgm-steamdeck-helper")
        .unwrap()
        .arg("--config")
        .arg(&config)
        .args(["sync", "--verbose"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(
        upload.calls(),
        1,
        "the shared backup RAM did not upload under its line key:\n{stderr}"
    );
}

// A blank, freshly formatted backup RAM holds no save, and must not replace a
// real one on the server.
#[test]
fn a_blank_sega_cd_backup_ram_is_not_uploaded() {
    let server = MockServer::start();
    let _token = server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_scd","expiresInDays":7}"#);
    });
    let _me = server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"scd@example.com"}}"#);
    });
    let _list = server.mock(|when, then| {
        when.method(GET).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"total":0,"limit":100,"offset":0,"saves":[]}"#);
    });
    let upload = server.mock(|when, then| {
        when.method(POST).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"save":{"id":"save-scd","sha256":"sha-scd"}}"#);
    });

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    fs::create_dir_all(root.join("segacd")).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    let mut blank = vec![0u8; 8192];
    blank[0x1FC0..0x1FCB].copy_from_slice(b"___________");
    blank[0x1FE0..0x1FEB].copy_from_slice(b"SEGA_CD_ROM");
    blank[0x1FF0..0x2000].copy_from_slice(b"RAM_CARTRIDGE___");
    fs::write(root.join("segacd/scd_U.brm"), blank).unwrap();
    let config = write_config(&tmp, &server, &root, &state_dir);
    Command::cargo_bin("sgm-steamdeck-helper")
        .unwrap()
        .arg("--config")
        .arg(&config)
        .args([
            "login",
            "--email",
            "scd@example.com",
            "--app-password",
            "pw",
        ])
        .assert()
        .success();
    Command::cargo_bin("sgm-steamdeck-helper")
        .unwrap()
        .arg("--config")
        .arg(&config)
        .arg("sync")
        .output()
        .unwrap();

    assert_eq!(upload.calls(), 0, "a blank backup RAM was uploaded");
}

// The restore half: another device's backup RAM lands in segacd/<same file>,
// where Genesis Plus GX reads it, and the bytes are kept whole.
#[test]
fn a_shared_sega_cd_backup_ram_restores_to_its_own_file() {
    let server = MockServer::start();
    let _token = server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_scd","expiresInDays":7}"#);
    });
    let _me = server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"scd@example.com"}}"#);
    });
    server.mock(|when, then| {
        when.method(GET).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"total":1,"limit":100,"offset":0,"saves":[{"id":"save-scd","filename":"scd_U.brm","displayTitle":"scd_U","systemSlug":"sega-cd","romSha1":"segacd-bram:retroarch:scd_u","sha256":"sha-scd","version":2,"latestSizeBytes":8192}]}"#);
    });
    server.mock(|when, then| {
        when.method(GET).path("/saves/download");
        then.status(200).body(sega_cd_internal_bram());
    });

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    fs::create_dir_all(root.join("segacd")).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    let config = tmp.path().join("config.ini");
    fs::write(
        &config,
        format!(
            "URL=\"127.0.0.1\"\nPORT=\"{}\"\nROOT=\"{}\"\nSTATE_DIR=\"{}\"\nWATCH=\"false\"\nWATCH_INTERVAL=\"1\"\n\n[source.retroarch]\nLABEL=\"RetroArch\"\nKIND=\"retroarch\"\nPROFILE=\"retroarch\"\nSAVE_PATH=\"{}\"\nROM_PATH=\"{}\"\nRECURSIVE=\"true\"\nSYSTEMS=\"sega-cd\"\n",
            server.port(),
            root.display(),
            state_dir.display(),
            root.display(),
            tmp.path().join("roms").display()
        ),
    )
    .unwrap();
    Command::cargo_bin("sgm-steamdeck-helper")
        .unwrap()
        .arg("--config")
        .arg(&config)
        .args([
            "login",
            "--email",
            "scd@example.com",
            "--app-password",
            "pw",
        ])
        .assert()
        .success();
    let output = Command::cargo_bin("sgm-steamdeck-helper")
        .unwrap()
        .arg("--config")
        .arg(&config)
        .args(["sync", "--verbose"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(
        fs::read(root.join("segacd/scd_U.brm")).ok(),
        Some(sega_cd_internal_bram()),
        "the backup RAM did not restore to segacd/scd_U.brm:\n{stderr}"
    );
}

/// A PARAM.SFO holding the SAVEDATA_DIRECTORY key, the way PPSSPP writes it.
fn psp_param_sfo(save_dir: &str) -> Vec<u8> {
    let mut value = save_dir.as_bytes().to_vec();
    value.push(0);
    let key = b"SAVEDATA_DIRECTORY\0\0";
    let key_offset = 20 + 16;
    let data_offset = key_offset + key.len();
    let mut out = b"\0PSF".to_vec();
    out.extend_from_slice(&0x0101u32.to_le_bytes());
    out.extend_from_slice(&(key_offset as u32).to_le_bytes());
    out.extend_from_slice(&(data_offset as u32).to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0x0204u16.to_le_bytes());
    out.extend_from_slice(&(value.len() as u32).to_le_bytes());
    out.extend_from_slice(&64u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(key);
    value.resize(64, 0);
    out.extend_from_slice(&value);
    out
}

fn write_ppsspp_savedata(saves: &Path, save_dir: &str) {
    let dir = saves.join("PSP/PPSSPP-SA").join(save_dir);
    fs::create_dir_all(&dir).unwrap();
    let mut data = vec![0u8; 286_344];
    for (index, value) in data.iter_mut().enumerate() {
        *value = (index % 251) as u8;
    }
    fs::write(dir.join("DISSIDIA.BIN"), data).unwrap();
    fs::write(dir.join("ICON0.PNG"), b"\x89PNG\r\n\x1a\nicon").unwrap();
    fs::write(dir.join("PARAM.SFO"), psp_param_sfo(save_dir)).unwrap();
}

fn run_ppsspp_sync(bundles_flag: Option<&str>) -> (usize, usize, usize, String) {
    let server = MockServer::start();
    let _token = server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_psp","expiresInDays":7}"#);
    });
    let _me = server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"psp@example.com"}}"#);
    });
    let _rom_lookup = server.mock(|when, then| {
        when.method(GET).path("/rom/lookup");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"count":1,"rom":{"sha1":"psp-rom","md5":"psp-md5"}}"#);
    });
    let _latest = server.mock(|when, then| {
        when.method(GET).path("/save/latest");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"exists":false,"sha256":null,"version":null,"id":null}"#);
    });
    let bundle_upload = server.mock(|when, then| {
        when.method(POST)
            .path("/saves")
            .body_includes("name=\"pspSaveDir\"")
            .body_includes("ULUS10437GameData00")
            .body_includes("filename=\"ULUS10437GameData00.pspsave\"")
            .body_includes("psp-savedir:ULUS10437GAMEDATA00")
            .body_includes("name=\"system\"");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"save":{"id":"save-psp","sha256":"sha-psp"}}"#);
    });
    let loose_upload = server.mock(|when, then| {
        when.method(POST)
            .path("/saves")
            .body_includes("filename=\"DISSIDIA.BIN\"");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"save":{"id":"save-bin","sha256":"sha-bin"}}"#);
    });
    let any_upload = server.mock(|when, then| {
        when.method(POST).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"save":{"id":"save-other","sha256":"sha-other"}}"#);
    });

    let tmp = tempfile::tempdir().unwrap();
    let saves = tmp.path().join("saves");
    let state_dir = tmp.path().join("state");
    fs::create_dir_all(&state_dir).unwrap();
    write_ppsspp_savedata(&saves, "ULUS10437GameData00");

    let config = tmp.path().join("config.ini");
    let flag_line = bundles_flag
        .map(|value| format!("PSP_SAVEDATA_BUNDLES=\"{value}\"\n"))
        .unwrap_or_default();
    let body = format!(
        "URL=\"127.0.0.1\"\nPORT=\"{}\"\nROOT=\"{}\"\nSTATE_DIR=\"{}\"\nWATCH=\"false\"\nWATCH_INTERVAL=\"1\"\n{}\n[source.retrodeck]\nLABEL=\"RetroDECK\"\nKIND=\"retroarch\"\nPROFILE=\"retroarch\"\nSAVE_PATH=\"{}\"\nROM_PATH=\"{}\"\nRECURSIVE=\"true\"\nSYSTEMS=\"psp\"\nCREATE_MISSING_SYSTEM_DIRS=\"false\"\nMANAGED=\"false\"\nORIGIN=\"test\"\n",
        server.port(),
        saves.display(),
        state_dir.display(),
        flag_line,
        saves.display(),
        saves.display(),
    );
    fs::write(&config, body).unwrap();

    Command::cargo_bin("sgm-steamdeck-helper")
        .unwrap()
        .arg("--config")
        .arg(&config)
        .arg("login")
        .arg("--email")
        .arg("psp@example.com")
        .arg("--app-password")
        .arg("pw")
        .assert()
        .success();

    let output = Command::cargo_bin("sgm-steamdeck-helper")
        .unwrap()
        .arg("--config")
        .arg(&config)
        .arg("sync")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(output.status.success(), "sync failed: {stderr}");
    (
        bundle_upload.calls(),
        loose_upload.calls(),
        any_upload.calls(),
        stderr,
    )
}

#[test]
fn a_ppsspp_savedata_folder_uploads_as_one_bundle() {
    let (bundles, loose, other, stderr) = run_ppsspp_sync(Some("true"));
    assert_eq!(
        bundles, 1,
        "the SAVEDATA folder was not uploaded as a bundle: {stderr}"
    );
    assert_eq!(
        loose, 0,
        "the folder's data file still uploaded on its own: {stderr}"
    );
    assert_eq!(other, 0, "an unexpected upload was sent: {stderr}");
}

#[test]
fn with_psp_savedata_bundles_off_no_bundle_is_uploaded() {
    for flag in [None, Some("false")] {
        let (bundles, loose, other, stderr) = run_ppsspp_sync(flag);
        assert_eq!(
            bundles, 0,
            "a bundle was uploaded with the flag {flag:?}: {stderr}"
        );
        // Off means exactly the behaviour before: the scanner does not take a
        // PPSSPP data file as a psp save, so nothing from the folder uploads.
        assert_eq!(
            (loose, other),
            (0, 0),
            "the folder's files uploaded with the flag {flag:?}: {stderr}"
        );
    }
}

// a lock whose owner is running still stops it.
#[test]
fn a_lock_left_by_a_dead_sync_is_taken_over_and_a_live_ones_is_not() {
    let server = MockServer::start();
    let _token = server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_lock","expiresInDays":7}"#);
    });
    let _me = server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"lock@example.com"}}"#);
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
        .arg(&config)
        .arg("login")
        .arg("--email")
        .arg("lock@example.com")
        .arg("--app-password")
        .arg("pw")
        .assert()
        .success();

    // A pid no process holds: above the default Linux pid_max, and not a multiple of 4 (Windows).
    let lock = state_dir.join("sync.lock");
    fs::write(&lock, "pid=4194303\nstarted_at=2026-10-31T00:00:00Z\n").unwrap();
    let dead = Command::cargo_bin("sgm-steamdeck-helper")
        .unwrap()
        .arg("--config")
        .arg(&config)
        .arg("sync")
        .output()
        .unwrap();
    assert!(
        dead.status.success(),
        "a dead sync's lock still stopped the sync:\n{}",
        String::from_utf8_lossy(&dead.stderr)
    );
    assert!(
        !lock.exists(),
        "the taken-over lock was not released when the sync ended"
    );

    // This test's own process: alive, so its lock is left alone.
    fs::write(
        &lock,
        format!(
            "pid={}\nstarted_at=2026-10-31T00:00:00Z\n",
            std::process::id()
        ),
    )
    .unwrap();
    let live = Command::cargo_bin("sgm-steamdeck-helper")
        .unwrap()
        .arg("--config")
        .arg(&config)
        .arg("sync")
        .output()
        .unwrap();
    assert!(
        !live.status.success(),
        "a running sync's lock was taken over"
    );
    assert!(
        String::from_utf8_lossy(&live.stderr).contains("sync is al actief"),
        "a live lock stopped the sync without saying why:\n{}",
        String::from_utf8_lossy(&live.stderr)
    );
    assert!(lock.exists(), "a running sync's lock was removed");
}
