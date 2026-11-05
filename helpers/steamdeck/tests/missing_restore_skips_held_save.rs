//! A save this helper synced under a second name -- another tool's short name
//! for the game -- can be deleted while the helper still holds the same game's
//! save under its own name. The missing-save restore must not write it back:
//! the save already held is the save. The deleted name's entry is dropped.
use std::fs;

use assert_cmd::Command;
use httpmock::Method::{GET, POST};
use httpmock::{Mock, MockServer};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const BIN: &str = "sgm-steamdeck-helper";
const OWN: &str = "saves/gba/007 - Everything or Nothing (USA, Europe) (En,Fr,De).srm";
const SHORT: &str = "saves/gba/007 - Everything or Nothing.srm";
const ROM: &str = "0eb2c4fafc15ed2492e3e7b29db52e2d565076de";

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn save_bytes(seed: u8) -> Vec<u8> {
    (0..512usize)
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
            .body(format!(
                r#"{{"success":true,"count":1,"rom":{{"sha1":"{ROM}","md5":"m"}}}}"#
            ));
    });
    server.mock(|when, then| {
        when.method(GET).path("/conflicts/check");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"exists":false}"#);
    });
    server.mock(|when, then| {
        when.method(GET).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"total":0,"limit":100,"offset":0,"saves":[]}"#);
    });
    let tmp = tempfile::tempdir().unwrap();
    let rig = Rig { server, tmp };
    fs::create_dir_all(rig.root().join("saves/gba")).unwrap();
    fs::create_dir_all(rig.root().join("roms/gba")).unwrap();
    fs::create_dir_all(rig.state()).unwrap();
    rig
}

fn entry(path: &std::path::Path, sha: &str, rom: &str, system: &str) -> String {
    format!(
        r#""{}":{{"sha256":"{sha}","rom_sha1":"{rom}","version":1,"system_slug":"{system}","local_container":"native","adapter_profile":"identity","source_kind":"retroarch","source_name":"retroarch","updated_at":"2026-01-01T00:00:00Z"}}"#,
        path.display()
    )
}

/// The held save on disk, plus a state entry for the deleted short-named copy.
fn state_with_deleted_copy(
    rig: &Rig,
    own: &[u8],
    short_sha: &str,
    short_rom: &str,
    short_system: &str,
) {
    let own_path = rig.root().join(OWN);
    fs::write(&own_path, own).unwrap();
    let body = format!(
        r#"{{"entries":{{{},{}}}}}"#,
        entry(&own_path, &sha256_hex(own), ROM, "gba"),
        entry(&rig.root().join(SHORT), short_sha, short_rom, short_system)
    );
    fs::write(rig.state().join("sync_state.json"), body).unwrap();
}

/// `/save/latest` for the held save's ROM answers with the held bytes; for the
/// missing entry's ROM, with `bytes`, which `/saves/download` serves.
fn latest_and_download<'a>(rig: &'a Rig, own: &[u8], missing_rom: &str, bytes: &[u8]) -> Mock<'a> {
    let own_sha = sha256_hex(own);
    rig.server.mock(move |when, then| {
        when.method(GET).path("/save/latest").query_param("romSha1", ROM);
        then.status(200)
            .header("content-type", "application/json")
            .body(format!(
                r#"{{"success":true,"exists":true,"sha256":"{own_sha}","version":1,"id":"save-own"}}"#
            ));
    });
    if missing_rom != ROM {
        let sha = sha256_hex(bytes);
        let rom = missing_rom.to_string();
        rig.server.mock(move |when, then| {
            when.method(GET).path("/save/latest").query_param("romSha1", rom.as_str());
            then.status(200)
                .header("content-type", "application/json")
                .body(format!(
                    r#"{{"success":true,"exists":true,"sha256":"{sha}","version":1,"id":"save-missing"}}"#
                ));
        });
    }
    let payload = bytes.to_vec();
    rig.server.mock(move |when, then| {
        when.method(GET).path("/saves/download");
        then.status(200).body(payload);
    })
}

fn sync(rig: &Rig) -> String {
    let config = rig.tmp.path().join("config.ini");
    fs::write(
        &config,
        format!(
            "URL=\"127.0.0.1\"\nPORT=\"{}\"\nROOT=\"{}\"\nSTATE_DIR=\"{}\"\nWATCH=\"false\"\nWATCH_INTERVAL=\"1\"\n\n[source.retroarch]\nLABEL=\"RetroArch\"\nKIND=\"retroarch\"\nPROFILE=\"retroarch\"\nSAVE_PATH=\"{}\"\nROM_PATH=\"{}\"\nRECURSIVE=\"true\"\nSYSTEMS=\"gba\"\n",
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

fn state_text(rig: &Rig) -> String {
    fs::read_to_string(rig.state().join("sync_state.json")).unwrap()
}

#[test]
fn a_deleted_copy_of_a_held_rom_is_not_restored() {
    let rig = rig();
    let own = save_bytes(0x21);
    let old = save_bytes(0x5a);
    state_with_deleted_copy(&rig, &own, &sha256_hex(&old), ROM, "gba");
    let download = latest_and_download(&rig, &own, ROM, &old);

    let stderr = sync(&rig);

    assert!(
        !rig.root().join(SHORT).exists(),
        "the deleted copy was written back:\n{stderr}"
    );
    assert_eq!(
        download.calls(),
        0,
        "the deleted copy was fetched:\n{stderr}"
    );
    assert!(
        !state_text(&rig).contains("Everything or Nothing.srm\""),
        "the deleted copy's entry was kept:\n{}",
        state_text(&rig)
    );
    assert_eq!(fs::read(rig.root().join(OWN)).unwrap(), own);
}

#[test]
fn a_deleted_copy_with_the_held_bytes_is_not_restored() {
    let rig = rig();
    let own = save_bytes(0x21);
    state_with_deleted_copy(&rig, &own, &sha256_hex(&own), "bridge:gba:007", "gba");
    let download = latest_and_download(&rig, &own, "bridge:gba:007", &own);

    let stderr = sync(&rig);

    assert!(
        !rig.root().join(SHORT).exists(),
        "the deleted copy was written back:\n{stderr}"
    );
    assert_eq!(
        download.calls(),
        0,
        "the deleted copy was fetched:\n{stderr}"
    );
}

#[test]
fn a_deleted_save_of_another_game_still_restores() {
    let rig = rig();
    let own = save_bytes(0x21);
    let other = save_bytes(0x77);
    state_with_deleted_copy(
        &rig,
        &own,
        &sha256_hex(&other),
        "f00dfacef00dfacef00dfacef00dfacef00dface",
        "gba",
    );
    let download = latest_and_download(
        &rig,
        &own,
        "f00dfacef00dfacef00dfacef00dfacef00dface",
        &other,
    );

    let stderr = sync(&rig);

    assert!(
        download.calls() >= 1,
        "a lost save of another game was not restored:\n{stderr}"
    );
    assert!(
        walk(&rig.root().join("saves"))
            .into_iter()
            .any(|path| path != rig.root().join(OWN)
                && fs::read(&path).map(|got| got == other).unwrap_or(false)),
        "the lost save was written nowhere:\n{stderr}"
    );
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
