//! `SAVE_SUBDIR_<system>="{content}"`: RetroArch sorting saves by content
//! directory, where a save sits beside the name of its ROM's folder.

use std::fs;

use assert_cmd::Command;
use httpmock::Method::{GET, POST};
use httpmock::MockServer;

/// Eight kilobytes of non-repeating, non-text bytes: plausible for SNES and
/// refused by nothing.
fn snes_save_bytes(seed: u8) -> Vec<u8> {
    (0..8192u32)
        .map(|index| ((index as u8).wrapping_mul(37).wrapping_add(seed)) | 0x01)
        .collect()
}

fn wb_helper() -> Command {
    Command::cargo_bin("sgm-steamdeck-helper").unwrap()
}

/// A RetroDECK-shaped source: GB and GBC ROMs in their own directories, both
/// the server's `gameboy`. `subdir` is its `SAVE_SUBDIR_gameboy`.
fn content_dir_config(
    tmp: &tempfile::TempDir,
    server: &MockServer,
    root: &std::path::Path,
    state_dir: &std::path::Path,
    subdir: &str,
) -> std::path::PathBuf {
    let config = tmp.path().join("config-content-dir.ini");
    fs::write(
        &config,
        format!(
            "URL=\"127.0.0.1\"\nPORT=\"{}\"\nROOT=\"{}\"\nSTATE_DIR=\"{}\"\nWATCH=\"false\"\nWATCH_INTERVAL=\"1\"\n\n[source.retroarch]\nLABEL=\"RetroArch\"\nKIND=\"retroarch\"\nPROFILE=\"retroarch\"\nSAVE_PATH=\"{}\"\nROM_PATH=\"{}\"\nRECURSIVE=\"true\"\nSYSTEMS=\"gameboy\"\nSAVE_SUBDIR_gameboy=\"{}\"\n",
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

fn login_and_sync(config: &std::path::Path) -> String {
    wb_helper()
        .arg("--config")
        .arg(config)
        .arg("login")
        .arg("--email")
        .arg("cd@example.com")
        .arg("--app-password")
        .arg("pw")
        .assert()
        .success();
    let output = wb_helper()
        .arg("--config")
        .arg(config)
        .arg("sync")
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn content_dir_auth_mocks(server: &MockServer) {
    server.mock(|when, then| {
        when.method(POST).path("/auth/token/app-password");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"token":"tok_cd","expiresInDays":7}"#);
    });
    server.mock(|when, then| {
        when.method(GET).path("/auth/me");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"user":{"email":"cd@example.com"}}"#);
    });
    // A helper that can't hash a ROM itself asks the server for its key; the
    // fixture's ROM is a placeholder, so the server answers for it.
    server.mock(|when, then| {
        when.method(GET).path("/rom/lookup");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"count":1,"rom":{"sha1":"rom-sha","md5":"rom-md5","fileName":"rom.zip"}}"#);
    });
    server.mock(|when, then| {
        when.method(GET).path("/save/latest");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"exists":false,"sha256":null,"version":null,"id":null}"#);
    });
}

// RetroArch sorting saves by content directory writes a GBC game's save to
// saves/gbc, beside saves/gb for GB games, though the server calls both
// `gameboy`. A fixed SAVE_SUBDIR makes every GBC save a stray, so it never
// uploads; `{content}` lets the ROM's directory decide.
#[test]
fn a_save_beside_its_content_directory_uploads() {
    let server = MockServer::start();
    content_dir_auth_mocks(&server);
    server.mock(|when, then| {
        when.method(GET).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"total":0,"limit":100,"offset":0,"saves":[]}"#);
    });
    let upload = server.mock(|when, then| {
        when.method(POST).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"save":{"id":"save-cd","sha256":"sha-cd"}}"#);
    });

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    for dir in ["saves/gb", "saves/gbc", "roms/gb", "roms/gbc"] {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    fs::create_dir_all(&state_dir).unwrap();
    let gbc_save = root.join("saves/gbc/Pokemon Pinball (USA).srm");
    fs::write(&gbc_save, snes_save_bytes(41)).unwrap();
    fs::write(root.join("roms/gbc/Pokemon Pinball (USA).zip"), b"rom").unwrap();
    let config = content_dir_config(&tmp, &server, &root, &state_dir, "{content}");

    let stderr = login_and_sync(&config);

    assert_eq!(
        upload.calls(),
        1,
        "the GBC save beside its ROM's directory did not upload:\n{stderr}"
    );
    assert!(
        !stderr.contains("stray copy"),
        "the GBC save was called a stray:\n{stderr}"
    );
}

// A title whose ROM sits in both roms/gb and roms/gbc gives `{content}` two
// answers. The ROM index keeps only the first path it walked, so trusting it
// would call one copy a stray by directory order; the copies on disk decide
// instead, and two of them name the ambiguity rather than guess.
#[test]
fn a_rom_in_two_content_directories_is_not_guessed() {
    let server = MockServer::start();
    content_dir_auth_mocks(&server);
    server.mock(|when, then| {
        when.method(GET).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"total":0,"limit":100,"offset":0,"saves":[]}"#);
    });
    let upload = server.mock(|when, then| {
        when.method(POST).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"save":{"id":"save-cd","sha256":"sha-cd"}}"#);
    });

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    for dir in ["saves/gb", "saves/gbc", "roms/gb", "roms/gbc"] {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    fs::create_dir_all(&state_dir).unwrap();
    for dir in ["gb", "gbc"] {
        fs::write(
            root.join(format!("saves/{dir}/Pokemon Pinball (USA).srm")),
            snes_save_bytes(41),
        )
        .unwrap();
        fs::write(
            root.join(format!("roms/{dir}/Pokemon Pinball (USA).zip")),
            b"rom",
        )
        .unwrap();
    }
    let config = content_dir_config(&tmp, &server, &root, &state_dir, "{content}");

    let stderr = login_and_sync(&config);

    assert_eq!(upload.calls(), 0, "a copy uploaded on a guess:\n{stderr}");
    assert!(
        !stderr.contains("stray copy"),
        "one copy was called a stray by directory order:\n{stderr}"
    );
    assert!(
        stderr.contains("no live directory is declared"),
        "the ambiguity was not named:\n{stderr}"
    );
}

