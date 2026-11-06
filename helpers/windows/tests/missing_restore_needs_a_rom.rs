//! A missing save is restored to the path its state entry names. When no ROM on
//! the device carries that path's name -- a short name another tool gave the
//! game, keyed on the server by that short name -- no emulator loads the file,
//! so it is not written back and the entry is dropped. A missing save whose ROM
//! is on the device still restores.
use std::fs;

use assert_cmd::Command;
use httpmock::Method::{GET, POST};
use httpmock::{Mock, MockServer};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const BIN: &str = "sgm-windows-helper";
const OWN: &str = "saves/gba/007 - Everything or Nothing (USA, Europe) (En,Fr,De).srm";
const SHORT: &str = "saves/gba/007 - Everything or Nothing.srm";
/// The server's key for the long ROM's stem.
const ROM: &str = "e7d9a68e28180000000000000000000000000000";
/// The server's key for the bare short stem: no ROM file has that name.
const SHORT_KEY: &str = "0eb2c4fafc15ed2492e3e7b29db52e2d565076de";

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
    fs::write(
        rig.root()
            .join("roms/gba/007 - Everything or Nothing (USA, Europe) (En,Fr,De).zip"),
        b"rom",
    )
    .unwrap();
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
fn a_missing_save_under_a_name_no_rom_has_is_not_restored() {
    let rig = rig();
    let own = save_bytes(0x21);
    let other = save_bytes(0x5a);
    state_with_deleted_copy(&rig, &own, &sha256_hex(&other), SHORT_KEY, "gba");
    let download = latest_and_download(&rig, &own, SHORT_KEY, &other);

    let stderr = sync(&rig);

    assert!(
        !rig.root().join(SHORT).exists(),
        "a save no ROM on the device names was written back:\n{stderr}"
    );
    assert!(
        !state_text(&rig).contains("Everything or Nothing.srm\""),
        "its entry was kept:\n{}",
        state_text(&rig)
    );
    let _ = download;
    assert_eq!(fs::read(rig.root().join(OWN)).unwrap(), own);
}

#[test]
fn a_missing_save_of_a_rom_on_the_device_still_restores() {
    let rig = rig();
    let own = save_bytes(0x21);
    let lost = save_bytes(0x77);
    // The long save itself is the missing one; its ROM is on the device.
    let own_path = rig.root().join(OWN);
    fs::write(
        rig.state().join("sync_state.json"),
        format!(
            r#"{{"entries":{{{}}}}}"#,
            entry(&own_path, &sha256_hex(&own), ROM, "gba")
        ),
    )
    .unwrap();
    let download = latest_and_download(&rig, &lost, ROM, &lost);

    let stderr = sync(&rig);

    assert!(
        download.calls() >= 1,
        "the lost save was not fetched:\n{stderr}"
    );
    assert_eq!(
        fs::read(&own_path).unwrap(),
        lost,
        "the lost save was not restored to its ROM's name:\n{stderr}"
    );
}
