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
fn backend_forced_dry_run_marks_the_summary_and_uploads_nothing() {
    let server = MockServer::start();
    let _token = server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_dry","expiresInDays":7}"#);
    });
    let _me = server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"dry@example.com"}}"#);
    });
    let _rom_lookup = server.mock(|when, then| {
        when.method(GET).path("/rom/lookup");
        then.status(200)
            .header("content-type", "application/json")
            .body(
                r#"{"success":true,"count":1,"rom":{"sha1":"dry123","md5":"md5v","fileName":"wario.sav"}}"#,
            );
    });
    let _save_latest = server.mock(|when, then| {
        when.method(GET).path("/save/latest");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"exists":false,"sha256":null,"version":null,"id":null}"#);
    });
    let _config_sync = server.mock(|when, then| {
        when.method(POST).path("/helpers/config/sync");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"accepted":true,"global":{"dryRun":true}}"#);
    });
    let upload = server.mock(|when, then| {
        when.method(POST).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"save":{"id":"save-dry","sha256":"sha-dry"}}"#);
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
    // A save the classifier cannot place: skipped before any write is decided.
    fs::create_dir_all(root.join("misc")).unwrap();
    fs::write(root.join("misc/unknown.sav"), vec![0x00u8; 32768]).unwrap();
    let config = write_config(&tmp, &server, &root, &state_dir);

    Command::cargo_bin("sgm-steamdeck-helper")
        .unwrap()
        .arg("--config")
        .arg(&config)
        .arg("login")
        .arg("--email")
        .arg("dry@example.com")
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
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success(), "sync failed: {stdout}");
    assert!(
        stdout.contains("Sync complete (DRY RUN)"),
        "summary line carries no dry-run marker: {stdout}"
    );
    assert!(
        stdout.contains("uploaded=0 deduplicated=0 accepted_not_stored=0 downloaded=0"),
        "dry run counted a simulation as work done: {stdout}"
    );
    assert!(
        stdout.contains(
            "skipped=1 rejected_n64_controller_pak=0 rejected_permanent=0 \
             refused_size_mismatch=0 errors=0 would_upload=1 would_download=0"
        ),
        "a skipped save was counted as a simulated upload, or the simulation was missed: {stdout}"
    );
    assert_eq!(upload.calls(), 0);
}

#[test]
fn port_save_dry_run_counts_a_simulation_not_an_upload() {
    let server = MockServer::start();
    let _token = server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_port","expiresInDays":7}"#);
    });
    let _me = server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"port@example.com"}}"#);
    });
    let _latest = server.mock(|when, then| {
        when.method(GET).path("/save/latest");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"exists":false,"sha256":null,"version":null,"id":null}"#);
    });
    let upload = server.mock(|when, then| {
        when.method(POST).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"save":{"id":"save-port","sha256":"sha-port"}}"#);
    });

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    let port_save = root.join("ports/OcarinaOfTime/Save/global.sav");
    fs::create_dir_all(port_save.parent().unwrap()).unwrap();
    fs::write(&port_save, vec![0x00u8; 4096]).unwrap();
    fs::create_dir_all(&state_dir).unwrap();

    let config = tmp.path().join("config.ini");
    let ports_root = root.join("ports");
    let body = format!(
        "URL=\"127.0.0.1\"\nPORT=\"{}\"\nROOT=\"{}\"\nSTATE_DIR=\"{}\"\nWATCH=\"true\"\nWATCH_INTERVAL=\"1\"\n\n[source.ports]\nLABEL=\"Ports\"\nKIND=\"ports\"\nSAVE_PATH=\"{}\"\n",
        server.port(),
        root.display(),
        state_dir.display(),
        ports_root.display()
    );
    fs::write(&config, body).unwrap();

    Command::cargo_bin("sgm-steamdeck-helper")
        .unwrap()
        .arg("--config")
        .arg(&config)
        .arg("login")
        .arg("--email")
        .arg("port@example.com")
        .arg("--app-password")
        .arg("pw")
        .assert()
        .success();

    let output = Command::cargo_bin("sgm-steamdeck-helper")
        .unwrap()
        .arg("--config")
        .arg(&config)
        .arg("sync")
        .arg("--dry-run")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success(), "sync failed: {stdout}");
    assert!(
        stdout.contains("uploaded=0 deduplicated=0 accepted_not_stored=0 downloaded=0"),
        "dry run counted a simulation as work done: {stdout}"
    );
    assert!(
        stdout.contains("would_upload=1"),
        "the port-save dry run did not count the upload it skipped: {stdout}"
    );
    assert_eq!(upload.calls(), 0);
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
    fs::write(
        root.join("Sony/PSX/memory_card_1.gme"),
        encode_ps1_dexdrive(&raw),
    )
    .unwrap();
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

/// A save root with one title in two directories: the system directory an
/// older helper wrote and a core-named one. `declared` is the SAVE_SUBDIR line
/// an operator may add, or empty.
fn live_directory_config(
    tmp: &tempfile::TempDir,
    server: &MockServer,
    root: &std::path::Path,
    state_dir: &std::path::Path,
    declared: &str,
) -> std::path::PathBuf {
    let config = tmp.path().join("config-live-dir.ini");
    fs::write(
        &config,
        format!(
            "URL=\"127.0.0.1\"\nPORT=\"{}\"\nROOT=\"{}\"\nSTATE_DIR=\"{}\"\nWATCH=\"false\"\nWATCH_INTERVAL=\"1\"\n\n[source.retroarch]\nLABEL=\"RetroArch\"\nKIND=\"retroarch\"\nPROFILE=\"retroarch\"\nSAVE_PATH=\"{}\"\nROM_PATH=\"{}\"\nRECURSIVE=\"true\"\nSYSTEMS=\"snes\"\n{}\n",
            server.port(),
            root.display(),
            state_dir.display(),
            root.join("saves").display(),
            root.join("roms").display(),
            declared
        ),
    )
    .unwrap();
    config
}

/// Eight kilobytes of non-repeating, non-text bytes: plausible for SNES and
/// refused by nothing.
fn snes_save_bytes(seed: u8) -> Vec<u8> {
    (0..8192u32)
        .map(|index| ((index as u8).wrapping_mul(37).wrapping_add(seed)) | 0x01)
        .collect()
}

// A device that sorts saves by core writes one title into a directory named
// after the core, while an older helper wrote the system-named one. Both copies
// are real; only one may sync, or a wrong upload replaces the record the other
// machine made.
#[test]
fn a_declared_live_directory_is_the_copy_that_uploads() {
    let server = MockServer::start();
    let _token = server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_live","expiresInDays":7}"#);
    });
    let _me = server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"live@example.com"}}"#);
    });
    let _latest = server.mock(|when, then| {
        when.method(GET).path("/save/latest");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"exists":false,"sha256":null,"version":null,"id":null}"#);
    });
    let upload = server.mock(|when, then| {
        let _list = server.mock(|when, then| {
            when.method(GET).path("/saves");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"success":true,"total":0,"limit":100,"offset":0,"saves":[]}"#);
        });

        when.method(POST).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"save":{"id":"save-live","sha256":"sha-live"}}"#);
    });

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    fs::create_dir_all(root.join("saves/snes")).unwrap();
    fs::create_dir_all(root.join("saves/Snes9x")).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    let system_copy = root.join("saves/snes/Tecmo Super Bowl (USA).srm");
    let core_copy = root.join("saves/Snes9x/Tecmo Super Bowl (USA).srm");
    fs::write(&system_copy, snes_save_bytes(11)).unwrap();
    fs::write(&core_copy, snes_save_bytes(29)).unwrap();
    fs::create_dir_all(root.join("roms/snes")).unwrap();
    fs::write(root.join("roms/snes/Tecmo Super Bowl (USA).sfc"), b"rom").unwrap();
    let config = live_directory_config(
        &tmp,
        &server,
        &root,
        &state_dir,
        "SAVE_SUBDIR_snes=\"Snes9x\"",
    );
    wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("login")
        .arg("--email")
        .arg("live@example.com")
        .arg("--app-password")
        .arg("pw")
        .assert()
        .success();

    let output = wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("sync")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert_eq!(
        upload.calls(),
        1,
        "exactly one copy may upload; both or neither is the defect:\n{stderr}\n--- stdout ---\n{stdout}"
    );
    assert!(
        stderr.contains("Not uploading") && stderr.contains("stray copy"),
        "the copy outside the declared directory was not named:\n{stderr}"
    );
    assert!(
        !stderr.contains(&format!("Not uploading {}", core_copy.display())),
        "the declared copy was skipped:\n{stderr}"
    );
}

// Nothing declared and two copies: the helper cannot tell which one the
// emulator reads, so neither uploads. A save that waits beats a record that is
// overwritten.
#[test]
fn two_copies_with_nothing_declared_do_not_upload() {
    let server = MockServer::start();
    let _token = server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_amb","expiresInDays":7}"#);
    });
    let _me = server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"amb@example.com"}}"#);
    });
    let _latest = server.mock(|when, then| {
        when.method(GET).path("/save/latest");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"exists":false,"sha256":null,"version":null,"id":null}"#);
    });
    let upload = server.mock(|when, then| {
        let _list = server.mock(|when, then| {
            when.method(GET).path("/saves");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"success":true,"total":0,"limit":100,"offset":0,"saves":[]}"#);
        });

        when.method(POST).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"save":{"id":"save-amb","sha256":"sha-amb"}}"#);
    });

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    fs::create_dir_all(root.join("saves/snes")).unwrap();
    fs::create_dir_all(root.join("saves/Snes9x")).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    fs::write(
        root.join("saves/snes/Tecmo Super Bowl (USA).srm"),
        snes_save_bytes(11),
    )
    .unwrap();
    fs::write(
        root.join("saves/Snes9x/Tecmo Super Bowl (USA).srm"),
        snes_save_bytes(29),
    )
    .unwrap();
    fs::create_dir_all(root.join("roms/snes")).unwrap();
    fs::write(root.join("roms/snes/Tecmo Super Bowl (USA).sfc"), b"rom").unwrap();
    let config = live_directory_config(&tmp, &server, &root, &state_dir, "");
    wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("login")
        .arg("--email")
        .arg("amb@example.com")
        .arg("--app-password")
        .arg("pw")
        .assert()
        .success();

    let output = wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("sync")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(
        upload.calls(),
        0,
        "a copy uploaded while the live directory was undecided:\n{stderr}"
    );
    assert!(
        stderr.contains("no live directory is declared"),
        "the conflict was not reported with its remedy:\n{stderr}"
    );
}

