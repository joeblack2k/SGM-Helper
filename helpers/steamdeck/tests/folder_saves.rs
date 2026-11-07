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

fn serve_vita_record(server: &MockServer, bundle: Vec<u8>) -> httpmock::Mock<'_> {
    mock_auth(server);
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
            "id": "save-vita",
            "filename": "PCSE00746.vitasave",
            "systemSlug": "psvita",
            "game": {"system": {"slug": "psvita"}, "name": "PCSE00746"},
            "displayTitle": "PCSE00746",
            "sha256": "vita",
            "version": 1,
            "metadata": {"rsm": {"folder": {"saveDir": VITA_TITLE, "bundle": "folder-zip"}}}
        }]
    });
    server.mock(move |when, then| {
        when.method(GET).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(record.to_string());
    });
    server.mock(move |when, then| {
        when.method(GET)
            .path("/saves/download")
            .query_param("id", "save-vita");
        then.status(200).body(bundle.clone());
    })
}

fn files_under(dir: &Path) -> Vec<(String, Vec<u8>)> {
    sgm_steamdeck_helper::folder_saves::folder_save_files(dir)
        .unwrap()
        .into_iter()
        .map(|(relative, path, _)| (relative, fs::read(path).unwrap()))
        .collect()
}

#[test]
fn a_vita_save_with_sce_sys_restores_through_sync_with_every_file_intact() {
    let source = tempfile::tempdir().unwrap();
    let save = source.path().join("psvita/vita3k").join(VITA_TITLE);
    fs::create_dir_all(save.join("sce_sys")).unwrap();
    fs::write(save.join("SAVEDATA.BIN"), vec![0x33u8; 4096]).unwrap();
    fs::write(save.join("sce_sys/param.sfo"), b"\0PSF param").unwrap();
    fs::write(save.join("sce_sys/sdslot.dat"), vec![0x07u8; 8192]).unwrap();
    fs::write(save.join("sce_sys/keystone"), b"keystone").unwrap();
    let (bundle, _) = sgm_steamdeck_helper::folder_saves::pack_folder_save(&save).unwrap();
    let server = MockServer::start();
    let download = serve_vita_record(&server, bundle);

    let tmp = tempfile::tempdir().unwrap();
    let saves = tmp.path().join("saves");
    // Device B runs Vita3K (its savedata root exists) but has never played the title.
    fs::create_dir_all(saves.join("psvita/vita3k")).unwrap();
    let config = write_config(tmp.path(), &server, &saves, Some("true"));
    let stderr = sync(&config);

    assert_eq!(
        download.calls(),
        1,
        "the Vita bundle was not downloaded: {stderr}"
    );
    let restored = saves.join("psvita/vita3k").join(VITA_TITLE);
    assert_eq!(files_under(&restored), files_under(&save), "{stderr}");
}

#[test]
fn a_vita_save_with_sce_sys_uploads_its_subfolder_files() {
    let server = MockServer::start();
    mock_auth(&server);
    empty_server(&server);
    let vita = server.mock(|when, then| {
        when.method(POST)
            .path("/saves")
            .body_includes("filename=\"PCSE00746.vitasave\"")
            .body_includes("sce_sys/param.sfo")
            .body_includes("sce_sys/sdslot.dat");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"save":{"id":"save-vita","sha256":"s","version":1}}"#);
    });
    let tmp = tempfile::tempdir().unwrap();
    let saves = tmp.path().join("saves");
    let save = saves.join("psvita/vita3k").join(VITA_TITLE);
    fs::create_dir_all(save.join("sce_sys")).unwrap();
    fs::write(save.join("SAVEDATA.BIN"), vec![0x33u8; 4096]).unwrap();
    fs::write(save.join("sce_sys/param.sfo"), b"\0PSF param").unwrap();
    fs::write(save.join("sce_sys/sdslot.dat"), vec![0x07u8; 8192]).unwrap();
    let config = write_config(tmp.path(), &server, &saves, Some("true"));
    let stderr = sync(&config);
    assert_eq!(
        vita.calls(),
        1,
        "sce_sys was not in the uploaded bundle: {stderr}"
    );
}