// A fixed SAVE_SUBDIR keeps the old rule: it names gb, and the gbc copy is a
// stray. This is the behaviour a source not sorted by content directory needs.
#[test]
fn a_fixed_save_subdir_still_decides() {
    let server = MockServer::start();
    content_dir_auth_mocks(&server);
    server.mock(|when, then| {
        when.method(GET).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"total":0,"limit":100,"offset":0,"saves":[]}"#);
    });
    let upload = server.mock(|when, then| {
        when.method(POST).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"save":{"id":"save-cd","sha256":"sha-cd"}}"#);
    });

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    for dir in ["saves/gbc", "roms/gbc"] {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    fs::create_dir_all(&state_dir).unwrap();
    fs::write(
        root.join("saves/gbc/Pokemon Pinball (USA).srm"),
        snes_save_bytes(41),
    )
    .unwrap();
    fs::write(root.join("roms/gbc/Pokemon Pinball (USA).zip"), b"rom").unwrap();
    let config = content_dir_config(&tmp, &server, &root, &state_dir, "gb");

    let stderr = login_and_sync(&config);

    assert_eq!(upload.calls(), 0, "a stray uploaded:\n{stderr}");
    assert!(
        stderr.contains("stray copy"),
        "the stray was not named:\n{stderr}"
    );
}

// The restore half of the same rule: a GBC game the server holds is written to
// saves/gbc, where RetroArch reads it, not to the declared gb.
#[test]
fn a_restore_lands_beside_its_content_directory() {
    let server = MockServer::start();
    content_dir_auth_mocks(&server);
    server.mock(|when, then| {
        when.method(GET).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"total":1,"limit":100,"offset":0,"saves":[{"id":"save-cdr","filename":"Pokemon Pinball (USA).srm","displayTitle":"Pokemon Pinball","systemSlug":"gameboy","sha256":"sha-cdr","version":2,"latestSizeBytes":8192}]}"#);
    });
    server.mock(|when, then| {
        when.method(GET).path("/saves/download");
        then.status(200).body(snes_save_bytes(53));
    });

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    for dir in ["saves/gb", "saves/gbc", "roms/gb", "roms/gbc"] {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    fs::create_dir_all(&state_dir).unwrap();
    fs::write(root.join("roms/gbc/Pokemon Pinball (USA).zip"), b"rom").unwrap();
    let config = content_dir_config(&tmp, &server, &root, &state_dir, "{content}");

    let stderr = login_and_sync(&config);

    assert_eq!(
        fs::read(root.join("saves/gbc/Pokemon Pinball (USA).srm")).ok(),
        Some(snes_save_bytes(53)),
        "the restore did not land in saves/gbc:\n{stderr}"
    );
    assert!(
        !root.join("saves/gb/Pokemon Pinball (USA).srm").exists(),
        "the restore landed in the declared gb as well:\n{stderr}"
    );
}

// A device launched from frontend platform folders: RetroArch sorting by
// content directory and by core writes saves/<platform folder>/<core>, so the
// live directory is two levels deep and names the ROM's folder.
#[test]
fn a_content_placeholder_can_sit_above_a_core_directory() {
    let server = MockServer::start();
    content_dir_auth_mocks(&server);
    server.mock(|when, then| {
        when.method(GET).path("/saves");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"success":true,"total":1,"limit":100,"offset":0,"saves":[{"id":"save-lb","filename":"Pokemon Pinball (USA).srm","displayTitle":"Pokemon Pinball","systemSlug":"gameboy","sha256":"sha-lb","version":2,"latestSizeBytes":8192}]}"#);
    });
    server.mock(|when, then| {
        when.method(GET).path("/saves/download");
        then.status(200).body(snes_save_bytes(61));
    });

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state_dir = tmp.path().join("state");
    let live_dir = "saves/Nintendo Game Boy Color/mGBA";
    for dir in [live_dir, "saves/mGBA", "roms/Nintendo Game Boy Color"] {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    fs::create_dir_all(&state_dir).unwrap();
    fs::write(
        root.join("roms/Nintendo Game Boy Color/Pokemon Pinball (USA).zip"),
        b"rom",
    )
    .unwrap();
    let config = content_dir_config(&tmp, &server, &root, &state_dir, "{content}/mGBA");

    let stderr = login_and_sync(&config);

    assert_eq!(
        fs::read(root.join(live_dir).join("Pokemon Pinball (USA).srm")).ok(),
        Some(snes_save_bytes(61)),
        "the restore did not land in <platform>/<core>:\n{stderr}"
    );
    assert!(
        !root.join("saves/mGBA/Pokemon Pinball (USA).srm").exists(),
        "the restore landed in the bare core directory:\n{stderr}"
    );
}
