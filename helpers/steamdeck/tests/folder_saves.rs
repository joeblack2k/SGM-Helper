//! A Dolphin NAND title and a Vita3K savedata folder are each synced as one folder bundle,
//! through the real sync command against a stand-in server, using the folders RetroDECK
//! keeps them in: `<saves>/wii/dolphin/title/<hi>/<lo>/data/` and `<saves>/psvita/vita3k/<ID>/`.

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use httpmock::Method::{GET, POST};
use httpmock::MockServer;

const CAVE_STORY: &str = "00010001/57435645";
const VITA_TITLE: &str = "PCSE00746";

fn mock_auth(server: &MockServer) {
    server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_folder","expiresInDays":7}"#);
    });
    server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"folder@example.com"}}"#);
    });
    server.mock(|when, then| {
        when.method(GET).path("/rom/lookup");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"count":0}"#);
    });
}

fn write_device(saves: &Path) {
    let cave = saves.join("wii/dolphin/title/00010001/57435645/data");
    fs::create_dir_all(&cave).unwrap();
    fs::write(cave.join("banner.bin"), b"WIBN cave story banner").unwrap();
    fs::write(cave.join("savedata.dat"), vec![0x5Au8; 131_104]).unwrap();
    // A system title holds the console's own settings: never a game save.
    let system = saves.join("wii/dolphin/title/00000001/00000002/data");
    fs::create_dir_all(&system).unwrap();
    fs::write(system.join("setting.txt"), b"system settings").unwrap();
    let vita = saves.join("psvita/vita3k").join(VITA_TITLE);
    fs::create_dir_all(&vita).unwrap();
    fs::write(vita.join("SAVEDATA.BIN"), vec![0x33u8; 4096]).unwrap();
}

fn write_config(
    tmp: &Path,
    server: &MockServer,
    saves: &Path,
    flag: Option<&str>,
) -> std::path::PathBuf {
    let state_dir = tmp.join("state");
    fs::create_dir_all(&state_dir).unwrap();
    let config = tmp.join("config.ini");
    let flag_line = flag
        .map(|value| format!("FOLDER_SAVE_BUNDLES=\"{value}\"\n"))
        .unwrap_or_default();
    let body = format!(
        "URL=\"127.0.0.1\"\nPORT=\"{}\"\nROOT=\"{}\"\nSTATE_DIR=\"{}\"\nWATCH=\"false\"\nWATCH_INTERVAL=\"1\"\n{}\n[source.retrodeck]\nLABEL=\"RetroDECK\"\nKIND=\"retroarch\"\nPROFILE=\"retroarch\"\nSAVE_PATH=\"{}\"\nROM_PATH=\"{}\"\nRECURSIVE=\"true\"\nSYSTEMS=\"wii,psvita\"\nCREATE_MISSING_SYSTEM_DIRS=\"false\"\nMANAGED=\"false\"\nORIGIN=\"test\"\n",
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
        .arg("folder@example.com")
        .arg("--app-password")
        .arg("pw")
        .assert()
        .success();
    config
}

fn sync(config: &Path) -> String {
    let output = Command::cargo_bin("sgm-steamdeck-helper")
        .unwrap()
        .arg("--config")
        .arg(config)
        .arg("sync")
        .arg("--verbose")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(output.status.success(), "sync failed: {stderr}");
    stderr
}

fn empty_server(server: &MockServer) {
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
            .body(r#"{"success":true,"saves":[],"total":0}"#);
    });
}

