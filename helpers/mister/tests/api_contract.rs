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

    Command::cargo_bin("sgm-mister-helper")
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

    Command::cargo_bin("sgm-mister-helper")
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

    Command::cargo_bin("sgm-mister-helper")
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

    let output = Command::cargo_bin("sgm-mister-helper")
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
        stdout.contains("uploaded=0 downloaded=0"),
        "dry run counted a simulation as work done: {stdout}"
    );
    assert!(
        stdout.contains(
            "skipped=1 rejected_n64_controller_pak=0 accepted_not_stored=0 \
             errors=0 would_upload=1 would_download=0"
        ),
        "a skipped save was counted as a simulated upload, or the simulation was missed: {stdout}"
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

    Command::cargo_bin("sgm-mister-helper")
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

    Command::cargo_bin("sgm-mister-helper")
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

    Command::cargo_bin("sgm-mister-helper")
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

    Command::cargo_bin("sgm-mister-helper")
        .unwrap()
        .arg("--config")
        .arg(config)
        .arg("sync")
        .assert()
        .success();

    assert!(conflict_report.calls() >= 1);
}

// The arcade's shape: RetroArch writes N64 saves into its core folder
// (SAVE_SUBDIR_N64), and an older helper left a copy in the system-named one.
// The core folder's copy is the live save. It must be the one that syncs:
// if the stray is chosen as the title's preferred variant, the live copy is
// skipped as the lesser one, the stray is refused as a stray, and the
// restore pass rewrites the live file on every run.
#[test]
fn the_live_copy_syncs_when_a_stray_shares_its_name() {
    let server = MockServer::start();
    let _token = server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_live64","expiresInDays":7}"#);
    });
    let _me = server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"live64@example.com"}}"#);
    });
    let _list = server.mock(|when, then| {
        when.method(GET).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"total":0,"limit":100,"offset":0,"saves":[]}"#);
    });
    // No ROM is local, so the ROM key comes from the server's lookup.
    let _lookup = server.mock(|when, then| {
        when.method(GET).path("/rom/lookup");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"count":1,"rom":{"sha1":"sm64-sha1","md5":"sm64-md5"}}"#);
    });
    let latest = server.mock(|when, then| {
        when.method(GET).path("/save/latest");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"exists":false,"sha256":null,"version":null,"id":null}"#);
    });
    let upload = server.mock(|when, then| {
        when.method(POST).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"save":{"id":"save-live64","sha256":"sha-live64"}}"#);
    });

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    let core = root.join("saves/Mupen64Plus-Next OpenGL");
    let stray_dir = root.join("saves/n64");
    fs::create_dir_all(&core).unwrap();
    fs::create_dir_all(&stray_dir).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    // A RetroArch N64 bundle: 296,960 bytes (EEPROM + SRAM + FlashRAM + paks).
    let bundle = |seed: u8| -> Vec<u8> {
        (0..296_960u32)
            .map(|index| ((index as u8).wrapping_mul(31).wrapping_add(seed)) | 0x01)
            .collect()
    };
    let live_copy = core.join("Super Mario 64 (USA).srm");
    fs::write(&live_copy, bundle(7)).unwrap();
    fs::write(stray_dir.join("Super Mario 64 (USA).srm"), bundle(19)).unwrap();
    // No ROM in ROM_PATH, as on the arcade (its ROMs are LaunchBox views):
    // only the system-named folder can say the file is an N64 save.
    let config = live_directory_config(
        &tmp,
        &server,
        &root,
        &state_dir,
        "SAVE_SUBDIR_N64=\"Mupen64Plus-Next OpenGL\"",
    );
    let ini = fs::read_to_string(&config)
        .unwrap()
        .replace("SYSTEMS=\"snes\"", "SYSTEMS=\"n64\"");
    fs::write(&config, ini).unwrap();
    wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("login")
        .arg("--email")
        .arg("live64@example.com")
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
    assert!(output.status.success(), "sync failed:\n{stderr}");
    assert!(
        latest.calls() >= 1 && upload.calls() == 1,
        "the live copy was never synced (latest={}, upload={}):\n{stderr}",
        latest.calls(),
        upload.calls()
    );
    let state = fs::read_to_string(state_dir.join("sync_state.json")).unwrap();
    assert!(
        state.contains("Mupen64Plus-Next OpenGL"),
        "the live copy has no state entry, so the restore pass rewrites it:\n{state}"
    );
}

