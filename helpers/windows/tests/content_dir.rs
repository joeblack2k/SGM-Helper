//! `SAVE_SUBDIR_<system>="{content}"`: RetroArch sorting saves by content
//! directory, where a save sits beside the name of its ROM's folder.

use std::fs;

use assert_cmd::Command;
use httpmock::Method::{GET, POST};
use httpmock::MockServer;

/// The cabinet's RetroArch: launched from LaunchBox platform folders and
/// sorting saves by content directory and by core, so a SNES save lives in
/// saves/Super Nintendo Entertainment System/Snes9x. `subdir` is its
/// SAVE_SUBDIR_snes.
fn cabinet_config(
    tmp: &tempfile::TempDir,
    server: &MockServer,
    root: &std::path::Path,
    state_dir: &std::path::Path,
    subdir: &str,
) -> std::path::PathBuf {
    let config = tmp.path().join("config-cabinet.ini");
    fs::write(
        &config,
        format!(
            "URL=\"127.0.0.1\"\nPORT=\"{}\"\nROOT=\"{}\"\nSTATE_DIR=\"{}\"\nWATCH=\"false\"\nWATCH_INTERVAL=\"1\"\n\n[source.retroarch]\nLABEL=\"RetroArch\"\nKIND=\"retroarch\"\nPROFILE=\"retroarch\"\nSAVE_PATH=\"{}\"\nROM_PATH=\"{}\"\nRECURSIVE=\"true\"\nSYSTEMS=\"snes\"\nSAVE_SUBDIR_snes=\"{}\"\n",
            server.port(),
            root.display(),
            state_dir.display(),
            root.join("saves").display(),
            root.join("roms").display(),
            subdir
        ),
    )
    .unwrap();
    config
}

fn cabinet_save_bytes(seed: u8) -> Vec<u8> {
    (0..8192u32)
        .map(|index| ((index as u8).wrapping_mul(37).wrapping_add(seed)) | 0x01)
        .collect()
}

fn cabinet_sync(server: &MockServer, config: &std::path::Path) -> String {
    server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_cab","expiresInDays":7}"#);
    });
    server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"cab@example.com"}}"#);
    });
    server.mock(|when, then| {
        when.method(GET).path("/save/latest");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"exists":false,"sha256":null,"version":null,"id":null}"#);
    });
    Command::cargo_bin("sgm-windows-helper")
        .unwrap()
        .arg("--config")
        .arg(config)
        .arg("login")
        .arg("--email")
        .arg("cab@example.com")
        .arg("--app-password")
        .arg("pw")
        .assert()
        .success();
    let output = Command::cargo_bin("sgm-windows-helper")
        .unwrap()
        .arg("--config")
        .arg(config)
        .arg("sync")
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stderr).into_owned()
}

// A SAVE_SUBDIR of the core alone pointed at saves/Snes9x, which this
// RetroArch never reads: the server's copy landed there and the game started
// without it. `{content}/Snes9x` names the folder the ROM is launched from.
#[test]
fn a_cabinet_restore_lands_under_the_platform_folder() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"total":1,"limit":100,"offset":0,"saves":[{"id":"save-cab","filename":"Super Mario World (USA).srm","displayTitle":"Super Mario World","systemSlug":"snes","sha256":"sha-cab","version":2,"latestSizeBytes":8192}]}"#);
    });
    server.mock(|when, then| {
        when.method(GET).path("/saves/download");
        then.status(200).body(cabinet_save_bytes(71));
    });

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    let live = "saves/Super Nintendo Entertainment System/Snes9x";
    for dir in [
        live,
        "saves/Snes9x",
        "roms/Super Nintendo Entertainment System",
    ] {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    fs::create_dir_all(&state_dir).unwrap();
    fs::write(
        root.join("roms/Super Nintendo Entertainment System/Super Mario World (USA).zip"),
        b"rom",
    )
    .unwrap();
    let config = cabinet_config(&tmp, &server, &root, &state_dir, "{content}/Snes9x");

    let stderr = cabinet_sync(&server, &config);

    assert_eq!(
        fs::read(root.join(live).join("Super Mario World (USA).srm")).ok(),
        Some(cabinet_save_bytes(71)),
        "the restore did not land under the platform folder:\n{stderr}"
    );
    assert!(
        !root
            .join("saves/Snes9x/Super Mario World (USA).srm")
            .exists(),
        "the restore landed in the bare core folder:\n{stderr}"
    );
}