// Two copies and nothing declared: the restore must not pick one. It reports
// the conflict, makes no request for the payload, and leaves both files alone.
#[test]
fn a_restore_with_two_copies_and_no_declaration_writes_nothing() {
    let server = MockServer::start();
    let _token = server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_rc","expiresInDays":7}"#);
    });
    let _me = server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"rc@example.com"}}"#);
    });
    let _latest = server.mock(|when, then| {
        when.method(GET).path("/save/latest");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"exists":false,"sha256":null,"version":null,"id":null}"#);
    });
    let _list = server.mock(|when, then| {
        when.method(GET).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"total":1,"limit":100,"offset":0,"saves":[{"id":"save-r","filename":"Tecmo Super Bowl (USA).srm","displayTitle":"Tecmo Super Bowl","systemSlug":"snes","sha256":"sha-r","version":3,"latestSizeBytes":8192}]}"#);
    });
    let download = server.mock(|when, then| {
        when.method(GET).path("/saves/download");
        then.status(200).body(snes_save_bytes(7));
    });

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    fs::create_dir_all(root.join("saves/snes")).unwrap();
    fs::create_dir_all(root.join("saves/Snes9x")).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    let system_copy = root.join("saves/snes/Tecmo Super Bowl (USA).srm");
    let core_copy = root.join("saves/Snes9x/Tecmo Super Bowl (USA).srm");
    fs::write(&system_copy, snes_save_bytes(11)).unwrap();
    fs::write(&core_copy, snes_save_bytes(29)).unwrap();
    let config = live_directory_config(&tmp, &server, &root, &state_dir, "");

    wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("login")
        .arg("--email")
        .arg("rc@example.com")
        .arg("--app-password")
        .arg("pw")
        .assert()
        .success();
    let output = wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("sync")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("Not restoring") && stderr.contains("no live directory is declared"),
        "the restore conflict was not reported:\n{stderr}"
    );
    assert_eq!(
        download.calls(),
        0,
        "a payload was fetched for a target the helper refused to choose:\n{stderr}"
    );
    assert_eq!(fs::read(&system_copy).unwrap(), snes_save_bytes(11));
    assert_eq!(fs::read(&core_copy).unwrap(), snes_save_bytes(29));
}

// Declared, with a second copy beside it: the restore lands in the declared
// directory and the other copy is named as a stray.
#[test]
fn a_restore_lands_in_the_declared_directory() {
    let server = MockServer::start();
    let _token = server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_rd","expiresInDays":7}"#);
    });
    let _me = server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"rd@example.com"}}"#);
    });
    let _latest = server.mock(|when, then| {
        when.method(GET).path("/save/latest");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"exists":false,"sha256":null,"version":null,"id":null}"#);
    });
    let _list = server.mock(|when, then| {
        when.method(GET).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"total":1,"limit":100,"offset":0,"saves":[{"id":"save-rd","filename":"Tecmo Super Bowl (USA).srm","displayTitle":"Tecmo Super Bowl","systemSlug":"snes","sha256":"sha-rd","version":3,"latestSizeBytes":8192}]}"#);
    });
    let _download = server.mock(|when, then| {
        when.method(GET).path("/saves/download");
        then.status(200).body(snes_save_bytes(7));
    });

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    fs::create_dir_all(root.join("saves/snes")).unwrap();
    fs::create_dir_all(root.join("saves/Snes9x")).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    let system_copy = root.join("saves/snes/Tecmo Super Bowl (USA).srm");
    let core_copy = root.join("saves/Snes9x/Tecmo Super Bowl (USA).srm");
    fs::write(&system_copy, snes_save_bytes(11)).unwrap();
    // Truncated, so it is present for the directory rule but not a save the
    // helper will keep instead of restoring.
    fs::write(&core_copy, vec![0x5Au8; 100]).unwrap();
    let config = live_directory_config(
        &tmp,
        &server,
        &root,
        &state_dir,
        "SAVE_SUBDIR_snes=\"Snes9x\"",
    );

    wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("login")
        .arg("--email")
        .arg("rd@example.com")
        .arg("--app-password")
        .arg("pw")
        .assert()
        .success();
    let output = wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("sync")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(
        fs::read(&core_copy).unwrap(),
        snes_save_bytes(7),
        "the restore did not land in the declared directory:\n{stderr}"
    );
    assert_eq!(
        fs::read(&system_copy).unwrap(),
        snes_save_bytes(11),
        "the stray copy was written:\n{stderr}"
    );
    assert!(
        stderr.contains("Stray copy") && stderr.contains("saves/snes"),
        "the stray was not named:\n{stderr}"
    );
}

// A local file that cannot be any save of its system (a truncated .srm, a few
// bytes long) does not count as present, so the server's save is restored over
// it; that is what heals a truncated save. A local file that is a valid save and
// differs from the server's is not overwritten by the restore: it goes through
// the sync, where a change on both sides is a conflict.
#[test]
fn an_unclassifiable_local_file_is_replaced_and_a_valid_one_is_kept() {
    let server = MockServer::start();
    let _token = server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_heal","expiresInDays":7}"#);
    });
    let _me = server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"heal@example.com"}}"#);
    });
    let _latest = server.mock(|when, then| {
        when.method(GET).path("/save/latest");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"exists":false,"sha256":null,"version":null,"id":null}"#);
    });
    let _list = server.mock(|when, then| {
        when.method(GET).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"total":2,"limit":100,"offset":0,"saves":[{"id":"save-trunc","filename":"Truncated Quest (USA).srm","displayTitle":"Truncated Quest","systemSlug":"snes","sha256":"sha-trunc","version":2,"latestSizeBytes":8192},{"id":"save-kept","filename":"Kept Quest (USA).srm","displayTitle":"Kept Quest","systemSlug":"snes","sha256":"sha-kept","version":2,"latestSizeBytes":8192}]}"#);
    });
    let _download = server.mock(|when, then| {
        when.method(GET).path("/saves/download");
        then.status(200).body(snes_save_bytes(7));
    });
    let _upload = server.mock(|when, then| {
        when.method(POST).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"save":{"id":"save-kept","sha256":"x","version":3}}"#);
    });

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    fs::create_dir_all(root.join("saves/snes")).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    let truncated = root.join("saves/snes/Truncated Quest (USA).srm");
    let kept = root.join("saves/snes/Kept Quest (USA).srm");
    fs::write(&truncated, vec![0x31u8; 8]).unwrap();
    fs::write(&kept, snes_save_bytes(11)).unwrap();
    let config = live_directory_config(&tmp, &server, &root, &state_dir, "");

    wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("login")
        .arg("--email")
        .arg("heal@example.com")
        .arg("--app-password")
        .arg("pw")
        .assert()
        .success();
    let output = wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("sync")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(
        fs::read(&truncated).unwrap(),
        snes_save_bytes(7),
        "an unclassifiable local file was not replaced by the server's save:\n{stderr}"
    );
    assert_eq!(
        fs::read(&kept).unwrap(),
        snes_save_bytes(11),
        "a valid local save that differs from the server's was overwritten by the restore:\n{stderr}"
    );
}

#[test]
fn a_truncated_game_boy_save_is_replaced_by_the_cloud_copy() {
    let server = MockServer::start();
    let _token = server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_heal","expiresInDays":7}"#);
    });
    let _me = server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"heal@example.com"}}"#);
    });
    let _latest = server.mock(|when, then| {
        when.method(GET).path("/save/latest");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"exists":false,"sha256":null,"version":null,"id":null}"#);
    });
    let _list = server.mock(|when, then| {
        when.method(GET).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"total":2,"limit":100,"offset":0,"saves":[{"id":"save-trunc","filename":"Truncated Quest (USA).srm","displayTitle":"Truncated Quest","systemSlug":"gameboy","sha256":"sha-trunc","version":2,"latestSizeBytes":8192},{"id":"save-kept","filename":"Kept Quest (USA).srm","displayTitle":"Kept Quest","systemSlug":"gameboy","sha256":"sha-kept","version":2,"latestSizeBytes":8192}]}"#);
    });
    let _download = server.mock(|when, then| {
        when.method(GET).path("/saves/download");
        then.status(200).body(snes_save_bytes(7));
    });
    let _upload = server.mock(|when, then| {
        when.method(POST).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"save":{"id":"save-kept","sha256":"x","version":3}}"#);
    });

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    fs::create_dir_all(root.join("saves/gbc")).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    let truncated = root.join("saves/gbc/Truncated Quest (USA).srm");
    let kept = root.join("saves/gbc/Kept Quest (USA).srm");
    fs::write(&truncated, vec![0x31u8; 8]).unwrap();
    fs::write(&kept, snes_save_bytes(11)).unwrap();
    let config = live_directory_config(&tmp, &server, &root, &state_dir, "");
    let text = fs::read_to_string(&config)
        .unwrap()
        .replace("SYSTEMS=\"snes\"", "SYSTEMS=\"gameboy\"");
    fs::write(&config, text).unwrap();

    wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("login")
        .arg("--email")
        .arg("heal@example.com")
        .arg("--app-password")
        .arg("pw")
        .assert()
        .success();
    let output = wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("sync")
        .arg("--verbose")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(
        fs::read(&truncated).unwrap(),
        snes_save_bytes(7),
        "an unclassifiable local file was not replaced by the server's save:\n{stderr}"
    );
    assert_eq!(
        fs::read(&kept).unwrap(),
        snes_save_bytes(11),
        "a valid local save that differs from the server's was overwritten by the restore:\n{stderr}"
    );
}

// A small file that some other system's profile accepts is a save, not junk:
// it classifies there, so a restore for this system must not write over it.
#[test]
fn a_file_this_helper_synced_as_another_system_is_not_overwritten() {
    let server = MockServer::start();
    let _token = server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_other","expiresInDays":7}"#);
    });
    let _me = server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"other@example.com"}}"#);
    });
    let _latest = server.mock(|when, then| {
        when.method(GET).path("/save/latest");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"exists":false,"sha256":null,"version":null,"id":null}"#);
    });
    let _list = server.mock(|when, then| {
        when.method(GET).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"total":1,"limit":100,"offset":0,"saves":[{"id":"save-other","filename":"Other Quest (USA).srm","displayTitle":"Other Quest","systemSlug":"snes","sha256":"sha-other","version":2,"latestSizeBytes":8192}]}"#);
    });
    let download = server.mock(|when, then| {
        when.method(GET).path("/saves/download");
        then.status(200).body(snes_save_bytes(7));
    });

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    fs::create_dir_all(root.join("saves/snes")).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    // 64 bytes of non-blank binary: SNES rejects it, a Genesis cart save can be
    // 64 bytes, and this helper's state records the file as a genesis save, so a
    // snes record is not written over it.
    let other: Vec<u8> = (0..64u32)
        .map(|i| (i.wrapping_mul(7) ^ 0xa5) as u8)
        .collect();
    let path = root.join("saves/snes/Other Quest (USA).srm");
    fs::write(&path, &other).unwrap();
    writeback_state(&state_dir, &path, "genesis");
    let config = live_directory_config(&tmp, &server, &root, &state_dir, "");

    wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("login")
        .arg("--email")
        .arg("other@example.com")
        .arg("--app-password")
        .arg("pw")
        .assert()
        .success();
    let output = wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("sync")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(
        fs::read(&path).unwrap(),
        other,
        "a file this helper synced as a genesis save was overwritten by a snes record:\n{stderr}"
    );
    assert_eq!(
        download.calls(),
        0,
        "the server copy was downloaded only to be thrown away:\n{stderr}"
    );
}

