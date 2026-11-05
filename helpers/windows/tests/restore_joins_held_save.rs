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

const BIN: &str = "sgm-windows-helper";
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
    for dir in ["saves/snes", "saves/psx", "saves/gb"] {
        fs::create_dir_all(rig.root().join(dir)).unwrap();
    }
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
            "URL=\"127.0.0.1\"\nPORT=\"{}\"\nROOT=\"{}\"\nSTATE_DIR=\"{}\"\nWATCH=\"false\"\nWATCH_INTERVAL=\"1\"\n\n[source.retroarch]\nLABEL=\"RetroArch\"\nKIND=\"retroarch\"\nPROFILE=\"retroarch\"\nSAVE_PATH=\"{}\"\nROM_PATH=\"{}\"\nRECURSIVE=\"true\"\nSYSTEMS=\"snes,psx,gameboy\"\n",
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

/// The device's own save recorded with a given sha256 string (it need not be the
/// file's real digest: this pins what the comparison reads).
fn own_save_recorded_as(rig: &Rig, bytes: &[u8], recorded_sha: &str) {
    let path = rig.root().join(OWN);
    fs::write(&path, bytes).unwrap();
    let body = format!(
        r#"{{"entries":{{"{}":{{"sha256":"{recorded_sha}","rom_sha1":"other-rom","version":1,"system_slug":"snes","local_container":"native","adapter_profile":"identity","source_kind":"retroarch","source_name":"retroarch","updated_at":"2026-01-01T00:00:00Z"}}}}}}"#,
        path.display()
    );
    fs::write(rig.state().join("sync_state.json"), body).unwrap();
    rig.server.mock(|when, then| {
        when.method(GET).path("/save/latest");
        then.status(200)
            .header("content-type", "application/json")
            .body(format!(
                r#"{{"success":true,"exists":true,"sha256":"{recorded_sha}","version":1,"id":"save-own"}}"#
            ));
    });
}

#[test]
fn a_short_digest_matching_a_held_save_s_prefix_is_not_the_same_save() {
    let rig = rig();
    let held = save_bytes(0x21);
    own_save_recorded_as(&rig, &held, &sha256_hex(&held));
    // The record names only the first 16 hex characters of the held save's
    // digest: not proof of the same bytes, so the restore goes ahead.
    let remote = save_bytes(0x44);
    let short: String = sha256_hex(&held)[..16].to_string();
    let body = format!(
        r#"{{"success":true,"total":1,"limit":100,"offset":0,"saves":[{{"id":"save-short","filename":"{SHORT}","displayTitle":"x","systemSlug":"snes","romSha1":"bridge:snes:x","sha256":"{short}","version":1,"latestSizeBytes":8192}}]}}"#
    );
    rig.server.mock(|when, then| {
        when.method(GET).path("/saves");
        then.status(200).header("content-type", "application/json").body(body);
    });
    let download = rig.server.mock(move |when, then| {
        when.method(GET).path("/saves/download");
        then.status(200).body(remote);
    });

    let stderr = sync(&rig);

    assert!(
        !stderr.contains("already held at"),
        "an abbreviated digest was treated as the same save:\n{stderr}"
    );
    assert_eq!(download.calls(), 1, "the record was not restored:\n{stderr}");
}

#[test]
fn two_full_digests_sharing_a_prefix_are_not_the_same_save() {
    let rig = rig();
    let remote = save_bytes(0x44);
    let remote_sha = sha256_hex(&remote);
    // Same first 16 hex characters, different after: a different save.
    let mut recorded = remote_sha.clone();
    let last = if recorded.ends_with('0') { "1" } else { "0" };
    recorded.replace_range(63..64, last);
    assert_eq!(recorded.len(), 64);
    assert_ne!(recorded, remote_sha);
    own_save_recorded_as(&rig, &save_bytes(0x21), &recorded);
    let download = short_record(&rig, &remote, "bridge:snes:legend of zelda the - a link to the past");

    let stderr = sync(&rig);

    assert!(
        !stderr.contains("already held at"),
        "a different full digest was treated as the same save:\n{stderr}"
    );
    assert_eq!(download.calls(), 1, "the record was not restored:\n{stderr}");
}

#[test]
fn the_skip_line_names_the_check_that_matched() {
    let rig = rig();
    let bytes = save_bytes(0x21);
    own_save(&rig, &bytes);
    let _download = short_record(&rig, &bytes, "bridge:snes:legend of zelda the - a link to the past");

    let stderr = sync(&rig);

    assert!(
        stderr.contains("already held at") && stderr.contains("(matched by bytes)"),
        "the skip line does not say which check matched:\n{stderr}"
    );
}

/// A formatted PS1 memory card (frame 0 "MC", frames 0-15 XOR-checksummed),
/// with `seed` in its data blocks so two cards differ.
fn ps1_card(seed: u8) -> Vec<u8> {
    let mut bytes = vec![0x00u8; 131072];
    bytes[..2].copy_from_slice(b"MC");
    for frame in 0..16usize {
        let start = frame * 128;
        let checksum = bytes[start..start + 127].iter().fold(0u8, |acc, value| acc ^ value);
        bytes[start + 127] = checksum;
    }
    for (index, byte) in bytes[8192..].iter_mut().enumerate() {
        *byte = seed.wrapping_add((index % 241) as u8);
    }
    bytes
}