#[test]
fn a_nand_title_and_a_vita_folder_each_upload_as_one_bundle_and_nothing_else_uploads() {
    let server = MockServer::start();
    mock_auth(&server);
    empty_server(&server);
    let wii = server.mock(|when, then| {
        when.method(POST)
            .path("/saves")
            .body_includes("name=\"folderSaveDir\"")
            .body_includes(CAVE_STORY)
            .body_includes("filename=\"00010001-57435645.wiinand\"")
            .body_includes("wii-folder:00010001/57435645");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"save":{"id":"save-wii","sha256":"s","version":1}}"#);
    });
    let vita = server.mock(|when, then| {
        when.method(POST)
            .path("/saves")
            .body_includes("name=\"folderSaveDir\"")
            .body_includes(VITA_TITLE)
            .body_includes("filename=\"PCSE00746.vitasave\"");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"save":{"id":"save-vita","sha256":"s","version":1}}"#);
    });
    let any = server.mock(|when, then| {
        when.method(POST).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"save":{"id":"save-other","sha256":"s","version":1}}"#);
    });

    let tmp = tempfile::tempdir().unwrap();
    let saves = tmp.path().join("saves");
    write_device(&saves);
    let config = write_config(tmp.path(), &server, &saves, Some("true"));
    let stderr = sync(&config);

    assert_eq!(
        wii.calls(),
        1,
        "the NAND title was not uploaded as one bundle: {stderr}"
    );
    assert_eq!(
        vita.calls(),
        1,
        "the Vita folder was not uploaded as one bundle: {stderr}"
    );
    assert_eq!(
        any.calls(),
        0,
        "a loose file or a system title uploaded on its own: {stderr}"
    );
}

#[test]
fn a_folder_bundle_record_restores_into_the_device_s_own_nand_with_identical_bytes() {
    // The bundle device A uploads for Cave Story, as the server stores and returns it.
    let source = tempfile::tempdir().unwrap();
    let cave = source
        .path()
        .join("wii/dolphin/title/00010001/57435645/data");
    fs::create_dir_all(&cave).unwrap();
    fs::write(cave.join("banner.bin"), b"WIBN cave story banner").unwrap();
    fs::write(cave.join("savedata.dat"), vec![0x5Au8; 131_104]).unwrap();
    let (bundle, _) = sgm_steamdeck_helper::folder_saves::pack_folder_save(&cave).unwrap();

    let server = MockServer::start();
    mock_auth(&server);
    server.mock(|when, then| {
        when.method(GET).path("/save/latest");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"exists":false,"sha256":null,"version":null,"id":null}"#);
    });
    let record = serde_json::json!({
        "success": true,
        "total": 1,
        "saves": [{
            "id": "save-cave",
            "filename": "00010001-57435645.wiinand",
            "systemSlug": "wii",
            "game": {"system": {"slug": "wii"}, "name": "Cave Story"},
            "displayTitle": "Cave Story",
            "sha256": "cave",
            "version": 1,
            "metadata": {"rsm": {"folder": {"saveDir": CAVE_STORY, "bundle": "folder-zip"}}}
        }]
    });
    server.mock(move |when, then| {
        when.method(GET).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(record.to_string());
    });
    let download = server.mock(move |when, then| {
        when.method(GET)
            .path("/saves/download")
            .query_param("id", "save-cave");
        then.status(200).body(bundle.clone());
    });

    let tmp = tempfile::tempdir().unwrap();
    let saves = tmp.path().join("saves");
    // Device B runs Dolphin (its NAND root exists) but has never played Cave Story.
    fs::create_dir_all(saves.join("wii/dolphin/title")).unwrap();
    let config = write_config(tmp.path(), &server, &saves, Some("true"));
    let stderr = sync(&config);

    assert_eq!(
        download.calls(),
        1,
        "the folder bundle was not downloaded: {stderr}"
    );
    let restored = saves.join("wii/dolphin/title/00010001/57435645/data");
    assert_eq!(
        fs::read(restored.join("savedata.dat")).unwrap(),
        vec![0x5Au8; 131_104],
        "savedata.dat is not the same bytes: {stderr}"
    );
    assert_eq!(
        fs::read(restored.join("banner.bin")).unwrap(),
        b"WIBN cave story banner"
    );
}

#[test]
fn with_folder_save_bundles_off_no_folder_bundle_is_uploaded() {
    for flag in [None, Some("false")] {
        let server = MockServer::start();
        mock_auth(&server);
        empty_server(&server);
        let bundle = server.mock(|when, then| {
            when.method(POST)
                .path("/saves")
                .body_includes("name=\"folderSaveDir\"");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"success":true,"save":{"id":"save-x","sha256":"s","version":1}}"#);
        });
        let tmp = tempfile::tempdir().unwrap();
        let saves = tmp.path().join("saves");
        write_device(&saves);
        let config = write_config(tmp.path(), &server, &saves, flag);
        let stderr = sync(&config);
        assert_eq!(
            bundle.calls(),
            0,
            "a folder bundle uploaded with the flag {flag:?}: {stderr}"
        );
    }
}