#[test]
fn a_file_this_system_cannot_use_is_replaced_without_other_evidence() {
    let server = MockServer::start();
    let _token = server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_other","expiresInDays":7}"#);
    });
    let _me = server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"other@example.com"}}"#);
    });
    let _latest = server.mock(|when, then| {
        when.method(GET).path("/save/latest");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"exists":false,"sha256":null,"version":null,"id":null}"#);
    });
    let _list = server.mock(|when, then| {
        when.method(GET).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"total":1,"limit":100,"offset":0,"saves":[{"id":"save-other","filename":"Other Quest (USA).srm","displayTitle":"Other Quest","systemSlug":"snes","sha256":"sha-other","version":2,"latestSizeBytes":8192}]}"#);
    });
    let _download = server.mock(|when, then| {
        when.method(GET).path("/saves/download");
        then.status(200).body(snes_save_bytes(7));
    });

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    fs::create_dir_all(root.join("saves/snes")).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    // The same 64 bytes with no state entry: nothing says it is another
    // system's save, and SNES can't use it, so the server's snes save replaces it.
    let other: Vec<u8> = (0..64u32)
        .map(|i| (i.wrapping_mul(7) ^ 0xa5) as u8)
        .collect();
    let path = root.join("saves/snes/Other Quest (USA).srm");
    fs::write(&path, &other).unwrap();
    let config = live_directory_config(&tmp, &server, &root, &state_dir, "");

    wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("login")
        .arg("--email")
        .arg("other@example.com")
        .arg("--app-password")
        .arg("pw")
        .assert()
        .success();
    let output = wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("sync")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(
        fs::read(&path).unwrap(),
        snes_save_bytes(7),
        "a file nothing marks as another system's save was not replaced:\n{stderr}"
    );
}

fn wb_helper() -> Command {
    Command::cargo_bin("sgm-steamdeck-helper").unwrap()
}

// RetroArch with sort_savefiles_enable files saves by the core that reads them,
// so a Game Gear save lives under `genesis/` because genesis_plus_gx serves
// both systems. The server names the system correctly; the helper looked only
// for a system-named directory, missed, and skipped the restore.
#[test]
fn a_cloud_save_is_restored_into_the_directory_that_already_holds_it() {
    let server = MockServer::start();
    let _token = server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_writeback","expiresInDays":7}"#);
    });
    let _me = server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"writeback@example.com"}}"#);
    });
    let _save_latest = server.mock(|when, then| {
        when.method(GET).path("/save/latest");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"exists":false,"sha256":null,"version":null,"id":null}"#);
    });
    let _list = server.mock(|when, then| {
        when.method(GET).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"total":1,"limit":100,"offset":0,"saves":[{"id":"save-defenders","filename":"Defenders of Oasis (USA, Europe).srm","displayTitle":"Defenders of Oasis","systemSlug":"game-gear","sha256":"701641b95e5e7394","version":3,"latestSizeBytes":65536}]}"#);
    });
    let download = server.mock(|when, then| {
        when.method(GET).path("/saves/download");
        then.status(200).body(vec![0xC7u8; 65536]);
    });

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    fs::create_dir_all(root.join("saves/genesis")).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    let local = root.join("saves/genesis/Defenders of Oasis (USA, Europe).srm");
    fs::write(&local, vec![0xC7u8; 65536]).unwrap();
    // The helper has synced this file as a game-gear save before, which is what
    // makes it safe to write a game-gear record back over it.
    writeback_state(&state_dir, &local, "game-gear");
    let config = writeback_config(&tmp, &server, &root, &state_dir);

    wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("login")
        .arg("--email")
        .arg("writeback@example.com")
        .arg("--app-password")
        .arg("pw")
        .assert()
        .success();
    let output = wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("sync")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(download.calls(), 1, "the restore was skipped:\n{stderr}");
    assert!(local.exists(), "the save is gone:\n{stderr}");
    // Nothing is created: the directory the core reads already exists.
    assert!(
        !root.join("saves/gamegear").exists(),
        "a new system directory was created:\n{stderr}"
    );
}

// The server's downloadProfiles entry declares the extension, and a profile can
// disagree with the core: genesis-plus-gx is listed with ".sav" while every
// device holds ".srm". Matching the record's filename exactly then misses the
// file that is already there and the restore falls back to a system directory
// the core never uses.
#[test]
fn a_cloud_save_is_restored_where_its_stem_already_lives_under_another_extension() {
    let server = MockServer::start();
    let _token = server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_stem","expiresInDays":7}"#);
    });
    let _me = server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"stem@example.com"}}"#);
    });
    let _save_latest = server.mock(|when, then| {
        when.method(GET).path("/save/latest");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"exists":false,"sha256":null,"version":null,"id":null}"#);
    });
    let _list = server.mock(|when, then| {
        when.method(GET).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"total":1,"limit":100,"offset":0,"saves":[{"id":"save-defenders","filename":"Defenders of Oasis (USA, Europe).sav","displayTitle":"Defenders of Oasis","systemSlug":"game-gear","sha256":"701641b95e5e7394","version":3,"latestSizeBytes":65536,"downloadProfiles":[{"id":"original","label":"Original","targetExtension":".sav"}]}]}"#);
    });
    let download = server.mock(|when, then| {
        when.method(GET).path("/saves/download");
        then.status(200).body(vec![0xC7u8; 65536]);
    });

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    fs::create_dir_all(root.join("saves/genesis")).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    let local = root.join("saves/genesis/Defenders of Oasis (USA, Europe).srm");
    fs::write(&local, vec![0x11u8; 65536]).unwrap();
    writeback_state(&state_dir, &local, "game-gear");
    let config = writeback_config(&tmp, &server, &root, &state_dir);

    wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("login")
        .arg("--email")
        .arg("stem@example.com")
        .arg("--app-password")
        .arg("pw")
        .assert()
        .success();
    let output = wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("sync")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(download.calls(), 1, "the restore was skipped:\n{stderr}");
    assert_eq!(
        fs::read(&local).unwrap(),
        vec![0xC7u8; 65536],
        "the save was restored somewhere else:\n{stderr}"
    );
    assert!(
        !root.join("saves/gamegear").exists(),
        "a new system directory was created:\n{stderr}"
    );
    assert!(
        stderr.contains("under another extension"),
        "the restore does not say which file it used:\n{stderr}"
    );
}

// A /saves response the client cannot read means no restore runs at all. That
// used to be reported only under --verbose, so the run looked clean while the
// device restored nothing.
#[test]
fn an_unreadable_save_list_is_reported_not_swallowed() {
    let server = MockServer::start();
    let _token = server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_unreadable","expiresInDays":7}"#);
    });
    let _me = server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"unreadable@example.com"}}"#);
    });
    let _save_latest = server.mock(|when, then| {
        when.method(GET).path("/save/latest");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"exists":false,"sha256":null,"version":null,"id":null}"#);
    });
    // A downloadProfiles entry without its id: the client cannot deserialize
    // this, and must say so rather than restore nothing in silence.
    let _list = server.mock(|when, then| {
        when.method(GET).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"total":1,"limit":100,"offset":0,"saves":[{"id":"save-defenders","filename":"Defenders of Oasis (USA, Europe).sav","displayTitle":"Defenders of Oasis","systemSlug":"game-gear","sha256":"701641b95e5e7394","version":3,"latestSizeBytes":65536,"downloadProfiles":[{"targetExtension":".sav"}]}]}"#);
    });
    let download = server.mock(|when, then| {
        when.method(GET).path("/saves/download");
        then.status(200).body(vec![0xC7u8; 65536]);
    });

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    fs::create_dir_all(root.join("saves/genesis")).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    let local = root.join("saves/genesis/Defenders of Oasis (USA, Europe).srm");
    fs::write(&local, vec![0x11u8; 65536]).unwrap();
    writeback_state(&state_dir, &local, "game-gear");
    let config = writeback_config(&tmp, &server, &root, &state_dir);

    wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("login")
        .arg("--email")
        .arg("unreadable@example.com")
        .arg("--app-password")
        .arg("pw")
        .assert()
        .success();
    let output = wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("sync")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(
        download.calls(),
        0,
        "a save was restored from an unreadable list:\n{stderr}"
    );
    assert!(
        stderr.contains("Cloud restore skipped") && stderr.contains("/saves unavailable"),
        "the skipped restore is not reported without --verbose:\n{stderr}"
    );
}

// A same-named save in the write-back directory may belong to another
// system; the helper must not overwrite it on a name match alone.
#[test]
fn a_write_back_does_not_overwrite_another_systems_save() {
    let server = MockServer::start();
    let _token = server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_ambiguous","expiresInDays":7}"#);
    });
    let _me = server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"ambiguous@example.com"}}"#);
    });
    let _save_latest = server.mock(|when, then| {
        when.method(GET).path("/save/latest");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"exists":false,"sha256":null,"version":null,"id":null}"#);
    });
    let _list = server.mock(|when, then| {
        when.method(GET).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"total":1,"limit":100,"offset":0,"saves":[{"id":"save-defenders","filename":"Defenders of Oasis (USA, Europe).srm","displayTitle":"Defenders of Oasis","systemSlug":"game-gear","sha256":"701641b95e5e7394","version":3,"latestSizeBytes":65536}]}"#);
    });
    let download = server.mock(|when, then| {
        when.method(GET).path("/saves/download");
        then.status(200).body(vec![0xC7u8; 65536]);
    });

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    fs::create_dir_all(root.join("saves/genesis")).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    // A same-named save belonging to another system: Sonic is a Genesis game and
    // a Game Gear game, and the helper must not write one over the other.
    let genesis = root.join("saves/genesis/Defenders of Oasis (USA, Europe).srm");
    fs::write(&genesis, vec![0x11u8; 65536]).unwrap();
    writeback_state(&state_dir, &genesis, "genesis");
    let config = writeback_config(&tmp, &server, &root, &state_dir);

    wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("login")
        .arg("--email")
        .arg("ambiguous@example.com")
        .arg("--app-password")
        .arg("pw")
        .assert()
        .success();
    let output = wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("sync")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(download.calls(), 0, "a guess was made:\n{stderr}");
    assert!(
        fs::read(&genesis).unwrap() == vec![0x11u8; 65536],
        "another system's save was overwritten:\n{stderr}"
    );
    assert!(
        stderr.contains("has not synced as game-gear"),
        "the refusal does not say why it declined to write there:\n{stderr}"
    );
}