/// A save this helper holds and has recorded as synced under `rom`.
fn held(rig: &Rig, relative: &str, system: &str, rom: &str, bytes: &[u8]) {
    let path = rig.root().join(relative);
    fs::write(&path, bytes).unwrap();
    let body = format!(
        r#"{{"entries":{{"{}":{{"sha256":"{}","rom_sha1":"{rom}","version":1,"system_slug":"{system}","local_container":"native","adapter_profile":"identity","source_kind":"retroarch","source_name":"retroarch","updated_at":"2026-01-01T00:00:00Z"}}}}}}"#,
        path.display(),
        sha256_hex(bytes)
    );
    fs::write(rig.state().join("sync_state.json"), body).unwrap();
    rig.server.mock(|when, then| {
        when.method(GET).path("/save/latest");
        then.status(200)
            .header("content-type", "application/json")
            .body(format!(
                r#"{{"success":true,"exists":true,"sha256":"{}","version":1,"id":"save-held"}}"#,
                sha256_hex(bytes)
            ));
    });
}

/// The server lists one record; returns the download mock.
fn record<'a>(
    rig: &'a Rig,
    filename: &str,
    system: &str,
    rom: &str,
    card_slot: Option<&str>,
    bytes: &[u8],
) -> Mock<'a> {
    let slot = card_slot
        .map(|slot| format!(r#""cardSlot":"{slot}","#))
        .unwrap_or_default();
    let body = format!(
        r#"{{"success":true,"total":1,"limit":100,"offset":0,"saves":[{{"id":"save-rec","filename":"{filename}","displayTitle":"x","systemSlug":"{system}",{slot}"romSha1":"{rom}","sha256":"{}","version":1,"latestSizeBytes":{}}}]}}"#,
        sha256_hex(bytes),
        bytes.len()
    );
    rig.server.mock(|when, then| {
        when.method(GET).path("/saves");
        then.status(200).header("content-type", "application/json").body(body);
    });
    let payload = bytes.to_vec();
    rig.server.mock(move |when, then| {
        when.method(GET).path("/saves/download");
        then.status(200).body(payload);
    })
}

fn landed(rig: &Rig, bytes: &[u8]) -> bool {
    walk(&rig.root())
        .into_iter()
        .any(|path| fs::read(&path).map(|got| got == bytes).unwrap_or(false))
}

#[test]
fn two_ps1_cards_of_one_rom_on_different_slots_both_restore() {
    // A PS1 game holds a card per slot: its own "Game: <name>" card here, and a
    // second card of the same ROM on the server. One ROM, two saves -- the ROM
    // says nothing about which card a record is, so the second card restores.
    let rig = rig();
    held(&rig, "saves/psx/Tekken 3 (USA).srm", "psx", ROM, &ps1_card(0x11));
    let _upload = upload(&rig);
    let second = ps1_card(0x77);
    let download = record(&rig, "Tekken 3 (USA)_2.mcr", "psx", ROM, Some("Memory Card 2"), &second);

    let stderr = sync(&rig);

    assert!(
        !stderr.contains("already held at"),
        "a second card of the same ROM was treated as the held card:\n{stderr}"
    );
    assert_eq!(download.calls(), 1, "the second card was not restored:\n{stderr}");
    assert!(landed(&rig, &second), "the second card was written nowhere:\n{stderr}");
}

#[test]
fn a_short_named_record_of_a_held_ps1_card_is_skipped_by_its_bytes() {
    let rig = rig();
    let card = ps1_card(0x11);
    held(&rig, "saves/psx/Tekken 3 (USA).srm", "psx", ROM, &card);
    let download = record(&rig, "Tekken 3.mcr", "psx", "bridge:psx:tekken 3", Some("Game: Tekken 3"), &card);

    let stderr = sync(&rig);

    assert!(
        stderr.contains("already held at") && stderr.contains("(matched by bytes)"),
        "the held card's own bytes did not stop the restore:\n{stderr}"
    );
    assert_eq!(download.calls(), 0, "the held card was fetched again:\n{stderr}");
}

#[test]
fn a_clock_sidecar_of_the_held_rom_restores_beside_its_save() {
    // A Game Boy cartridge's .rtc sits beside its .srm under the same ROM: the
    // shared ROM never makes the clock the save already held.
    let rig = rig();
    let sram = save_bytes(0x21);
    held(&rig, "saves/gb/Pokemon - Gold Version (USA).srm", "gameboy", ROM, &sram);
    let clock: Vec<u8> = (0u8..48).collect();
    let download = record(&rig, "Pokemon - Gold Version (USA).rtc", "gameboy", ROM, None, &clock);

    let stderr = sync(&rig);

    assert!(
        !stderr.contains("already held at"),
        "the clock sidecar was treated as the held save:\n{stderr}"
    );
    assert_eq!(download.calls(), 1, "the clock sidecar was not restored:\n{stderr}");
    assert_eq!(
        fs::read(rig.root().join("saves/gb/Pokemon - Gold Version (USA).rtc")).ok(),
        Some(clock),
        "the clock did not land as .rtc beside its save:\n{stderr}"
    );
    assert_eq!(
        fs::read(rig.root().join("saves/gb/Pokemon - Gold Version (USA).srm")).unwrap(),
        sram,
        "the held save was changed:\n{stderr}"
    );
}
