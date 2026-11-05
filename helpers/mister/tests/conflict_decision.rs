//! A conflict someone decided ("keep_local" / "keep_server") for exactly the two
//! hashes on hand is acted on once and recorded; a decision for another pair is
//! stale and the conflict is reported again; a server without decisions changes
//! nothing.
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

fn check_with(rig: &Rig, resolution: Option<&str>, local: &str, cloud: &str) {
    let body = match resolution {
        Some(r) => format!(
            r#"{{"exists":true,"conflictId":"conf-1","resolution":"{r}","localSha256":"{local}","cloudSha256":"{cloud}","cloudVersion":2,"cloudSaveId":"save-remote"}}"#
        ),
        None => r#"{"exists":false}"#.to_string(),
    };
    rig.server.mock(|when, then| {
        when.method(GET).path("/conflicts/check");
        then.status(200)
            .header("content-type", "application/json")
            .body(body);
    });
}

fn applied(rig: &Rig, status: u16) -> Mock<'_> {
    rig.server.mock(|when, then| {
        when.method(POST).path("/conflicts/conf-1/applied");
        then.status(status)
            .header("content-type", "application/json")
            .body(r#"{"success":true}"#);
    })
}

#[test]
fn keep_local_uploads_this_save_and_records_it() {
    let rig = rig();
    let local = save_bytes(0x11);
    let remote = save_bytes(0x77);
    put(&rig.save_path(), &local);
    latest(&rig, &sha256_hex(&remote));
    check_with(
        &rig,
        Some("keep_local"),
        &sha256_hex(&local),
        &sha256_hex(&remote),
    );
    let download = download(&rig, remote);
    let upload = upload(&rig);
    let report = conflict_report(&rig);
    let applied = applied(&rig, 200);

    sync(&rig);

    assert_eq!(upload.calls(), 1, "keep_local must upload the local save");
    assert_eq!(download.calls(), 0);
    assert_eq!(
        report.calls(),
        0,
        "a decided conflict is not reported again"
    );
    assert_eq!(applied.calls(), 1, "the server must hear it was applied");
    assert_eq!(fs::read(rig.save_path()).unwrap(), local);
    assert_eq!(
        recorded_sha(&rig).as_deref(),
        Some(sha256_hex(&local).as_str())
    );
}

#[test]
fn keep_server_downloads_and_records_it() {
    let rig = rig();
    let local = save_bytes(0x11);
    let remote = save_bytes(0x77);
    put(&rig.save_path(), &local);
    latest(&rig, &sha256_hex(&remote));
    check_with(
        &rig,
        Some("keep_server"),
        &sha256_hex(&local),
        &sha256_hex(&remote),
    );
    let download = download(&rig, remote.clone());
    let upload = upload(&rig);
    let report = conflict_report(&rig);
    let applied = applied(&rig, 200);

    sync(&rig);

    assert_eq!(download.calls(), 1);
    assert_eq!(upload.calls(), 0);
    assert_eq!(report.calls(), 0);
    assert_eq!(applied.calls(), 1);
    // The helper may write the download under the emulator's preferred extension.
    let written: Vec<Vec<u8>> = fs::read_dir(rig.root().join("snes"))
        .unwrap()
        .filter_map(|entry| fs::read(entry.unwrap().path()).ok())
        .collect();
    assert!(
        written.contains(&remote),
        "the server's save was not written"
    );
    let state = fs::read_to_string(rig.state().join("sync_state.json")).unwrap();
    assert!(
        state.contains(&sha256_hex(&remote)),
        "the server's sha256 must be recorded as last synced"
    );
}

#[test]
fn a_decision_for_another_pair_is_not_acted_on() {
    let rig = rig();
    let local = save_bytes(0x11);
    let remote = save_bytes(0x77);
    put(&rig.save_path(), &local);
    latest(&rig, &sha256_hex(&remote));
    // Decided when the local save was different bytes: stale.
    check_with(
        &rig,
        Some("keep_server"),
        &sha256_hex(&save_bytes(0x33)),
        &sha256_hex(&remote),
    );
    let download = download(&rig, remote);
    let upload = upload(&rig);
    let report = conflict_report(&rig);
    let applied = applied(&rig, 200);

    sync(&rig);

    assert_eq!(
        fs::read(rig.save_path()).unwrap(),
        local,
        "a stale decision overwrote the save"
    );
    assert_eq!(upload.calls(), 0);
    assert_eq!(applied.calls(), 0);
    assert_eq!(report.calls(), 1, "the conflict must be reported again");
    let _ = download;
}

#[test]
fn a_server_without_decisions_changes_nothing() {
    let rig = rig();
    let local = save_bytes(0x11);
    let remote = save_bytes(0x77);
    put(&rig.save_path(), &local);
    latest(&rig, &sha256_hex(&remote));
    check_with(&rig, None, "", "");
    let upload = upload(&rig);
    let report = conflict_report(&rig);
    let applied = applied(&rig, 404);
    let _download = download(&rig, remote);

    sync(&rig);

    assert_eq!(fs::read(rig.save_path()).unwrap(), local);
    assert_eq!(upload.calls(), 0);
    assert_eq!(applied.calls(), 0);
    assert_eq!(report.calls(), 1);
}