fn writeback_config(
    tmp: &tempfile::TempDir,
    server: &MockServer,
    root: &std::path::Path,
    state_dir: &std::path::Path,
) -> std::path::PathBuf {
    let config = tmp.path().join("config-writeback.ini");
    fs::write(
        &config,
        format!(
            "URL=\"127.0.0.1\"\nPORT=\"{}\"\nROOT=\"{}\"\nSTATE_DIR=\"{}\"\nWATCH=\"false\"\nWATCH_INTERVAL=\"1\"\n\n[source.retroarch]\nLABEL=\"RetroArch\"\nKIND=\"retroarch\"\nPROFILE=\"retroarch\"\nSAVE_PATH=\"{}\"\nROM_PATH=\"{}\"\nRECURSIVE=\"true\"\nSYSTEMS=\"game-gear\"\n",
            server.port(),
            root.display(),
            state_dir.display(),
            root.join("saves").display(),
            root.join("roms").display()
        ),
    )
    .unwrap();
    config
}

fn writeback_state(state_dir: &std::path::Path, path: &std::path::Path, system_slug: &str) {
    let state = format!(
        r#"{{"entries": {{"{}": {{"sha256": "701641b95e5e7394", "rom_sha1": "rom-sha", "version": 3, "system_slug": "{}", "local_container": "native", "adapter_profile": "identity", "source_kind": "retroarch", "source_name": "RetroArch", "slot_name": "default", "updated_at": "2026-01-01T00:00:00Z"}}}}}}"#,
        path.display(),
        system_slug
    );
    fs::write(state_dir.join("sync_state.json"), state).unwrap();
}

// RetroArch's Beetle PSX keeps one card per game, named after it. That card is
// its own line on the server ("Game: <name>"), not the shared Memory Card 1,
// so one game's card can't read as a deletion of another game's save.
#[test]
fn a_per_game_ps1_card_uploads_as_its_own_slot() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_pg","expiresInDays":7}"#);
    });
    server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"pg@example.com"}}"#);
    });
    server.mock(|when, then| {
        when.method(GET).path("/save/latest");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"exists":false,"sha256":null,"version":null,"id":null}"#);
    });
    server.mock(|when, then| {
        when.method(GET).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"total":0,"limit":100,"offset":0,"saves":[]}"#);
    });
    let upload = server.mock(|when, then| {
        when.method(POST)
            .path("/saves")
            .body_includes("ps-line:psx:retroarch:game:-tekken-3-(usa)")
            .body_includes("Game: Tekken 3 (USA)");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"save":{"id":"save-pg","sha256":"sha-pg"}}"#);
    });

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    fs::create_dir_all(root.join("saves/psx")).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    fs::write(
        root.join("saves/psx/Tekken 3 (USA).srm"),
        build_valid_ps1_memcard(),
    )
    .unwrap();
    let config = write_config(&tmp, &server, &root, &state_dir);
    Command::cargo_bin("sgm-steamdeck-helper")
        .unwrap()
        .arg("--config")
        .arg(&config)
        .arg("login")
        .arg("--email")
        .arg("pg@example.com")
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
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        upload.calls(),
        1,
        "the per-game card did not upload as its own slot:\n{stderr}"
    );
}

// The restore half: a per-game card's record lands in that game's card, which
// is the file RetroArch reads, not under the title written inside the save.
#[test]
fn a_per_game_ps1_card_restores_into_the_games_card() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_pgr","expiresInDays":7}"#);
    });
    server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"pgr@example.com"}}"#);
    });
    server.mock(|when, then| {
        when.method(GET).path("/save/latest");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"exists":false,"sha256":null,"version":null,"id":null}"#);
    });
    server.mock(|when, then| {
        when.method(GET).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"total":1,"limit":100,"offset":0,"saves":[{"id":"save-pgr","filename":"Tekken 3 (USA).mcr","displayTitle":"[TEKKEN 3] THE BATTLE BEGINS!!","systemSlug":"psx","cardSlot":"Game: Tekken 3 (USA)","runtimeProfile":"psx/retroarch","sha256":"sha-pgr","version":1,"latestSizeBytes":131072}]}"#);
    });
    let card = build_valid_ps1_memcard();
    let body = card.clone();
    server.mock(move |when, then| {
        when.method(GET).path("/saves/download");
        then.status(200).body(body.clone());
    });

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    fs::create_dir_all(root.join("psx")).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    let config = write_config(&tmp, &server, &root, &state_dir);
    Command::cargo_bin("sgm-steamdeck-helper")
        .unwrap()
        .arg("--config")
        .arg(&config)
        .arg("login")
        .arg("--email")
        .arg("pgr@example.com")
        .arg("--app-password")
        .arg("pw")
        .assert()
        .success();
    let output = Command::cargo_bin("sgm-steamdeck-helper")
        .unwrap()
        .arg("--config")
        .arg(&config)
        .arg("--verbose")
        .arg("sync")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let restored = fs::read(root.join("psx/Tekken 3 (USA).srm")).ok();
    let listing: Vec<String> = fs::read_dir(root.join("psx"))
        .map(|dir| {
            dir.flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        restored.as_deref() == Some(card.as_slice()),
        "the per-game card did not restore to Tekken 3 (USA).srm; psx holds {listing:?}:\n{stderr}"
    );
}

fn slot_state_synced(
    state_dir: &std::path::Path,
    path: &std::path::Path,
    system_slug: &str,
    slot: &str,
    sha256: &str,
) {
    let state = format!(
        r#"{{"entries": {{"{}": {{"sha256": "{}", "rom_sha1": "rom-sha", "version": 3, "system_slug": "{}", "local_container": "native", "adapter_profile": "identity", "source_kind": "retroarch", "source_name": "RetroArch", "slot_name": "{}", "updated_at": "2026-01-01T00:00:00Z"}}}}}}"#,
        path.display(),
        sha256,
        system_slug,
        slot
    );
    fs::write(state_dir.join("sync_state.json"), state).unwrap();
}

#[test]
fn a_save_moved_to_a_new_card_is_uploaded_when_this_device_last_synced_the_same_bytes() {
    // A played PS1 card: real save blocks, the shape the live per-game card has.
    let bytes = build_valid_ps1_memcard();
    let sha = {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        format!("{:x}", hasher.finalize())
    };

    let server = MockServer::start();
    let _token = server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_slot","expiresInDays":7}"#);
    });
    let _me = server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"slot@example.com"}}"#);
    });
    let latest_body = format!(
        r#"{{"success":true,"exists":true,"sha256":"{}","version":4,"id":"save-tekken","latestSizeBytes":131072}}"#,
        sha
    );
    let _save_latest = server.mock(|when, then| {
        when.method(GET).path("/save/latest");
        then.status(200)
            .header("content-type", "application/json")
            .body(latest_body);
    });
    let _conflict_check = server.mock(|when, then| {
        when.method(GET).path("/conflicts/check");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"exists":false}"#);
    });
    let upload = server.mock(|when, then| {
        when.method(POST).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"save":{"id":"save-tekken","sha256":"stored","version":5}}"#);
    });

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    fs::create_dir_all(root.join("saves/psx")).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    let local = root.join("saves/psx/Tekken 3 (USA).srm");
    fs::write(&local, &bytes).unwrap();
    // This device last synced the save under the shared card, which is the
    // live shape: the file now sits on a per-game card.
    // The live shape: this device last synced exactly these bytes, so
    // nothing marks the file as edited -- only the card moved.
    slot_state_synced(&state_dir, &local, "psx", "Memory Card 1", &sha);
    let config = writeback_config_for(&tmp, &server, &root, &state_dir, "psx");

    wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("login")
        .arg("--email")
        .arg("slot@example.com")
        .arg("--app-password")
        .arg("pw")
        .assert()
        .success();
    let output = wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("sync")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(
        upload.calls(),
        1,
        "the save was not re-offered under its new card, so the server still serves it from the old one:\n{stderr}"
    );
}

#[test]
fn a_ps1_card_named_after_no_rom_in_the_index_is_not_uploaded() {
    // A blank formatted PS1 card: frame 0 carries the "MC" magic and frames
    // 0-15 each end in their XOR checksum, which is the evidence the scanner
    // requires before it reads a .srm as a card at all.
    let mut bytes = vec![0x00u8; 131072];
    bytes[..2].copy_from_slice(b"MC");
    for frame in 0..16usize {
        let start = frame * 128;
        let checksum = bytes[start..start + 127]
            .iter()
            .fold(0u8, |acc, value| acc ^ value);
        bytes[start + 127] = checksum;
    }
    let sha = {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        format!("{:x}", hasher.finalize())
    };

    let server = MockServer::start();
    let _token = server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_slot","expiresInDays":7}"#);
    });
    let _me = server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"slot@example.com"}}"#);
    });
    // The server holds nothing for this card, so only the guard stops the upload.
    let latest_body = r#"{"success":true,"exists":false}"#.to_string();
    let _ = &sha;
    let _save_latest = server.mock(|when, then| {
        when.method(GET).path("/save/latest");
        then.status(200)
            .header("content-type", "application/json")
            .body(latest_body);
    });
    let _conflict_check = server.mock(|when, then| {
        when.method(GET).path("/conflicts/check");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"exists":false}"#);
    });
    let upload = server.mock(|when, then| {
        when.method(POST).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"save":{"id":"save-tekken","sha256":"stored","version":5}}"#);
    });

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    fs::create_dir_all(root.join("saves/psx")).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    // The ROM index holds the game; the card is named after the title the
    // game writes inside the card, which names no ROM. Uploading it makes
    // the server open a line for that name and move the game's save onto it.
    fs::create_dir_all(root.join("roms/psx")).unwrap();
    fs::write(root.join("roms/psx/Tekken 3 (USA).chd"), b"rom").unwrap();
    let local = root.join("saves/psx/[TEKKEN 3] THE BATTLE BEGINS!!.srm");
    fs::write(&local, &bytes).unwrap();
    let config = writeback_config_for(&tmp, &server, &root, &state_dir, "psx");

    wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("login")
        .arg("--email")
        .arg("slot@example.com")
        .arg("--app-password")
        .arg("pw")
        .assert()
        .success();
    let output = wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("sync")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(
        upload.calls(),
        0,
        "a card naming no ROM was uploaded, so the server opens a line for it and moves the game's save there:
{stderr}",
    );
}