// A Game Gear save the core keeps in the Genesis folder: the folder names
// another system, so the path alone cannot say the file is this save. The
// helper synced it as game-gear, and the restore pass resolved that very file
// as the target ("it already holds this game-gear save"); it must then count
// as present, not be downloaded and rewritten on every pass.
#[test]
fn a_save_synced_as_its_system_is_not_rewritten_by_the_restore_pass() {
    let server = MockServer::start();
    let _token = server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_gg","expiresInDays":7}"#);
    });
    let _me = server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"gg@example.com"}}"#);
    });
    let _lookup = server.mock(|when, then| {
        when.method(GET).path("/rom/lookup");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"count":1,"rom":{"sha1":"gg-sha1","md5":"gg-md5"}}"#);
    });
    let _latest = server.mock(|when, then| {
        when.method(GET).path("/save/latest");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"exists":false,"sha256":null,"version":null,"id":null}"#);
    });
    let _upload = server.mock(|when, then| {
        when.method(POST).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"save":{"id":"save-gg-up","sha256":"up"}}"#);
    });
    let bytes: Vec<u8> = (0..65_536u32)
        .map(|index| ((index as u8).wrapping_mul(13).wrapping_add(3)) | 0x01)
        .collect();
    // The server's record holds exactly the bytes the device already has.
    let held_sha = {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(&bytes))
    };
    let listing = format!(
        r#"{{"success":true,"total":1,"limit":100,"offset":0,"saves":[{{"id":"save-gg","displayTitle":"Defenders of Oasis","systemSlug":"game-gear","filename":"Defenders of Oasis (USA, Europe).sav","fileSize":65536,"latestSizeBytes":65536,"version":1,"sha256":"{held_sha}"}}]}}"#
    );
    let _list = server.mock(move |when, then| {
        when.method(GET).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(listing);
    });
    let download = server.mock(|when, then| {
        when.method(GET).path("/saves/download");
        then.status(200).body(vec![0x5au8; 65536]);
    });

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    let genesis = root.join("saves/genesis");
    fs::create_dir_all(&genesis).unwrap();
    // As on the arcade, Genesis Plus GX is the declared live directory for
    // genesis and holds that system's copy of the stem, so the genesis-folder
    // file is not uploaded as genesis and keeps its game-gear state entry.
    let live_genesis = root.join("saves/Genesis Plus GX");
    fs::create_dir_all(&live_genesis).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    let held = genesis.join("Defenders of Oasis (USA, Europe).srm");
    fs::write(&held, &bytes).unwrap();
    fs::write(
        live_genesis.join("Defenders of Oasis (USA, Europe).srm"),
        &bytes,
    )
    .unwrap();
    let state = serde_json::json!({
        "entries": {
            held.to_string_lossy(): {
                "sha256": "bb",
                "rom_sha1": "gg-sha1",
                "version": 1,
                "system_slug": "game-gear",
                "local_container": "native",
                "adapter_profile": "identity",
                "source_kind": "retroarch",
                "source_name": "RetroArch",
                "slot_name": "default",
                "updated_at": "2026-11-05T00:00:00Z"
            }
        }
    });
    fs::write(state_dir.join("sync_state.json"), state.to_string()).unwrap();
    let config = live_directory_config(
        &tmp,
        &server,
        &root,
        &state_dir,
        "SAVE_SUBDIR_GENESIS=\"Genesis Plus GX\"",
    );
    let ini = fs::read_to_string(&config)
        .unwrap()
        .replace("SYSTEMS=\"snes\"", "SYSTEMS=\"game-gear,genesis\"");
    fs::write(&config, ini).unwrap();
    wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("login")
        .arg("--email")
        .arg("gg@example.com")
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
    assert!(output.status.success(), "sync failed:\n{stderr}");
    assert_eq!(
        download.calls(),
        0,
        "the file synced as game-gear was downloaded again:\n{stderr}"
    );
    assert_eq!(
        fs::read(&held).unwrap(),
        bytes,
        "the file synced as game-gear was rewritten:\n{stderr}"
    );
}

