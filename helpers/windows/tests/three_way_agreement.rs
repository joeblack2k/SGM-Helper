//! The three-way check compares local and server against what both last agreed on.
//! Local and server holding the same bytes is agreement, whatever was recorded last,
//! so it settles and records them; only a real difference is a conflict, and that
//! conflict is reported to the server so it is visible there, not only in the log.
use std::fs;
use std::path::Path;

use assert_cmd::Command;
use httpmock::Method::{GET, POST};
use httpmock::{Mock, MockServer};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const BIN: &str = "sgm-windows-helper";

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
        r#"{{"entries":{{"{}":{{"sha256":"{last_sha}","rom_sha1":"rom-sha1","version":1,"system_slug":"snes","local_container":"native","adapter_profile":"identity","source_kind":"windows","source_name":"default-windows","updated_at":"2026-01-01T00:00:00Z"}}}}}}"#,
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
fn local_equal_to_the_server_settles_even_when_both_moved_since_the_last_sync() {
    let rig = rig();
    let last = save_bytes(0x22);
    let both = save_bytes(0x44);
    put(&rig.save_path(), &both);
    write_state(&rig, &sha256_hex(&last));
    latest(&rig, &sha256_hex(&both));
    no_backend_conflict(&rig);
    let download = download(&rig, both.clone());
    let upload = upload(&rig);
    let report = conflict_report(&rig);

    sync(&rig);

    assert_eq!(fs::read(rig.save_path()).unwrap(), both);
    assert_eq!(upload.calls(), 0, "agreeing bytes were uploaded");
    assert_eq!(download.calls(), 0, "agreeing bytes were downloaded");
    assert_eq!(report.calls(), 0, "agreement was reported as a conflict");
    assert_eq!(
        recorded_sha(&rig),
        Some(sha256_hex(&both)),
        "the agreed bytes are not recorded, so the next pass sees the same split"
    );
}

#[test]
fn a_three_way_conflict_is_reported_to_the_server_and_touches_neither_side() {
    let rig = rig();
    let last = save_bytes(0x22);
    let local = save_bytes(0x33);
    let remote = save_bytes(0x55);
    put(&rig.save_path(), &local);
    write_state(&rig, &sha256_hex(&last));
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
    assert_eq!(upload.calls(), 0);
    assert!(download.calls() <= 1);
    assert_eq!(
        report.calls(),
        1,
        "the conflict is not reported to the server"
    );
    assert_eq!(
        recorded_sha(&rig),
        Some(sha256_hex(&last)),
        "the agreed record moved"
    );
}
