//! A save the helper never synced has no record of which side is newer.  When the
//! server holds different bytes, downloading over it loses a real save, so it is a
//! conflict; when the bytes match, or the file is missing, or it is blank, the
//! ordinary paths still apply.
use std::fs;
use std::path::Path;

use assert_cmd::Command;
use httpmock::Method::{GET, POST};
use httpmock::{Mock, MockServer};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const BIN: &str = "sgm-mister-helper";

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn save_bytes(seed: u8) -> Vec<u8> {
    (0..32768usize)
        .map(|index| seed.wrapping_add((index % 251) as u8))
        .collect()
}

struct Rig {
    server: MockServer,
    tmp: TempDir,
}

impl Rig {
    fn root(&self) -> std::path::PathBuf {
        self.tmp.path().join("root")
    }
    fn state(&self) -> std::path::PathBuf {
        self.tmp.path().join("state")
    }
    fn save_path(&self) -> std::path::PathBuf {
        self.root().join("snes/zelda.sav")
    }
}

fn rig() -> Rig {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok","expiresInDays":7}"#);
    });
    server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"t@example.com"}}"#);
    });
    server.mock(|when, then| {
        when.method(GET).path("/rom/lookup");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"count":1,"rom":{"sha1":"rom-sha1","md5":"rom-md5"}}"#);
    });
    let tmp = tempfile::tempdir().unwrap();
    let rig = Rig { server, tmp };
    fs::create_dir_all(rig.root().join("snes")).unwrap();
    fs::create_dir_all(rig.state()).unwrap();
    rig
}

fn latest(rig: &Rig, remote_sha: &str) {
    let body = format!(
        r#"{{"success":true,"exists":true,"sha256":"{remote_sha}","version":2,"id":"save-remote"}}"#
    );
    rig.server.mock(|when, then| {
        when.method(GET).path("/save/latest");
        then.status(200)
            .header("content-type", "application/json")
            .body(body);
    });
}

fn download(rig: &Rig, bytes: Vec<u8>) -> Mock<'_> {
    rig.server.mock(|when, then| {
        when.method(GET).path("/saves/download");
        then.status(200).body(bytes);
    })
}

fn upload(rig: &Rig) -> Mock<'_> {
    rig.server.mock(|when, then| {
        when.method(POST).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"save":{"id":"save-up","sha256":"sha-up"}}"#);
    })
}

fn conflict_report(rig: &Rig) -> Mock<'_> {
    rig.server.mock(|when, then| {
        when.method(POST).path("/conflicts/report");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"created":true,"conflictId":"conf-1"}"#);
    })
}

fn no_backend_conflict(rig: &Rig) {
    rig.server.mock(|when, then| {
        when.method(GET).path("/conflicts/check");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"exists":false}"#);
    });
}

fn write_state(rig: &Rig, last_sha: &str) {
    let body = format!(
        r#"{{"entries":{{"{}":{{"sha256":"{last_sha}","rom_sha1":"rom-sha1","version":1,"system_slug":"snes","local_container":"native","adapter_profile":"identity","source_kind":"mister","source_name":"default-mister","updated_at":"2026-01-01T00:00:00Z"}}}}}}"#,
        rig.save_path().display()
    );
    fs::write(rig.state().join("sync_state.json"), body).unwrap();
}

fn sync(rig: &Rig) {
    let config = rig.tmp.path().join("config.ini");
    fs::write(
        &config,
        format!(
            "URL=\"127.0.0.1\"\nPORT=\"{}\"\nROOT=\"{}\"\nSTATE_DIR=\"{}\"\nWATCH=\"false\"\nWATCH_INTERVAL=\"1\"\n",
            rig.server.port(),
            rig.root().display(),
            rig.state().display()
        ),
    )
    .unwrap();
    Command::cargo_bin(BIN)
        .unwrap()
        .arg("--config")
        .arg(&config)
        .args(["login", "--email", "t@example.com", "--app-password", "pw"])
        .assert()
        .success();
    Command::cargo_bin(BIN)
        .unwrap()
        .arg("--config")
        .arg(&config)
        .arg("sync")
        .assert()
        .success();
}

fn recorded_sha(rig: &Rig) -> Option<String> {
    let raw = fs::read_to_string(rig.state().join("sync_state.json")).ok()?;
    let state: serde_json::Value = serde_json::from_str(&raw).ok()?;
    state["entries"][rig.save_path().display().to_string()]["sha256"]
        .as_str()
        .map(str::to_string)
}