#[test]
fn sync_accepts_ps1_gme_and_uploads_normalized_payload() {
    let expected_line = "ps-line:psx:mister:memory-card-1";
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
            .body_includes("mister");
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

    Command::cargo_bin("sgm-mister-helper")
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

    Command::cargo_bin("sgm-mister-helper")
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
            .body_includes("\"deviceType\":\"mister\"")
            .body_includes("\"helperName\":\"sgm-mister-helper\"");
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

    Command::cargo_bin("sgm-mister-helper")
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

// A sync that died leaves sync.lock behind. Its owner gone, the next sync takes the lock over and runs;
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
    Command::cargo_bin("sgm-mister-helper")
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
    let dead = Command::cargo_bin("sgm-mister-helper")
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
    let live = Command::cargo_bin("sgm-mister-helper")
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

/// The arcade's RetroArch: mGBA runs Game Boy and GBA, and the operator files
/// Game Boy saves under it with SAVE_SUBDIR_GAMEBOY. The server holds one
/// 8 KiB Game Boy record; 8 KiB is a cartridge size for both systems.
fn mgba_gameboy_harness() -> (
    MockServer,
    tempfile::TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_mgba","expiresInDays":7}"#);
    });
    server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"mgba@example.com"}}"#);
    });
    server.mock(|when, then| {
        when.method(GET).path("/save/latest");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"exists":true,"sha256":"KIRBY_SHA","version":1,"id":"save-kirby"}"#);
    });
    server.mock(|when, then| {
        when.method(GET).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"total":1,"limit":100,"offset":0,"saves":[{"id":"save-kirby","filename":"Kirby's Dream Land 2.srm","displayTitle":"Kirby's Dream Land 2","systemSlug":"gameboy","sha256":"KIRBY_SHA","version":1,"latestSizeBytes":8192}]}"#);
    });
    server.mock(|when, then| {
        when.method(GET)
            .path("/saves/download")
            .query_param("id", "save-kirby");
        then.status(200).body(snes_save_bytes(0x11));
    });

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    fs::create_dir_all(root.join("saves/mGBA")).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    let config = tmp.path().join("config-mgba.ini");
    fs::write(
        &config,
        format!(
            "URL=\"127.0.0.1\"\nPORT=\"{}\"\nROOT=\"{}\"\nSTATE_DIR=\"{}\"\nWATCH=\"false\"\n\n[source.retroarch]\nLABEL=\"RetroArch\"\nKIND=\"retroarch\"\nPROFILE=\"retroarch\"\nSAVE_PATH=\"{}\"\nROM_PATH=\"{}\"\nRECURSIVE=\"true\"\nSYSTEMS=\"gameboy,gba\"\nSAVE_SUBDIR_GAMEBOY=\"mGBA\"\nSAVE_SUBDIR_GBA=\"mGBA\"\n",
            server.port(),
            root.display(),
            state_dir.display(),
            root.join("saves").display(),
            root.join("roms").display()
        ),
    )
    .unwrap();
    wb_helper()
        .arg("--config")
        .arg(&config)
        .arg("login")
        .arg("--email")
        .arg("mgba@example.com")
        .arg("--app-password")
        .arg("pw")
        .assert()
        .success();
    (server, tmp, root.join("saves/mGBA"), config)
}

fn mgba_sync(config: &std::path::Path) -> String {
    let output = wb_helper()
        .arg("--config")
        .arg(config)
        .arg("sync")
        .arg("--verbose")
        .output()
        .unwrap();
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn a_game_boy_save_restored_into_mgba_is_present_on_the_next_pass_and_never_uploaded_as_gba() {
    let (server, _tmp, dir, config) = mgba_gameboy_harness();
    let any_upload = server.mock(|when, then| {
        when.method(POST).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"save":{"id":"save-up","sha256":"x","version":1}}"#);
    });
    let first = mgba_sync(&config);
    let target = dir.join("Kirby's Dream Land 2.srm");
    assert_eq!(
        fs::read(&target).ok(),
        Some(snes_save_bytes(0x11)),
        "the Game Boy record was not restored into mGBA: {first}"
    );
    let written = fs::metadata(&target).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));

    let second = mgba_sync(&config);
    assert!(
        !second.contains("Restored cloud-only"),
        "the second pass restored again: {second}"
    );
    assert_eq!(
        fs::metadata(&target).unwrap().modified().unwrap(),
        written,
        "the second pass rewrote the file: {second}"
    );
    assert!(
        !second.contains("Detected gba savegame"),
        "the restored Game Boy save was read as GBA: {second}"
    );
    any_upload.assert_calls(0);
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
        .arg("--verbose")
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

fn wb_helper() -> Command {
    Command::cargo_bin("sgm-mister-helper").unwrap()
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

    Command::cargo_bin("sgm-mister-helper")
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

    let output = Command::cargo_bin("sgm-mister-helper")
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
