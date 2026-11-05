//! A record can arrive under a name no device uses -- another tool's short name
//! for the game. When this helper already holds that game's save under its own
//! name, restoring the record under the record's name writes a second copy, and
//! once the record is gone the next pass uploads that copy as a new save. The
//! save already held is the save: no second file, no download.
use std::fs;

use assert_cmd::Command;
use httpmock::Method::{GET, POST};
use httpmock::{Mock, MockServer};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const BIN: &str = "sgm-steamdeck-helper";
const OWN: &str = "saves/snes/Legend of Zelda, The - A Link to the Past (USA).srm";
const SHORT: &str = "Legend of Zelda The - A Link to the Past.srm";
const ROM: &str = "6d4f8e5fa0b1c2d3e4f5a6b7c8d9e0f1a2b3c4d5";

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn save_bytes(seed: u8) -> Vec<u8> {
    (0..8192usize)
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
            .body(format!(r#"{{"success":true,"count":1,"rom":{{"sha1":"{ROM}","md5":"m"}}}}"#));
    });
    server.mock(|when, then| {
        when.method(GET).path("/conflicts/check");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"exists":false}"#);
    });
    let tmp = tempfile::tempdir().unwrap();
    let rig = Rig { server, tmp };
    fs::create_dir_all(rig.root().join("saves/snes")).unwrap();
    fs::create_dir_all(rig.root().join("roms/snes")).unwrap();
    fs::create_dir_all(rig.state()).unwrap();
    rig
}

/// The device's own save, recorded as synced: snes, the ROM, these bytes.
fn own_save(rig: &Rig, bytes: &[u8]) {
    let path = rig.root().join(OWN);
    fs::write(&path, bytes).unwrap();
    let body = format!(
        r#"{{"entries":{{"{}":{{"sha256":"{}","rom_sha1":"{ROM}","version":1,"system_slug":"snes","local_container":"native","adapter_profile":"identity","source_kind":"retroarch","source_name":"retroarch","updated_at":"2026-01-01T00:00:00Z"}}}}}}"#,
        path.display(),
        sha256_hex(bytes)
    );
    fs::write(rig.state().join("sync_state.json"), body).unwrap();
    rig.server.mock(|when, then| {
        when.method(GET).path("/save/latest");
        then.status(200)
            .header("content-type", "application/json")
            .body(format!(
                r#"{{"success":true,"exists":true,"sha256":"{}","version":1,"id":"save-own"}}"#,
                sha256_hex(bytes)
            ));
    });
}

/// The server lists a record of the same game under the short name.
fn short_record<'a>(rig: &'a Rig, bytes: &[u8], rom: &str) -> Mock<'a> {
    let body = format!(
        r#"{{"success":true,"total":1,"limit":100,"offset":0,"saves":[{{"id":"save-short","filename":"{SHORT}","displayTitle":"Legend of Zelda The - A Link to the Past","systemSlug":"snes","romSha1":"{rom}","sha256":"{}","version":1,"latestSizeBytes":8192}}]}}"#,
        sha256_hex(bytes)
    );
    rig.server.mock(|when, then| {
        when.method(GET).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(body);
    });
    let payload = bytes.to_vec();
    rig.server.mock(move |when, then| {
        when.method(GET).path("/saves/download");
        then.status(200).body(payload);
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

fn sync(rig: &Rig) -> String {
    let config = rig.tmp.path().join("config.ini");
    fs::write(
        &config,
        format!(
            "URL=\"127.0.0.1\"\nPORT=\"{}\"\nROOT=\"{}\"\nSTATE_DIR=\"{}\"\nWATCH=\"false\"\nWATCH_INTERVAL=\"1\"\n\n[source.retroarch]\nLABEL=\"RetroArch\"\nKIND=\"retroarch\"\nPROFILE=\"retroarch\"\nSAVE_PATH=\"{}\"\nROM_PATH=\"{}\"\nRECURSIVE=\"true\"\nSYSTEMS=\"snes\"\n",
            rig.server.port(),
            rig.root().display(),
            rig.state().display(),
            rig.root().join("saves").display(),
            rig.root().join("roms").display()
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
    let output = Command::cargo_bin(BIN)
        .unwrap()
        .arg("--config")
        .arg(&config)
        .arg("sync")
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn a_short_named_record_of_a_held_save_is_not_restored_as_a_second_file() {
    let rig = rig();
    let bytes = save_bytes(0x21);
    own_save(&rig, &bytes);
    let download = short_record(&rig, &bytes, "bridge:snes:legend of zelda the - a link to the past");
    let upload = upload(&rig);

    let stderr = sync(&rig);

    assert!(
        !rig.root().join("saves/snes").join(SHORT).exists(),
        "a second copy was written under the record's name:\n{stderr}"
    );
    assert_eq!(download.calls(), 0, "the held save was fetched again:\n{stderr}");
    assert_eq!(upload.calls(), 0, "something was uploaded:\n{stderr}");
    assert_eq!(fs::read(rig.root().join(OWN)).unwrap(), bytes);
}

#[test]
fn a_short_named_record_of_the_held_rom_is_not_restored_beside_it() {
    let rig = rig();
    let own = save_bytes(0x21);
    own_save(&rig, &own);
    let download = short_record(&rig, &save_bytes(0x5a), ROM);

    let stderr = sync(&rig);

    assert!(
        !rig.root().join("saves/snes").join(SHORT).exists(),
        "a second copy was written beside the held save:\n{stderr}"
    );
    assert_eq!(download.calls(), 0, "the record was fetched:\n{stderr}");
    assert_eq!(fs::read(rig.root().join(OWN)).unwrap(), own);
}

#[test]
fn a_record_of_a_game_this_helper_does_not_hold_still_restores() {
    let rig = rig();
    let bytes = save_bytes(0x33);
    let download = short_record(&rig, &bytes, "bridge:snes:legend of zelda the - a link to the past");

    let stderr = sync(&rig);

    // Wherever this helper's layout puts a restore, the new game's save lands.
    assert_eq!(download.calls(), 1, "a new game's save was not restored:\n{stderr}");
    let landed = walk(&rig.root())
        .into_iter()
        .any(|path| fs::read(&path).map(|got| got == bytes).unwrap_or(false));
    assert!(landed, "the downloaded save was written nowhere:\n{stderr}");
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}
