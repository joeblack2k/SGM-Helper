//! A PortMaster port's save lives under `ports/<port>/conf/`, in the folder its engine writes
//! (`conf/love/CurseOfTheArrow/` for a LÖVE game). With FOLDER_SAVE_BUNDLES on, a Ports
//! source syncs each port's conf/ folder as one bundle keyed by the port's name, through the
//! real sync command against a stand-in server.

use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use httpmock::Method::{GET, POST};
use httpmock::MockServer;

fn mock_auth(server: &MockServer) {
    server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_ports","expiresInDays":7}"#);
    });
    server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"ports@example.com"}}"#);
    });
    server.mock(|when, then| {
        when.method(GET).path("/rom/lookup");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"count":0}"#);
    });
}

fn install_port(ports: &Path, name: &str) -> PathBuf {
    let port = ports.join(name);
    fs::create_dir_all(&port).unwrap();
    fs::write(
        port.join("port.json"),
        format!("{{\"version\": 4, \"name\": \"{name}.zip\"}}"),
    )
    .unwrap();
    port
}

fn write_config(tmp: &Path, server: &MockServer, ports_home: &Path) -> PathBuf {
    let state_dir = tmp.join("state");
    fs::create_dir_all(&state_dir).unwrap();
    let config = tmp.join("config.ini");
    let body = format!(
        "URL=\"127.0.0.1\"\nPORT=\"{}\"\nROOT=\"{}\"\nSTATE_DIR=\"{}\"\nWATCH=\"false\"\nWATCH_INTERVAL=\"1\"\nFOLDER_SAVE_BUNDLES=\"true\"\n\n[source.portmaster]\nLABEL=\"PortMaster\"\nKIND=\"ports\"\nPROFILE=\"generic\"\nSAVE_PATH=\"{}\"\nROM_PATH=\"{}\"\nRECURSIVE=\"true\"\nSYSTEMS=\"ports\"\nCREATE_MISSING_SYSTEM_DIRS=\"false\"\nMANAGED=\"false\"\nORIGIN=\"test\"\n",
        server.port(),
        ports_home.display(),
        state_dir.display(),
        ports_home.display(),
        ports_home.display(),
    );
    fs::write(&config, body).unwrap();
    Command::cargo_bin("sgm-steamdeck-helper")
        .unwrap()
        .arg("--config")
        .arg(&config)
        .arg("login")
        .arg("--email")
        .arg("ports@example.com")
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

#[test]
fn a_portmaster_port_s_conf_folder_uploads_as_one_bundle_named_for_the_port() {
    let server = MockServer::start();
    mock_auth(&server);
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
    let upload = server.mock(|when, then| {
        when.method(POST)
            .path("/saves")
            .body_includes("name=\"folderSaveDir\"")
            .body_includes("curseofthearrow")
            .body_includes("filename=\"curseofthearrow.portconf\"")
            .body_includes("ports-folder:curseofthearrow")
            .body_includes("love/CurseOfTheArrow/save.lua");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"save":{"id":"save-arrow","sha256":"s","version":1}}"#);
    });
    let any_upload = server.mock(|when, then| {
        when.method(POST).path("/saves");
        then.status(500);
    });

    let tmp = tempfile::tempdir().unwrap();
    // RetroDECK's home: PortMaster/ports/<port>/ under it.
    let home = tmp.path().join("retrodeck");
    let ports = home.join("PortMaster/ports");
    let arrow = install_port(&ports, "curseofthearrow");
    let game = arrow.join("conf/love/CurseOfTheArrow");
    fs::create_dir_all(&game).unwrap();
    fs::write(game.join(".gitkeep"), b"").unwrap();
    fs::write(game.join("save.lua"), b"return { level = 7 }").unwrap();
    fs::write(arrow.join("log.txt"), b"launch log").unwrap();
    // A port never played: its conf/ holds only the .gitkeep it ships with.
    let fresh = install_port(&ports, "superhexagon");
    fs::create_dir_all(fresh.join("conf/SuperHexagon")).unwrap();
    fs::write(fresh.join("conf/SuperHexagon/.gitkeep"), b"").unwrap();

    let config = write_config(tmp.path(), &server, &home);
    let stderr = sync(&config);

    assert_eq!(
        upload.calls(),
        1,
        "conf/ was not uploaded as one bundle: {stderr}"
    );
    assert_eq!(
        any_upload.calls(),
        0,
        "something else was uploaded: {stderr}"
    );
}

#[test]
fn a_port_conf_record_restores_into_the_installed_port_with_identical_bytes() {
    let source = tempfile::tempdir().unwrap();
    let conf_a = source.path().join("conf");
    fs::create_dir_all(conf_a.join("love/CurseOfTheArrow")).unwrap();
    fs::write(
        conf_a.join("love/CurseOfTheArrow/save.lua"),
        b"return { level = 7 }",
    )
    .unwrap();
    let (bundle, _) = sgm_steamdeck_helper::folder_saves::pack_folder_save(&conf_a).unwrap();

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
        "total": 2,
        "saves": [{
            "id": "save-arrow",
            "filename": "curseofthearrow.portconf",
            "systemSlug": "ports",
            "game": {"system": {"slug": "ports"}, "name": "curseofthearrow"},
            "displayTitle": "curseofthearrow",
            "sha256": "arrow",
            "version": 1,
            "metadata": {"rsm": {"folder": {"saveDir": "curseofthearrow", "bundle": "folder-zip"}}}
        }, {
            "id": "save-hexagon",
            "filename": "superhexagon.portconf",
            "systemSlug": "ports",
            "game": {"system": {"slug": "ports"}, "name": "superhexagon"},
            "displayTitle": "superhexagon",
            "sha256": "hex",
            "version": 1,
            "metadata": {"rsm": {"folder": {"saveDir": "superhexagon", "bundle": "folder-zip"}}}
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
            .query_param("id", "save-arrow");
        then.status(200).body(bundle.clone());
    });
    let hexagon = server.mock(|when, then| {
        when.method(GET)
            .path("/saves/download")
            .query_param("id", "save-hexagon");
        then.status(200).body("x");
    });

    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("retrodeck");
    let ports = home.join("PortMaster/ports");
    // Installed, never played: the shipped .gitkeep only. Super Hexagon is not installed.
    let arrow = install_port(&ports, "curseofthearrow");
    fs::create_dir_all(arrow.join("conf/love/CurseOfTheArrow")).unwrap();
    fs::write(arrow.join("conf/love/CurseOfTheArrow/.gitkeep"), b"").unwrap();
    let config = write_config(tmp.path(), &server, &home);
    let stderr = sync(&config);

    assert_eq!(
        download.calls(),
        1,
        "the port's bundle was not downloaded: {stderr}"
    );
    assert_eq!(
        fs::read(arrow.join("conf/love/CurseOfTheArrow/save.lua")).unwrap(),
        b"return { level = 7 }",
        "save.lua is not the same bytes: {stderr}"
    );
    assert_eq!(
        hexagon.calls(),
        0,
        "a port not installed was restored: {stderr}"
    );
    assert!(!ports.join("superhexagon").exists());
}
