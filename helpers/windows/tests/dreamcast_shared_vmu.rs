//! RetroDECK's Flycast (per-content VMUs off) keeps saves in the shared cards
//! `<retrodeck>/bios/dc/vmu_save_<slot>.bin`, beside the source's saves root.
//! A card holding a game's save is uploaded; a formatted, empty card is not.

use assert_cmd::Command;
use httpmock::prelude::*;
use std::fs;

const SOULCALIBUR_A1: &[u8] = include_bytes!("fixtures/vmu_soulcalibur_A1.bin");
const BLANK_B1: &[u8] = include_bytes!("fixtures/vmu_blank_B1.bin");
/// The Soulcalibur card with one byte of save data changed: a valid card holding a different save.
const OTHER_SAVE_A1: &[u8] = include_bytes!("fixtures/vmu_other_save_A1.bin");

#[test]
fn a_shared_flycast_vmu_with_a_save_is_uploaded_and_a_blank_one_is_not() {
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
            .body(r#"{"success":true,"user":{"email":"dc@example.com"}}"#);
    });
    server.mock(|when, then| {
        when.method(GET).path("/rom/lookup");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"count":0}"#);
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
            .body(r#"{"success":true,"saves":[]}"#);
    });
    let upload = server.mock(|when, then| {
        when.method(POST).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"save":{"id":"save-dc","sha256":"sha-dc"}}"#);
    });

    let tmp = tempfile::tempdir().unwrap();
    let rd = tmp.path().join("RetroArch");
    let saves = rd.join("saves");
    let roms = rd.join("roms");
    let dc = rd.join("system").join("dc");
    let state = tmp.path().join("state");
    for d in [&saves, &roms.join("dreamcast"), &dc, &state] {
        fs::create_dir_all(d).unwrap();
    }
    fs::write(dc.join("vmu_save_A1.bin"), SOULCALIBUR_A1).unwrap();
    fs::write(dc.join("vmu_save_B1.bin"), BLANK_B1).unwrap();

    let config = tmp.path().join("config.ini");
    fs::write(
        &config,
        format!(
            "URL=\"127.0.0.1\"\nPORT=\"{}\"\nROOT=\"{}\"\nSTATE_DIR=\"{}\"\nWATCH=\"false\"\nWATCH_INTERVAL=\"1\"\n\n[source.retrodeck]\nLABEL=\"RetroDECK\"\nKIND=\"retroarch\"\nPROFILE=\"retroarch\"\nSAVE_PATH=\"{}\"\nROM_PATH=\"{}\"\nRECURSIVE=\"true\"\nSYSTEMS=\"dreamcast\"\nCREATE_MISSING_SYSTEM_DIRS=\"false\"\nMANAGED=\"false\"\nORIGIN=\"test\"\n",
            server.port(),
            saves.display(),
            state.display(),
            saves.display(),
            roms.display()
        ),
    )
    .unwrap();

    Command::cargo_bin("sgm-windows-helper")
        .unwrap()
        .args([
            "--config",
            config.to_str().unwrap(),
            "login",
            "--email",
            "dc@example.com",
            "--app-password",
            "pw",
        ])
        .assert()
        .success();
    let out = Command::cargo_bin("sgm-windows-helper")
        .unwrap()
        .args(["--config", config.to_str().unwrap(), "sync", "--verbose"])
        .output()
        .unwrap();
    eprintln!("{}", String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success());

    assert_eq!(
        upload.calls(),
        1,
        "only the card holding Soulcalibur is uploaded"
    );
}

/// A Dreamcast record served from the cloud, for the restore tests.
fn restore_server(card: &'static [u8]) -> MockServer {
    restore_server_named(card, "vmu_save_A1.bin")
}