#[test]
fn a_ps1_card_is_not_refused_by_a_source_whose_rom_root_is_its_save_root() {
    // The live shape: a second, backend-managed source scans the same saves
    // folder and uses it as its ROM root. Its ROM index is not empty (an
    // emulator's own .bin files live there) but holds no PS1 ROM, so a missing
    // ROM in it says nothing about the card. The card's ROM is in the first
    // source's ROM tree, and the card must still be offered.
    let mut bytes = vec![0x00u8; 131072];
    bytes[..2].copy_from_slice(b"MC");
    for frame in 0..16usize {
        let start = frame * 128;
        let checksum = bytes[start..start + 127]
            .iter()
            .fold(0u8, |acc, value| acc ^ value);
        bytes[start + 127] = checksum;
    }

    let server = MockServer::start();
    let _token = server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_tree","expiresInDays":7}"#);
    });
    let _me = server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"tree@example.com"}}"#);
    });
    let _save_latest = server.mock(|when, then| {
        when.method(GET).path("/save/latest");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"exists":false}"#);
    });
    let _conflict_check = server.mock(|when, then| {
        when.method(GET).path("/conflicts/check");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"exists":false}"#);
    });
    let upload = server.mock(|when, then| {
        when.method(POST).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"save":{"id":"save-tekken","sha256":"stored","version":1}}"#);
    });

    let tmp = tempfile::tempdir().unwrap();
    let saves = tmp.path().join("saves");
    let roms = tmp.path().join("roms");
    let state_dir = tmp.path().join("state");
    fs::create_dir_all(saves.join("psx")).unwrap();
    fs::create_dir_all(saves.join("wii/dolphin/shared2")).unwrap();
    fs::create_dir_all(roms.join("psx")).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    fs::write(roms.join("psx/Tekken 3 (USA).chd"), b"rom").unwrap();
    // A file with a ROM extension in the saves folder: the second source's
    // ROM index is non-empty because of it.
    fs::write(saves.join("wii/dolphin/shared2/misc.bin"), b"dolphin").unwrap();
    fs::write(saves.join("psx/Tekken 3 (USA).srm"), &bytes).unwrap();

    let config = tmp.path().join("config.ini");
    fs::write(
        &config,
        format!(
            "URL=\"127.0.0.1\"\nPORT=\"{port}\"\nROOT=\"{saves}\"\nSTATE_DIR=\"{state}\"\nWATCH=\"false\"\nWATCH_INTERVAL=\"1\"\n\n[source.retrodeck]\nLABEL=\"RetroDeck\"\nKIND=\"retroarch\"\nPROFILE=\"retroarch\"\nSAVE_PATH=\"{saves}\"\nROM_PATH=\"{roms}\"\nRECURSIVE=\"true\"\nSYSTEMS=\"psx\"\nMANAGED=\"false\"\nORIGIN=\"manual\"\n\n[source.deck_default]\nLABEL=\"Default\"\nKIND=\"retroarch\"\nPROFILE=\"generic\"\nSAVE_PATH=\"{saves}\"\nROM_PATH=\"{saves}\"\nRECURSIVE=\"true\"\nSYSTEMS=\"psx\"\nMANAGED=\"true\"\nORIGIN=\"autoscan-known\"\n",
            port = server.port(),
            saves = saves.display(),
            roms = roms.display(),
            state = state_dir.display(),
        ),
    )
    .unwrap();

    wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("login")
        .arg("--email")
        .arg("tree@example.com")
        .arg("--app-password")
        .arg("pw")
        .assert()
        .success();
    let output = wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("sync")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !stderr.contains("named after no ROM"),
        "a source whose ROM root is its save root refused a card whose ROM is in another source's ROM tree:\n{stderr}"
    );
    // The card has to reach the server, not just escape the refusal. Both
    // sources scan the one saves folder and the mock never reports the save as
    // held, so each pass offers it once: two uploads, and the summary counts
    // both. Before the fix the second source refused it instead.
    assert_eq!(
        upload.calls(),
        2,
        "the card was not uploaded by both sources:\n{stdout}\n{stderr}"
    );
    assert!(
        stdout.contains(" uploaded=2 "),
        "the summary does not count both uploads:\n{stdout}\n{stderr}"
    );
}

// The ROM root is dropped before anything reports on it, so a root that is the
// source's own save directory is never also announced as missing.
#[test]
fn a_save_root_used_as_a_rom_root_is_dropped_without_a_missing_warning() {
    let server = MockServer::start();
    let _token = server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_root","expiresInDays":7}"#);
    });
    let _me = server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"root@example.com"}}"#);
    });
    let _list = server.mock(|when, then| {
        when.method(GET).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"total":0,"limit":100,"offset":0,"saves":[]}"#);
    });

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    fs::create_dir_all(&state_dir).unwrap();
    fs::create_dir_all(&root).unwrap();
    // The directory the source names as both roots does not exist: the arcade's
    // shape, and the one where a reprimand and a drop could both appear.
    let config = tmp.path().join("config-root.ini");
    fs::write(
        &config,
        format!(
            "URL=\"127.0.0.1\"\nPORT=\"{}\"\nROOT=\"{}\"\nSTATE_DIR=\"{}\"\nWATCH=\"false\"\nWATCH_INTERVAL=\"1\"\n\n[source.arcade]\nLABEL=\"Arcade\"\nKIND=\"retroarch\"\nPROFILE=\"retroarch\"\nSAVE_PATH=\"{}/saves\"\nROM_PATH=\"{}/./saves\"\nRECURSIVE=\"true\"\nSYSTEMS=\"genesis\"\n",
            server.port(),
            root.display(),
            state_dir.display(),
            root.display(),
            root.display()
        ),
    )
    .unwrap();

    wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("login")
        .arg("--email")
        .arg("root@example.com")
        .arg("--app-password")
        .arg("pw")
        .assert()
        .success();
    let output = wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("sync")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("Ignoring ROM root"),
        "the ROM root was not dropped:\n{stderr}"
    );
    // The line names the source, so two sources' lines for one path can be
    // told apart in a device log.
    assert!(
        stderr.contains("in source 'Arcade'"),
        "the dropped-root line does not name its source:\n{stderr}"
    );
    assert!(
        !stderr.contains("does not exist"),
        "the dropped root was also reported as missing:\n{stderr}"
    );
}

#[test]
fn a_ds_save_names_its_runtime_profile_on_the_latest_check() {
    // The server refuses a latest check for a projection-capable system that
    // names no runtime profile ("runtimeProfile is required for
    // projection-capable helper latest checks"). DS is one: a RetroArch DS
    // save has to ask for its melonDS profile.
    let server = MockServer::start();
    let _token = server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_nds","expiresInDays":7}"#);
    });
    let _me = server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"nds@example.com"}}"#);
    });
    let latest_with_profile = server.mock(|when, then| {
        when.method(GET)
            .path("/save/latest")
            .query_param("runtimeProfile", "nds/retroarch-melonds");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"exists":false}"#);
    });
    let _latest_without = server.mock(|when, then| {
        when.method(GET).path("/save/latest");
        then.status(400)
            .header("content-type", "application/json")
            .body(r#"{"error":"Bad Request","message":"runtimeProfile is required for projection-capable helper latest checks","statusCode":400}"#);
    });
    let _conflict_check = server.mock(|when, then| {
        when.method(GET).path("/conflicts/check");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"exists":false}"#);
    });
    let _upload = server.mock(|when, then| {
        when.method(POST).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"save":{"id":"save-nds","sha256":"stored","version":1}}"#);
    });

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    fs::create_dir_all(root.join("saves/nds")).unwrap();
    fs::create_dir_all(root.join("roms/nds")).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    fs::write(root.join("roms/nds/Mario Kart DS (USA).nds"), b"rom").unwrap();
    let mut save = vec![0x00u8; 512];
    save[0] = 0x4d;
    save[1] = 0x4b;
    fs::write(root.join("saves/nds/Mario Kart DS (USA).sav"), &save).unwrap();
    let config = writeback_config_for(&tmp, &server, &root, &state_dir, "nds");

    wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("login")
        .arg("--email")
        .arg("nds@example.com")
        .arg("--app-password")
        .arg("pw")
        .assert()
        .success();
    let output = wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("sync")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        latest_with_profile.calls() >= 1,
        "the DS latest check named no runtime profile, so the server refuses it:\n{stdout}\n{stderr}"
    );
    assert!(
        !stderr.contains("runtimeProfile is required"),
        "the server refused a DS check for its missing profile:\n{stderr}"
    );
}