/// A device turning folder bundles on for the first time has no folder-keyed state, though it
/// may hold progress for a title the server already has from another device. Its folder must
/// upload, not be replaced by the server's copy.
#[test]
fn on_a_first_sync_a_local_folder_that_differs_from_the_server_uploads_and_is_kept() {
    let server = MockServer::start();
    mock_auth(&server);
    server.mock(|when, then| {
        when.method(GET).path("/save/latest");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"exists":true,"sha256":"another-device","version":3,"id":"save-other"}"#);
    });
    server.mock(|when, then| {
        when.method(GET).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"saves":[],"total":0}"#);
    });
    let download = server.mock(|when, then| {
        when.method(GET).path("/saves/download");
        then.status(200).body(b"not this device's save");
    });
    let upload = server.mock(|when, then| {
        when.method(POST)
            .path("/saves")
            .body_includes("filename=\"PCSE00746.vitasave\"");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"save":{"id":"save-mine","sha256":"s","version":4}}"#);
    });
    let tmp = tempfile::tempdir().unwrap();
    let saves = tmp.path().join("saves");
    let save = saves.join("psvita/vita3k").join(VITA_TITLE);
    fs::create_dir_all(save.join("sce_sys")).unwrap();
    fs::write(save.join("SAVEDATA.BIN"), b"this device's progress").unwrap();
    fs::write(save.join("sce_sys/param.sfo"), b"\0PSF mine").unwrap();
    let before = sgm_steamdeck_helper::folder_saves::folder_save_files(&save)
        .unwrap()
        .into_iter()
        .map(|(relative, path, _)| (relative, fs::read(path).unwrap()))
        .collect::<Vec<_>>();
    let config = write_config(tmp.path(), &server, &saves, Some("true"));
    let stderr = sync(&config);
    assert_eq!(
        upload.calls(),
        1,
        "the device's folder was not uploaded: {stderr}"
    );
    assert_eq!(
        download.calls(),
        0,
        "the server's copy was downloaded over it: {stderr}"
    );
    let after = sgm_steamdeck_helper::folder_saves::folder_save_files(&save)
        .unwrap()
        .into_iter()
        .map(|(relative, path, _)| (relative, fs::read(path).unwrap()))
        .collect::<Vec<_>>();
    assert_eq!(after, before, "the device's folder changed: {stderr}");
}

#[test]
fn a_folder_over_the_server_s_size_limit_is_skipped_not_uploaded() {
    let server = MockServer::start();
    mock_auth(&server);
    empty_server(&server);
    let any = server.mock(|when, then| {
        when.method(POST).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"save":{"id":"save-x","sha256":"s","version":1}}"#);
    });
    let tmp = tempfile::tempdir().unwrap();
    let saves = tmp.path().join("saves");
    let save = saves.join("psvita/vita3k").join(VITA_TITLE);
    fs::create_dir_all(&save).unwrap();
    // A sparse file one byte over the limit: its size counts, not what is on disk.
    let big = fs::File::create(save.join("SAVEDATA.BIN")).unwrap();
    big.set_len(sgm_steamdeck_helper::folder_saves::FOLDER_SAVE_MAX_BYTES + 1)
        .unwrap();
    let config = write_config(tmp.path(), &server, &saves, Some("true"));
    let stderr = sync(&config);
    assert_eq!(any.calls(), 0, "an oversized folder was uploaded: {stderr}");
    assert!(stderr.contains("bundle limit"), "no skip line: {stderr}");
}

#[cfg(unix)]
#[test]
fn a_symlink_in_a_save_folder_is_left_out_of_the_bundle_by_name() {
    let server = MockServer::start();
    mock_auth(&server);
    empty_server(&server);
    let vita = server.mock(|when, then| {
        when.method(POST)
            .path("/saves")
            .body_includes("filename=\"PCSE00746.vitasave\"");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"save":{"id":"save-vita","sha256":"s","version":1}}"#);
    });
    let tmp = tempfile::tempdir().unwrap();
    let saves = tmp.path().join("saves");
    let save = saves.join("psvita/vita3k").join(VITA_TITLE);
    fs::create_dir_all(&save).unwrap();
    fs::write(save.join("SAVEDATA.BIN"), vec![0x33u8; 4096]).unwrap();
    std::os::unix::fs::symlink("SAVEDATA.BIN", save.join("linked.bin")).unwrap();
    let config = write_config(tmp.path(), &server, &saves, Some("true"));
    let stderr = sync(&config);
    assert_eq!(vita.calls(), 1, "{stderr}");
    assert!(
        stderr.contains("Leaving") && stderr.contains("linked.bin"),
        "the skipped symlink was not named: {stderr}"
    );
}