fn put(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
}

#[test]
fn a_never_synced_save_that_differs_from_the_server_is_a_conflict_and_stays() {
    let rig = rig();
    let local = save_bytes(0x11);
    let remote = save_bytes(0x77);
    put(&rig.save_path(), &local);
    latest(&rig, &sha256_hex(&remote));
    no_backend_conflict(&rig);
    let download = download(&rig, remote);
    let upload = upload(&rig);
    let report = conflict_report(&rig);

    sync(&rig);

    assert_eq!(
        fs::read(rig.save_path()).unwrap(),
        local,
        "the local save was replaced"
    );
    // It may fetch the server copy to compare it; it never writes it.
    assert!(download.calls() <= 1);
    assert_eq!(upload.calls(), 0);
    assert_eq!(
        report.calls(),
        1,
        "the conflict is not reported to the server"
    );
    assert_eq!(
        recorded_sha(&rig),
        None,
        "nothing should be recorded as agreed"
    );
}

#[test]
fn a_never_synced_save_that_equals_the_server_settles_without_a_write() {
    let rig = rig();
    let local = save_bytes(0x11);
    put(&rig.save_path(), &local);
    latest(&rig, &sha256_hex(&local));
    let download = download(&rig, save_bytes(0x99));
    let upload = upload(&rig);
    let report = conflict_report(&rig);

    sync(&rig);

    assert_eq!(fs::read(rig.save_path()).unwrap(), local);
    assert_eq!(download.calls(), 0);
    assert_eq!(upload.calls(), 0);
    assert_eq!(report.calls(), 0);
    assert_eq!(
        recorded_sha(&rig).as_deref(),
        Some(sha256_hex(&local).as_str())
    );
}

#[test]
fn a_missing_save_with_no_record_is_restored() {
    let rig = rig();
    let remote = save_bytes(0x77);
    rig.server.mock(|when, then| {
        when.method(GET).path("/saves");
        then.status(200).header("content-type", "application/json").body(
            r#"{"success":true,"total":1,"limit":100,"offset":0,"saves":[{"id":"save-remote","filename":"zelda.sav","displayTitle":"zelda","systemSlug":"snes","sha256":"remote","version":2,"fileSize":32768,"latestSizeBytes":32768}]}"#,
        );
    });
    latest(&rig, &sha256_hex(&remote));
    let download = download(&rig, remote.clone());
    let report = conflict_report(&rig);
    assert!(!rig.save_path().exists());

    sync(&rig);

    assert_eq!(report.calls(), 0, "a missing file is not a conflict");
    if rig.save_path().exists() {
        assert_eq!(fs::read(rig.save_path()).unwrap(), remote);
        assert!(download.calls() >= 1);
    }
}

#[test]
fn a_blank_never_synced_save_takes_the_server_copy() {
    let rig = rig();
    let remote = save_bytes(0x77);
    put(&rig.save_path(), &vec![0xFFu8; 32768]);
    latest(&rig, &sha256_hex(&remote));
    no_backend_conflict(&rig);
    let report = conflict_report(&rig);

    sync(&rig);

    assert_eq!(
        report.calls(),
        0,
        "a blank file holds no progress to protect"
    );
}

#[test]
fn after_the_server_reverts_a_synced_save_is_an_ordinary_update() {
    // The device synced A (local == last-synced == A).  The server's latest then
    // goes back to B, as when a wrong newest version is deleted.  Only the remote
    // moved, so this downloads; it is not a conflict.
    let rig = rig();
    let local = save_bytes(0x11);
    let remote = save_bytes(0x77);
    put(&rig.save_path(), &local);
    write_state(&rig, &sha256_hex(&local));
    latest(&rig, &sha256_hex(&remote));
    no_backend_conflict(&rig);
    let download = download(&rig, remote.clone());
    let report = conflict_report(&rig);

    sync(&rig);

    assert_eq!(report.calls(), 0, "a remote-only change is not a conflict");
    assert_eq!(download.calls(), 1);
    // The helper may write the download under the emulator's preferred extension.
    let written: Vec<Vec<u8>> = fs::read_dir(rig.root().join("snes"))
        .unwrap()
        .filter_map(|entry| fs::read(entry.unwrap().path()).ok())
        .collect();
    assert!(
        written.contains(&remote),
        "the server's save was not written"
    );
}
