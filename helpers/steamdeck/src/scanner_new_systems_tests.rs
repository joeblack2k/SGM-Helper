//! Neo Geo Pocket, Virtual Boy and Neo Geo CD saves, which the server accepts
//! (RetroSaveManager's runtime profiles `ngp/mednafen`, `virtualboy/mednafen`
//! and `neogeo-cd/retroarch`) and a device writes into `ngp`, `virtualboy` and
//! `neogeocd` folders.

use std::fs;

use super::{discover_save_files, infer_supported_console_slug};

fn save_with(dir: &str, name: &str, size: usize) -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("saves").join(dir).join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let bytes: Vec<u8> = (0..size).map(|i| (i as u8).wrapping_mul(31) | 1).collect();
    fs::write(&path, bytes).unwrap();
    (tmp, path)
}

#[test]
fn a_neo_geo_pocket_flash_save_is_ngp() {
    let (_tmp, path) = save_with("ngp", "Baseball Stars (Japan, Europe).flash", 536);
    assert_eq!(
        infer_supported_console_slug(&path, None).as_deref(),
        Some("ngp")
    );
}

#[test]
fn a_neo_geo_pocket_flash_save_is_discovered() {
    let (tmp, path) = save_with("ngp", "Baseball Stars (Japan, Europe).flash", 536);
    let found = discover_save_files(&[tmp.path().join("saves")], true).unwrap();
    assert!(
        found.contains(&path),
        "the .flash save was not discovered: {found:?}"
    );
}

#[test]
fn a_virtual_boy_save_is_virtualboy() {
    let (_tmp, path) = save_with("virtualboy", "Golf (USA).srm", 65_536);
    assert_eq!(
        infer_supported_console_slug(&path, None).as_deref(),
        Some("virtualboy")
    );
}

// "neogeocd" contains "neogeo": without its own arm ahead of the Neo Geo one,
// a Neo Geo CD save is filed as an MVS/AES save.
#[test]
fn a_neo_geo_cd_save_is_neogeo_cd_not_neogeo() {
    let (_tmp, path) = save_with("neogeocd", "Savage Reign (World).srm", 8192);
    assert_eq!(
        infer_supported_console_slug(&path, None).as_deref(),
        Some("neogeo-cd")
    );
}

#[test]
fn a_neo_geo_save_stays_neogeo() {
    let (_tmp, path) = save_with("neogeo", "Metal Slug (World).srm", 65_536);
    assert_eq!(
        infer_supported_console_slug(&path, None).as_deref(),
        Some("neogeo")
    );
}