#[test]
fn a_permanently_refused_save_is_not_uploaded_again() {
    let server = MockServer::start();
    let _token = server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_reject","expiresInDays":7}"#);
    });
    let _me = server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"reject@example.com"}}"#);
    });
    let _rom_lookup = server.mock(|when, then| {
        when.method(GET).path("/rom/lookup");
        then.status(200)
            .header("content-type", "application/json")
            .body(
                r#"{"success":true,"count":1,"rom":{"sha1":"reject123","md5":"md5v","fileName":"wario.sav"}}"#,
            );
    });
    let _save_latest = server.mock(|when, then| {
        when.method(GET).path("/save/latest");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"exists":false,"sha256":null,"version":null,"id":null}"#);
    });
    let upload = server.mock(|when, then| {
        when.method(POST).path("/saves");
        then.status(422)
            .header("content-type", "application/json")
            .body(r#"{"success":false,"reason":"snes raw save payload is blank"}"#);
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
        .arg("reject@example.com")
        .arg("--app-password")
        .arg("pw")
        .assert()
        .success();

    Command::cargo_bin("sgm-steamdeck-helper")
        .unwrap()
        .arg("--config")
        .arg(&config)
        .arg("sync")
        .assert()
        .success();

    let output = Command::cargo_bin("sgm-steamdeck-helper")
        .unwrap()
        .arg("--config")
        .arg(&config)
        .arg("sync")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success(), "second sync failed: {stdout}");
    assert!(
        stdout.contains("rejected_permanent=1"),
        "the refused save was not counted as permanently rejected: {stdout}"
    );
    assert!(
        stdout.contains("errors=0"),
        "the refused save was retried instead of skipped: {stdout}"
    );
    assert_eq!(upload.calls(), 1);
    let state = fs::read_to_string(state_dir.join("sync_state.json")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&state).unwrap();
    let recorded = parsed["terminal_rejections"]
        .as_object()
        .and_then(|records| records.values().next())
        .and_then(|record| record["build_id"].as_str())
        .unwrap_or("");
    assert!(
        !recorded.is_empty(),
        "the refusal record does not name the build that wrote it: {state}"
    );
}

/// Seeds a refusal for the save at its current bytes, recorded by `version`,
/// then syncs once and returns how many uploads the server saw.
fn uploads_after_a_refusal_recorded_by(version: Option<&str>) -> (usize, String) {
    let server = MockServer::start();
    let _token = server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_rev","expiresInDays":7}"#);
    });
    let _me = server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"rev@example.com"}}"#);
    });
    let _rom_lookup = server.mock(|when, then| {
        when.method(GET).path("/rom/lookup");
        then.status(200)
            .header("content-type", "application/json")
            .body(
                r#"{"success":true,"count":1,"rom":{"sha1":"rev123","md5":"md5v","fileName":"wario.sav"}}"#,
            );
    });
    let _save_latest = server.mock(|when, then| {
        when.method(GET).path("/save/latest");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"exists":false,"sha256":null,"version":null,"id":null}"#);
    });
    let upload = server.mock(|when, then| {
        when.method(POST).path("/saves");
        then.status(422)
            .header("content-type", "application/json")
            .body(r#"{"success":false,"reason":"snes raw save payload is blank"}"#);
    });

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    fs::create_dir_all(root.join("Nintendo/Game Boy")).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    let bytes = vec![0x00u8; 32768];
    let save = root.join("Nintendo/Game Boy/wario.sav");
    fs::write(&save, &bytes).unwrap();
    let sha = {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        format!("{:x}", hasher.finalize())
    };
    // `helper_version` is the field's old name; records on devices carry it, so
    // seeding under it also proves the alias.
    let version_field = version
        .map(|value| format!(r#","helper_version":"{value}""#))
        .unwrap_or_default();
    fs::write(
        state_dir.join("sync_state.json"),
        format!(
            r#"{{"entries":{{}},"terminal_rejections":{{"{}":{{"sha256":"{sha}","status":422,"reason":"POST /saves: status=422","recorded_at":"2026-11-04T00:00:00Z"{version_field}}}}}}}"#,
            save.display()
        ),
    )
    .unwrap();
    let config = write_config(&tmp, &server, &root, &state_dir);
    wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("login")
        .arg("--email")
        .arg("rev@example.com")
        .arg("--app-password")
        .arg("pw")
        .assert()
        .success();
    let output = wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("sync")
        .output()
        .unwrap();
    (
        upload.calls(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

// A refusal is honoured only by the release that recorded it. One an older
// release recorded is asked again once, because the fix that makes a save
// acceptable usually ships in the helper or the server.
#[test]
fn a_refusal_another_build_recorded_is_asked_again_once() {
    for version in [
        Some("0.4.16+0123456789ab"),
        Some(env!("CARGO_PKG_VERSION")),
        None,
    ] {
        let (uploads, stderr) = uploads_after_a_refusal_recorded_by(version);
        assert_eq!(
            uploads, 1,
            "a refusal recorded by {version:?} was not asked again:\n{stderr}"
        );
        assert!(
            stderr.contains("Re-probing"),
            "the re-probe was not named ({version:?}):\n{stderr}"
        );
    }
}

#[test]
fn a_refusal_this_build_recorded_is_not_asked_again() {
    let (uploads, stderr) = uploads_after_a_refusal_recorded_by(Some(env!("SGM_BUILD_ID")));
    assert_eq!(
        uploads, 0,
        "a refusal this release recorded was sent again:\n{stderr}"
    );
}

// A cloud save the scanner cannot read back is restored again on every pass:
// it is invisible locally, so it always looks cloud-only. A Sega cart save at
// a non-power-of-two size (a RetroArch Mega Drive save is 604 bytes) was that
// case, and a device rewrote it every sync.
#[test]
fn a_restored_sega_save_at_its_cart_size_is_not_restored_again() {
    let mut payload = vec![0x00u8; 604];
    for (index, byte) in payload.iter_mut().enumerate() {
        *byte = (index % 251) as u8;
    }
    let sha = {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(&payload);
        format!("{:x}", hasher.finalize())
    };
    let server = MockServer::start();
    let _token = server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_md","expiresInDays":7}"#);
    });
    let _me = server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"md@example.com"}}"#);
    });
    let list = format!(
        r#"{{"success":true,"total":1,"limit":100,"offset":0,"saves":[{{"id":"save-s3","filename":"Sonic The Hedgehog 3 (USA).srm","displayTitle":"Sonic The Hedgehog 3","systemSlug":"genesis","romSha1":"rom-s3","sha256":"{sha}","version":1,"latestSizeBytes":604}}]}}"#
    );
    let _list = server.mock(|when, then| {
        when.method(GET).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(list);
    });
    let latest = format!(
        r#"{{"success":true,"exists":true,"sha256":"{sha}","version":1,"id":"save-s3","latestSizeBytes":604}}"#
    );
    let _latest = server.mock(|when, then| {
        when.method(GET).path("/save/latest");
        then.status(200)
            .header("content-type", "application/json")
            .body(latest);
    });
    let _rom = server.mock(|when, then| {
        when.method(GET).path("/rom/lookup");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"count":1,"rom":{"sha1":"rom-s3","md5":"md5-s3"}}"#);
    });
    let payload_body = payload.clone();
    let download = server.mock(|when, then| {
        when.method(GET).path("/saves/download");
        then.status(200).body(payload_body);
    });

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    fs::create_dir_all(root.join("saves/megadrive")).unwrap();
    fs::create_dir_all(root.join("roms/megadrive")).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    fs::write(
        root.join("roms/megadrive/Sonic The Hedgehog 3 (USA).md"),
        b"rom",
    )
    .unwrap();
    let config = writeback_config_for(&tmp, &server, &root, &state_dir, "genesis");
    wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("login")
        .arg("--email")
        .arg("md@example.com")
        .arg("--app-password")
        .arg("pw")
        .assert()
        .success();
    let mut logs = String::new();
    for _ in 0..2 {
        let output = wb_helper()
            .arg("--config")
            .arg(&config)
            .arg("sync")
            .arg("--verbose")
            .output()
            .unwrap();
        logs.push_str(&String::from_utf8_lossy(&output.stdout));
        logs.push_str(&String::from_utf8_lossy(&output.stderr));
    }

    let restored = root.join("saves/megadrive/Sonic The Hedgehog 3 (USA).srm");
    assert_eq!(
        fs::read(&restored).ok(),
        Some(payload),
        "the save was not restored:\n{logs}"
    );
    assert_eq!(
        download.calls(),
        1,
        "the restored save was not read back, so the second pass restored it again:\n{logs}"
    );
}

/// Syncs once against a server whose record for the local save differs, and
/// whose download returns `payload`; returns the local file after, and the log.
fn local_after_a_download_of(
    system: &str,
    dir: &str,
    file: &str,
    local: &[u8],
    payload: &[u8],
) -> (Vec<u8>, String, Vec<String>) {
    let server = MockServer::start();
    let _token = server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_size","expiresInDays":7}"#);
    });
    let _me = server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"size@example.com"}}"#);
    });
    let _rom = server.mock(|when, then| {
        when.method(GET).path("/rom/lookup");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"count":1,"rom":{"sha1":"rom-size","md5":"md5-size"}}"#);
    });
    // The server holds a different save than the device has, and this device has
    // no sync history for it: the restore case, which downloads.
    let latest = format!(
        r#"{{"success":true,"exists":true,"sha256":"server-other","version":3,"id":"save-size","latestSizeBytes":{}}}"#,
        payload.len()
    );
    let _latest = server.mock(|when, then| {
        when.method(GET).path("/save/latest");
        then.status(200)
            .header("content-type", "application/json")
            .body(latest);
    });
    let _conflict = server.mock(|when, then| {
        when.method(GET).path("/conflicts/check");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"exists":false}"#);
    });
    let body = payload.to_vec();
    let _download = server.mock(|when, then| {
        when.method(GET).path("/saves/download");
        then.status(200).body(body);
    });

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    fs::create_dir_all(root.join(format!("saves/{dir}"))).unwrap();
    fs::create_dir_all(root.join(format!("roms/{dir}"))).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    let path = root.join(format!("saves/{dir}/{file}"));
    fs::write(&path, local).unwrap();
    let config = writeback_config_for(&tmp, &server, &root, &state_dir, system);
    wb_helper()
        .arg("--config")
        .arg(&config)
        .args([
            "login",
            "--email",
            "size@example.com",
            "--app-password",
            "pw",
        ])
        .assert()
        .success();
    let output = wb_helper()
        .arg("--config")
        .arg(&config)
        .args(["sync", "--verbose"])
        .output()
        .unwrap();
    let log = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let after = fs::read(&path).unwrap_or_default();
    let mut names: Vec<String> = fs::read_dir(root.join(format!("saves/{dir}")))
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    (after, log, names)
}

// A download never changes the size of the save it would replace. An N64
// RetroArch projection (296,960 bytes) written over a 32 KiB SRAM save loses the
// save the game reads, and a device then rewrites it on every pass.
#[test]
fn a_download_that_would_change_an_n64_saves_size_is_refused() {
    let local: Vec<u8> = (0..32_768u32).map(|i| (i % 241) as u8 | 1).collect();
    let projection = vec![0x5au8; 296_960];
    let (after, log, names) = local_after_a_download_of(
        "n64",
        "n64",
        "1080 Snowboarding (Japan, USA) (En,Ja).sra",
        &local,
        &projection,
    );
    assert_eq!(after, local, "the local save was overwritten:\n{log}");
    assert!(
        log.contains("refused_size_mismatch=1"),
        "the refused download was not counted:\n{log}"
    );
    assert_eq!(
        names,
        vec!["1080 Snowboarding (Japan, USA) (En,Ja).sra".to_string()],
        "a projection was written beside the save:
{log}"
    );
}

#[test]
fn a_download_that_would_change_a_master_system_saves_size_is_refused() {
    let local: Vec<u8> = (0..8_192u32).map(|i| (i % 239) as u8 | 1).collect();
    let padded = vec![0x33u8; 65_536];
    let (after, log, _) = local_after_a_download_of(
        "master-system",
        "mastersystem",
        "Phantasy Star (USA, Europe).srm",
        &local,
        &padded,
    );
    assert_eq!(after, local, "the local save was overwritten:\n{log}");
    assert!(
        log.contains("refused_size_mismatch=1"),
        "the refused download was not counted:\n{log}"
    );
}

// Padding is not a size difference. The same GBA save written by a core that pads
// its SRAM to 128 KiB with 0xFF and stored by the server at 32 KiB is one save:
// refusing it as "a different save" leaves the two devices disagreeing forever.
#[test]
fn a_save_that_differs_only_by_ff_padding_is_in_sync_not_refused() {
    let save: Vec<u8> = (0..32_768u32).map(|i| (i % 233) as u8 | 1).collect();
    let mut padded = save.clone();
    padded.resize(131_072, 0xFF);
    let (after, log, _) = local_after_a_download_of(
        "gba",
        "gba",
        "Kirby - Nightmare in Dream Land (USA).srm",
        &padded,
        &save,
    );
    assert_eq!(after, padded, "the padded local save was rewritten:\n{log}");
    assert!(
        log.contains("refused_size_mismatch=0"),
        "padding was refused as a different save:\n{log}"
    );
    assert!(
        log.contains("the difference is 0xFF padding"),
        "the padding decision was not named:\n{log}"
    );
    assert!(
        log.contains("in_sync=1"),
        "the padded save was not counted in sync:\n{log}"
    );
}