/// A Dreamcast record served from the cloud under `filename`.
fn restore_server_named(card: &'static [u8], filename: &str) -> MockServer {
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
            .body(r#"{"success":true,"user":{"email":"dc@example.com"}}"#);
    });
    server.mock(|when, then| {
        when.method(GET).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(format!(r#"{{"success":true,"total":1,"limit":100,"offset":0,"saves":[{{"id":"save-dc","filename":"{filename}","displayTitle":"Soulcalibur","systemSlug":"dreamcast","cardSlot":"A1","sha256":"85479d63c35ac341","version":1,"latestSizeBytes":131072}}]}}"#));
    });
    server.mock(|when, then| {
        when.method(GET).path("/saves/download");
        then.status(200).body(card);
    });
    server
}

/// A RetroDECK layout with the shared-card folder present, and its config.
fn retrodeck_layout(
    tmp: &tempfile::TempDir,
    server: &MockServer,
) -> (std::path::PathBuf, std::path::PathBuf) {
    let rd = tmp.path().join("RetroArch");
    let saves = rd.join("saves");
    let roms = rd.join("roms");
    let dc = rd.join("system").join("dc");
    let state = tmp.path().join("state");
    for d in [&saves, &roms.join("dreamcast"), &dc, &state] {
        fs::create_dir_all(d).unwrap();
    }
    let config = tmp.path().join("config.ini");
    fs::write(
        &config,
        format!(
            "URL=\"127.0.0.1\"\nPORT=\"{}\"\nROOT=\"{}\"\nSTATE_DIR=\"{}\"\nWATCH=\"false\"\nWATCH_INTERVAL=\"1\"\n\n[source.retrodeck]\nLABEL=\"RetroDECK\"\nKIND=\"retroarch\"\nPROFILE=\"retroarch\"\nSAVE_PATH=\"{}\"\nROM_PATH=\"{}\"\nRECURSIVE=\"true\"\nSYSTEMS=\"dreamcast\"\nCREATE_MISSING_SYSTEM_DIRS=\"false\"\nMANAGED=\"false\"\nORIGIN=\"test\"\n",
            server.port(),
            saves.display(),
            state.display(),
            saves.display(),
            roms.display()
        ),
    )
    .unwrap();
    (config, dc)
}

fn login_and_sync(config: &std::path::Path) -> String {
    Command::cargo_bin("sgm-windows-helper")
        .unwrap()
        .args([
            "--config",
            config.to_str().unwrap(),
            "login",
            "--email",
            "dc@example.com",
            "--app-password",
            "pw",
        ])
        .assert()
        .success();
    let out = Command::cargo_bin("sgm-windows-helper")
        .unwrap()
        .args(["--config", config.to_str().unwrap(), "sync", "--verbose"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stderr).to_string()
}

#[test]
fn a_dreamcast_record_restores_into_the_shared_card_when_none_exists_yet() {
    let server = restore_server(SOULCALIBUR_A1);
    let tmp = tempfile::tempdir().unwrap();
    let (config, dc) = retrodeck_layout(&tmp, &server);
    assert!(!dc.join("vmu_save_A1.bin").exists());

    let stderr = login_and_sync(&config);

    assert_eq!(
        fs::read(dc.join("vmu_save_A1.bin")).ok().as_deref(),
        Some(SOULCALIBUR_A1),
        "the record did not land in bios/dc/vmu_save_A1.bin:\n{stderr}"
    );
}

/// The card's sync record, as this device would have left it after syncing `card_bytes`.
fn synced_card_state(tmp: &tempfile::TempDir, dc: &std::path::Path, card_bytes: &[u8]) {
    use sha2::{Digest, Sha256};
    let sha = format!("{:x}", Sha256::digest(card_bytes));
    let card = dc.join("vmu_save_A1.bin");
    let state = format!(
        r#"{{"entries": {{"{}": {{"sha256": "{}", "rom_sha1": null, "version": 1, "system_slug": "dreamcast", "local_container": "native", "adapter_profile": "identity", "source_kind": "retroarch", "source_name": "RetroDECK", "slot_name": "A1", "updated_at": "2026-01-01T00:00:00Z"}}}}}}"#,
        card.display(),
        sha
    );
    fs::write(tmp.path().join("state").join("sync_state.json"), state).unwrap();
}

#[test]
fn a_dreamcast_record_replaces_a_shared_card_still_holding_what_this_device_last_synced() {
    let server = restore_server(SOULCALIBUR_A1);
    let tmp = tempfile::tempdir().unwrap();
    let (config, dc) = retrodeck_layout(&tmp, &server);
    fs::write(dc.join("vmu_save_A1.bin"), OTHER_SAVE_A1).unwrap();
    synced_card_state(&tmp, &dc, OTHER_SAVE_A1);

    let stderr = login_and_sync(&config);

    assert_eq!(
        fs::read(dc.join("vmu_save_A1.bin")).ok().as_deref(),
        Some(SOULCALIBUR_A1),
        "a card unchanged since the last sync was not updated:\n{stderr}"
    );
}

/// A shared card with saves this device never synced (no sync record) holds every game's saves;
/// the cloud record must not replace it.
#[test]
fn a_shared_card_with_no_sync_record_that_differs_from_the_cloud_is_left_unchanged() {
    let server = restore_server(SOULCALIBUR_A1);
    let tmp = tempfile::tempdir().unwrap();
    let (config, dc) = retrodeck_layout(&tmp, &server);
    fs::write(dc.join("vmu_save_A1.bin"), OTHER_SAVE_A1).unwrap();

    let stderr = login_and_sync(&config);

    assert_eq!(
        fs::read(dc.join("vmu_save_A1.bin")).ok().as_deref(),
        Some(OTHER_SAVE_A1),
        "the cloud record replaced a card holding unsynced saves:\n{stderr}"
    );
}

/// A record uploaded from a per-game VMU holds one game's card; it must never land on the shared
/// card, where it would wipe the other games' saves.
#[test]
fn a_per_game_vmu_record_does_not_touch_the_shared_card() {
    let server = restore_server_named(SOULCALIBUR_A1, "Soulcalibur (USA).A1.bin");
    let tmp = tempfile::tempdir().unwrap();
    let (config, dc) = retrodeck_layout(&tmp, &server);
    fs::write(dc.join("vmu_save_A1.bin"), OTHER_SAVE_A1).unwrap();
    synced_card_state(&tmp, &dc, OTHER_SAVE_A1);

    let stderr = login_and_sync(&config);

    assert_eq!(
        fs::read(dc.join("vmu_save_A1.bin")).ok().as_deref(),
        Some(OTHER_SAVE_A1),
        "a per-game VMU record was written over the shared card:\n{stderr}"
    );
}