// Only 0xFF padding is forgiven: a longer local file whose tail holds data is a
// different save and stays refused.
#[test]
fn a_longer_save_whose_tail_is_not_ff_padding_is_still_refused() {
    let save: Vec<u8> = (0..32_768u32).map(|i| (i % 233) as u8 | 1).collect();
    let mut longer = save.clone();
    longer.resize(131_072, 0x5A);
    let (after, log, _) = local_after_a_download_of(
        "gba",
        "gba",
        "Kirby - Nightmare in Dream Land (USA).srm",
        &longer,
        &save,
    );
    assert_eq!(after, longer, "the local save was overwritten:\n{log}");
    assert!(
        log.contains("refused_size_mismatch=1"),
        "a real size difference was not refused:\n{log}"
    );
}

// A DeSmuME .dsv is the raw backup plus a 122-byte footer, so its own length is
// not a power of two and it is not the size the server's raw check knows. The
// container decides the profile: .dsv means nds/desmume, and the server strips
// the footer for that profile only.
#[test]
fn a_dsv_uploads_as_the_desmume_profile() {
    let server = MockServer::start();
    let _token = server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_dsv","expiresInDays":7}"#);
    });
    let _me = server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"dsv@example.com"}}"#);
    });
    let _rom_lookup = server.mock(|when, then| {
        when.method(GET).path("/rom/lookup");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"count":1,"rom":{"sha1":"dsv123","md5":"md5v","fileName":"mkds.rom"}}"#);
    });
    let _save_latest = server.mock(|when, then| {
        when.method(GET).path("/save/latest");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"exists":false,"sha256":null,"version":null,"id":null}"#);
    });
    let upload = server.mock(|when, then| {
        when.method(POST)
            .path("/saves")
            .body_includes("nds/desmume");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"save":{"id":"save-dsv","sha256":"sha-dsv","version":1}}"#);
    });

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    fs::create_dir_all(root.join("saves/nds")).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    fs::write(
        root.join("saves/nds/Mario Kart DS (USA).dsv"),
        vec![0xC5u8; 262144 + 122],
    )
    .unwrap();
    let config = tmp.path().join("config-nds.ini");
    fs::write(
        &config,
        format!(
            "URL=\"127.0.0.1\"\nPORT=\"{}\"\nROOT=\"{}\"\nSTATE_DIR=\"{}\"\nWATCH=\"false\"\nWATCH_INTERVAL=\"1\"\n\n[source.nds]\nLABEL=\"nds\"\nKIND=\"mister-fpga\"\nPROFILE=\"mister\"\nSAVE_PATH=\"{}\"\nROM_PATH=\"{}\"\nRECURSIVE=\"true\"\nSYSTEMS=\"nds\"\n",
            server.port(),
            root.display(),
            state_dir.display(),
            root.join("saves").display(),
            root.join("roms").display()
        ),
    )
    .unwrap();

    Command::cargo_bin("sgm-steamdeck-helper")
        .unwrap()
        .arg("--config")
        .arg(&config)
        .arg("login")
        .arg("--email")
        .arg("dsv@example.com")
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
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(
        upload.calls(),
        1,
        "a .dsv was not uploaded with the DeSmuME profile: {stdout}\nSTDERR: {stderr}"
    );
}

fn romroot_helper() -> Command {
    Command::cargo_bin("sgm-steamdeck-helper").unwrap()
}

// A ROM root that does not exist indexes nothing, and every save then looks like
// a title the server has never seen. The run reports each root and what came out
// of it, so the absence is visible instead of inferred from a missing hash.
#[test]
fn a_missing_rom_root_is_reported() {
    let server = MockServer::start();
    let _token = server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_romroot","expiresInDays":7}"#);
    });
    let _me = server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"romroot@example.com"}}"#);
    });

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    fs::create_dir_all(root.join("saves/genesis")).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    let config = tmp.path().join("config-romroot.ini");
    fs::write(
        &config,
        format!(
            "URL=\"127.0.0.1\"\nPORT=\"{}\"\nROOT=\"{}\"\nSTATE_DIR=\"{}\"\nWATCH=\"false\"\nWATCH_INTERVAL=\"1\"\n\n[source.genesis]\nLABEL=\"genesis\"\nKIND=\"retroarch\"\nPROFILE=\"retroarch\"\nSAVE_PATH=\"{}\"\nROM_PATH=\"{}\"\nRECURSIVE=\"true\"\nSYSTEMS=\"genesis\"\n",
            server.port(),
            root.display(),
            state_dir.display(),
            root.join("saves").display(),
            root.join("not-mounted").display()
        ),
    )
    .unwrap();

    romroot_helper()
        .arg("--config")
        .arg(&config)
        .arg("login")
        .arg("--email")
        .arg("romroot@example.com")
        .arg("--app-password")
        .arg("pw")
        .assert()
        .success();
    let output = romroot_helper()
        .arg("--config")
        .arg(&config)
        .arg("sync")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("does not exist"),
        "a missing ROM root was not reported:\n{stderr}"
    );
    assert!(
        stderr.contains("exists=false, indexed=0"),
        "the ROM root report does not say what it found:\n{stderr}"
    );
}

#[test]
fn a_save_the_server_holds_on_another_card_is_uploaded_even_when_local_state_agrees() {
    // A played PS1 card: real save blocks, the shape the live per-game card has.
    let bytes = build_valid_ps1_memcard();
    let sha = {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        format!("{:x}", hasher.finalize())
    };

    let server = MockServer::start();
    let _token = server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_slot","expiresInDays":7}"#);
    });
    let _me = server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"slot@example.com"}}"#);
    });
    let latest_body = format!(
        r#"{{"success":true,"exists":true,"sha256":"{}","version":4,"id":"save-tekken","latestSizeBytes":131072,"cardSlot":"Memory Card 1"}}"#,
        sha
    );
    let _save_latest = server.mock(|when, then| {
        when.method(GET).path("/save/latest");
        then.status(200)
            .header("content-type", "application/json")
            .body(latest_body);
    });
    let _conflict_check = server.mock(|when, then| {
        when.method(GET).path("/conflicts/check");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"exists":false}"#);
    });
    let upload = server.mock(|when, then| {
        when.method(POST).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"save":{"id":"save-tekken","sha256":"stored","version":5}}"#);
    });

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    fs::create_dir_all(root.join("saves/psx")).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    let local = root.join("saves/psx/Tekken 3 (USA).srm");
    fs::write(&local, &bytes).unwrap();
    // The server re-homed the save onto the shared card (another device, a
    // repair); this device's own state already names the per-game card it
    // offers, so only the card the server reports shows the move.
    slot_state_synced(&state_dir, &local, "psx", "Game: Tekken 3 (USA)", &sha);
    let config = writeback_config_for(&tmp, &server, &root, &state_dir, "psx");

    wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("login")
        .arg("--email")
        .arg("slot@example.com")
        .arg("--app-password")
        .arg("pw")
        .assert()
        .success();
    let output = wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("sync")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(
        upload.calls(),
        1,
        "the server reported another card and the save was not re-offered, so it stays on the card the server named:\n{stderr}"
    );
}

fn writeback_config_for(
    tmp: &tempfile::TempDir,
    server: &MockServer,
    root: &std::path::Path,
    state_dir: &std::path::Path,
    systems: &str,
) -> std::path::PathBuf {
    let config = tmp.path().join("config-writeback.ini");
    fs::write(
        &config,
        format!(
            "URL=\"127.0.0.1\"\nPORT=\"{}\"\nROOT=\"{}\"\nSTATE_DIR=\"{}\"\nWATCH=\"false\"\nWATCH_INTERVAL=\"1\"\n\n[source.retroarch]\nLABEL=\"RetroArch\"\nKIND=\"retroarch\"\nPROFILE=\"retroarch\"\nSAVE_PATH=\"{}\"\nROM_PATH=\"{}\"\nRECURSIVE=\"true\"\nSYSTEMS=\"{}\"\n",
            server.port(),
            root.display(),
            state_dir.display(),
            root.join("saves").display(),
            root.join("roms").display(),
            systems
        ),
    )
    .unwrap();
    config
}

#[test]
fn a_success_body_without_a_version_is_still_a_stored_save() {
    // The server's success body is {"success":true,"save":{"id","sha256"}} --
    // no version. Reading a missing version as "nothing was stored" reported
    // every such upload as accepted-and-discarded while the record was on disk.
    let server = MockServer::start();
    let _token = server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_nov","expiresInDays":7}"#);
    });
    let _me = server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"noversion@example.com"}}"#);
    });
    let _rom_lookup = server.mock(|when, then| {
        when.method(GET).path("/rom/lookup");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"count":1,"rom":{"sha1":"nov123","md5":"md5v","fileName":"wario.sav"}}"#);
    });
    let _save_latest = server.mock(|when, then| {
        when.method(GET).path("/save/latest");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"exists":false,"sha256":null,"version":null,"id":null}"#);
    });
    let _config_sync = server.mock(|when, then| {
        when.method(POST).path("/helpers/config/sync");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"accepted":true}"#);
    });
    let upload = server.mock(|when, then| {
        when.method(POST).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"save":{"id":"save-nov","sha256":"sha-nov"}}"#);
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
        .arg("noversion@example.com")
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
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert_eq!(
        upload.calls(),
        1,
        "the save should be uploaded exactly once"
    );
    assert!(
        stdout.contains("uploaded=1"),
        "a stored save without a version was not counted as uploaded: {stdout}"
    );
    assert!(
        stdout.contains("accepted_not_stored=0"),
        "a stored save without a version was counted as discarded: {stdout}"
    );
}

#[test]
fn a_duplicate_upload_is_counted_as_deduplicated_not_uploaded() {
    // The server answers an upload of bytes it already holds with
    // {"duplicate":true,...} and stores nothing new. Counting that as an upload
    // made every pass report work while the store did not change.
    let server = MockServer::start();
    let _token = server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_nov","expiresInDays":7}"#);
    });
    let _me = server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"noversion@example.com"}}"#);
    });
    let _rom_lookup = server.mock(|when, then| {
        when.method(GET).path("/rom/lookup");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"count":1,"rom":{"sha1":"nov123","md5":"md5v","fileName":"wario.sav"}}"#);
    });
    let _save_latest = server.mock(|when, then| {
        when.method(GET).path("/save/latest");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"exists":false,"sha256":null,"version":null,"id":null}"#);
    });
    let _config_sync = server.mock(|when, then| {
        when.method(POST).path("/helpers/config/sync");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"accepted":true}"#);
    });
    let upload = server.mock(|when, then| {
        when.method(POST).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"duplicate":true,"duplicateDisposition":"ignored-latest","save":{"id":"save-nov","sha256":"sha-nov","version":1}}"#);
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
        .arg("noversion@example.com")
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
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert_eq!(
        upload.calls(),
        1,
        "the save should be uploaded exactly once"
    );
    assert!(
        stdout.contains(" uploaded=0 "),
        "a deduplicated upload was counted as uploaded: {stdout}"
    );
    assert!(
        stdout.contains(" deduplicated=1 "),
        "a deduplicated upload was not counted as deduplicated: {stdout}"
    );
    assert!(
        stdout.contains("accepted_not_stored=0"),
        "a deduplicated upload was counted as discarded: {stdout}"
    );
}

// A 2xx whose body names no save stored nothing. Every upload path has to read
// the body the way the main path does; counting the request as uploaded is how
// a client reports successful uploads while the store never grows.
const NO_SAVE_BODY: &str = r#"{"success":true}"#;

fn summary_count(stdout: &str, key: &str) -> Option<u64> {
    let needle = format!(" {key}=");
    let start = stdout.find(&needle)? + needle.len();
    stdout[start..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .ok()
}

#[test]
fn a_port_save_upload_with_no_save_in_the_body_is_not_counted_as_uploaded() {
    let server = MockServer::start();
    let _token = server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_port","expiresInDays":7}"#);
    });
    let _me = server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"port@example.com"}}"#);
    });
    let _latest = server.mock(|when, then| {
        when.method(GET).path("/save/latest");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"exists":false,"sha256":null,"version":null,"id":null}"#);
    });
    let upload = server.mock(|when, then| {
        when.method(POST).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(NO_SAVE_BODY);
    });

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    let port_save = root.join("ports/OcarinaOfTime/Save/global.sav");
    fs::create_dir_all(port_save.parent().unwrap()).unwrap();
    fs::write(&port_save, vec![0x5au8; 4096]).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    let config = tmp.path().join("config.ini");
    let body = format!(
        "URL=\"127.0.0.1\"\nPORT=\"{}\"\nROOT=\"{}\"\nSTATE_DIR=\"{}\"\nWATCH=\"false\"\nWATCH_INTERVAL=\"1\"\n\n[source.ports]\nLABEL=\"Ports\"\nKIND=\"ports\"\nSAVE_PATH=\"{}\"\n",
        server.port(),
        root.display(),
        state_dir.display(),
        root.join("ports").display()
    );
    fs::write(&config, body).unwrap();
    wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("login")
        .arg("--email")
        .arg("port@example.com")
        .arg("--app-password")
        .arg("pw")
        .assert()
        .success();
    let output = wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("sync")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(upload.calls(), 1, "the port save was not offered: {stderr}");
    assert_eq!(
        summary_count(&stdout, "uploaded"),
        Some(0),
        "a body with no save was counted as an upload:\n{stdout}\n{stderr}"
    );
    assert_eq!(
        summary_count(&stdout, "accepted_not_stored"),
        Some(1),
        "a body with no save was not reported as stored nothing:\n{stdout}\n{stderr}"
    );
}

#[test]
fn a_psp_bundle_upload_with_no_save_in_the_body_is_not_counted_as_uploaded() {
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
    let upload = server.mock(|when, then| {
        when.method(POST)
            .path("/saves")
            .body_includes("name=\"pspSaveDir\"");
        then.status(200)
            .header("content-type", "application/json")
            .body(NO_SAVE_BODY);
    });

    let tmp = tempfile::tempdir().unwrap();
    let saves = tmp.path().join("saves");
    let state_dir = tmp.path().join("state");
    fs::create_dir_all(&state_dir).unwrap();
    write_ppsspp_savedata(&saves, "ULUS10437GameData00");
    let config = tmp.path().join("config.ini");
    let body = format!(
        "URL=\"127.0.0.1\"\nPORT=\"{}\"\nROOT=\"{}\"\nSTATE_DIR=\"{}\"\nWATCH=\"false\"\nWATCH_INTERVAL=\"1\"\nPSP_SAVEDATA_BUNDLES=\"true\"\n\n[source.retrodeck]\nLABEL=\"RetroDECK\"\nKIND=\"retroarch\"\nPROFILE=\"retroarch\"\nSAVE_PATH=\"{}\"\nROM_PATH=\"{}\"\nRECURSIVE=\"true\"\nSYSTEMS=\"psp\"\nCREATE_MISSING_SYSTEM_DIRS=\"false\"\nMANAGED=\"false\"\nORIGIN=\"test\"\n",
        server.port(),
        saves.display(),
        state_dir.display(),
        saves.display(),
        saves.display(),
    );
    fs::write(&config, body).unwrap();
    wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("login")
        .arg("--email")
        .arg("psp@example.com")
        .arg("--app-password")
        .arg("pw")
        .assert()
        .success();
    let output = wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("sync")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(
        upload.calls(),
        1,
        "the PSP bundle was not offered: {stderr}"
    );
    assert_eq!(
        summary_count(&stdout, "uploaded"),
        Some(0),
        "a body with no save was counted as an upload:\n{stdout}\n{stderr}"
    );
    assert_eq!(
        summary_count(&stdout, "accepted_not_stored"),
        Some(1),
        "a body with no save was not reported as stored nothing:\n{stdout}\n{stderr}"
    );
}

/// Which backend call answers that the PSX projection is missing, sending the
/// helper down one of its repair uploads.
#[derive(Clone, Copy)]
enum PsxRepairTrigger {
    ConflictCheck,
    ConflictReport,
    Download,
}

fn run_psx_repair_with_no_save_body(trigger: PsxRepairTrigger) -> (usize, String, String) {
    let bytes = build_valid_ps1_memcard();
    let projection_missing = r#"{"success":false,"error":"playstation projection not found"}"#;
    let server = MockServer::start();
    let _token = server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_psx","expiresInDays":7}"#);
    });
    let _me = server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"psx@example.com"}}"#);
    });
    // The server holds a different card, so the save is neither new nor in sync.
    let _latest = server.mock(|when, then| {
        when.method(GET).path("/save/latest");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"exists":true,"sha256":"server-card","version":2,"id":"save-psx","latestSizeBytes":131072}"#);
    });
    let _check = server.mock(|when, then| {
        when.method(GET).path("/conflicts/check");
        match trigger {
            PsxRepairTrigger::ConflictCheck => {
                then.status(404)
                    .header("content-type", "application/json")
                    .body(projection_missing);
            }
            PsxRepairTrigger::ConflictReport => {
                then.status(200)
                    .header("content-type", "application/json")
                    .body(r#"{"exists":true,"conflictId":"c1","cloudSha256":"server-card"}"#);
            }
            PsxRepairTrigger::Download => {
                then.status(200)
                    .header("content-type", "application/json")
                    .body(r#"{"exists":false}"#);
            }
        }
    });
    let _report = server.mock(|when, then| {
        when.method(POST).path("/conflicts/report");
        then.status(404)
            .header("content-type", "application/json")
            .body(projection_missing);
    });
    let _download = server.mock(|when, then| {
        when.method(GET).path("/saves/download");
        then.status(404)
            .header("content-type", "application/json")
            .body(projection_missing);
    });
    let upload = server.mock(|when, then| {
        when.method(POST).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(NO_SAVE_BODY);
    });

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    fs::create_dir_all(root.join("saves/psx")).unwrap();
    fs::create_dir_all(root.join("roms/psx")).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    fs::write(root.join("roms/psx/Tekken 3 (USA).chd"), b"rom").unwrap();
    let local = root.join("saves/psx/Tekken 3 (USA).srm");
    fs::write(&local, &bytes).unwrap();
    if matches!(trigger, PsxRepairTrigger::Download) {
        // The device last synced the server's card and has not edited its own,
        // so the newer server copy is downloaded -- and that download is what
        // reports the projection missing.
        let state = format!(
            r#"{{"entries": {{"{}": {{"sha256": "{}", "rom_sha1": "rom-sha", "version": 1, "system_slug": "psx", "local_container": "ps1-raw", "adapter_profile": "ps1-raw", "source_kind": "retroarch", "source_name": "RetroArch", "slot_name": "Game: Tekken 3 (USA)", "updated_at": "2026-01-01T00:00:00Z"}}}}}}"#,
            local.display(),
            {
                use sha2::{Digest, Sha256};
                let mut hasher = Sha256::new();
                hasher.update(&bytes);
                format!("{:x}", hasher.finalize())
            }
        );
        fs::write(state_dir.join("sync_state.json"), state).unwrap();
    }
    let config = writeback_config_for(&tmp, &server, &root, &state_dir, "psx");
    wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("login")
        .arg("--email")
        .arg("psx@example.com")
        .arg("--app-password")
        .arg("pw")
        .assert()
        .success();
    let output = wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("sync")
        .arg("--verbose")
        .output()
        .unwrap();
    (
        upload.calls(),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

fn assert_psx_repair_counts_no_save_as_stored_nothing(trigger: PsxRepairTrigger, path: &str) {
    let (calls, stdout, stderr) = run_psx_repair_with_no_save_body(trigger);
    assert!(
        stderr.contains("uploading local card as repair"),
        "the {path} repair path was not taken:\n{stderr}"
    );
    assert_eq!(
        calls, 1,
        "the repair upload was not sent ({path}):\n{stderr}"
    );
    assert_eq!(
        summary_count(&stdout, "uploaded"),
        Some(0),
        "a repair upload with no save in the body was counted as an upload ({path}):\n{stdout}\n{stderr}"
    );
    assert_eq!(
        summary_count(&stdout, "accepted_not_stored"),
        Some(1),
        "a repair upload with no save in the body was not reported as stored nothing ({path}):\n{stdout}\n{stderr}"
    );
}

#[test]
fn a_psx_repair_after_a_missing_projection_on_conflict_check_reads_the_body() {
    assert_psx_repair_counts_no_save_as_stored_nothing(
        PsxRepairTrigger::ConflictCheck,
        "conflict check",
    );
}

#[test]
fn a_psx_repair_after_a_missing_projection_on_conflict_report_reads_the_body() {
    assert_psx_repair_counts_no_save_as_stored_nothing(
        PsxRepairTrigger::ConflictReport,
        "conflict report",
    );
}

#[test]
fn a_psx_repair_after_a_missing_projection_on_download_reads_the_body() {
    assert_psx_repair_counts_no_save_as_stored_nothing(PsxRepairTrigger::Download, "download");
}
