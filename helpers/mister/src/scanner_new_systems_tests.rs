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

// `.flash` is discovered everywhere now, but only an `ngp` path makes it a
// save: one outside any hinted folder is classified as nothing, so it is
// skipped rather than uploaded under some other system.
#[test]
fn a_flash_file_outside_a_neo_geo_pocket_folder_is_not_a_save() {
    let (_tmp, path) = save_with("misc", "Firmware.flash", 536);
    assert_eq!(infer_supported_console_slug(&path, None), None);
}

// Atari Lynx, WonderSwan, SG-1000 and ColecoVision, under the folder names the
// devices write: MiSTer's saves/ (AtariLynx, WonderSwan, WonderSwanColor,
// Coleco), RetroDECK/ES-DE's (atarilynx, wonderswancolor, sg-1000,
// colecovision) and a RetroArch core folder. Each must come out as the slug
// RetroSaveManager knows, so the upload names a system and a runtime profile.
#[test]
fn lynx_wonderswan_sg1000_and_colecovision_saves_are_classified_where_devices_write_them() {
    for (dir, name, size, want) in [
        (
            "AtariLynx",
            "Gates of Zendocon (USA).sav",
            128,
            "atari-lynx",
        ),
        (
            "atarilynx",
            "Gates of Zendocon (USA).srm",
            128,
            "atari-lynx",
        ),
        ("Handy", "Gates of Zendocon (USA).srm", 128, "atari-lynx"),
        ("WonderSwan", "Gunpey (Japan).sav", 32_768, "wonderswan"),
        (
            "WonderSwanColor",
            "Final Fantasy (Japan).sav",
            65_536,
            "wonderswan",
        ),
        (
            "wonderswancolor",
            "Final Fantasy (Japan).srm",
            65_536,
            "wonderswan",
        ),
        ("sg-1000", "Girl's Garden (Japan).sav", 8192, "sg-1000"),
        ("SG1000", "Girl's Garden (Japan).sav", 8192, "sg-1000"),
        ("Coleco", "Boxxle (USA).sav", 2048, "colecovision"),
        ("colecovision", "Boxxle (USA).srm", 2048, "colecovision"),
    ] {
        let (_tmp, path) = save_with(dir, name, size);
        assert_eq!(
            infer_supported_console_slug(&path, None).as_deref(),
            Some(want),
            "{dir}/{name}"
        );
    }
}

// The Sega arm's "sega" needle comes after these: an SG-1000 save in an
// ES-DE "sega sg-1000" style folder is still SG-1000, not Master System.
#[test]
fn an_sg1000_save_is_not_a_sega_cartridge_save() {
    let (_tmp, path) = save_with("Sega SG-1000", "Girl's Garden (Japan).sav", 8192);
    assert_eq!(
        infer_supported_console_slug(&path, None).as_deref(),
        Some("sg-1000")
    );
}

// Handy writes the Lynx EEPROM as .eeprom; it must be discovered at all.
#[test]
fn a_lynx_eeprom_save_is_discovered() {
    let (tmp, path) = save_with("atarilynx", "Gates of Zendocon (USA).eeprom", 128);
    let found = discover_save_files(&[tmp.path().join("saves")], true).unwrap();
    assert!(
        found.contains(&path),
        "the .eeprom save was not discovered: {found:?}"
    );
}

// 3DO (Opera keeps one NVRAM image per game under 3do/opera/per_game) and
// Atari Jaguar (Virtual Jaguar's EEPROM .srm) were uploaded by older builds and
// then classified by none: a new save of either was skipped on every device.
#[test]
fn an_opera_nvram_save_is_3do() {
    let (_tmp, path) = save_with("3do/opera/per_game", "Bust-A-Move (USA).0.srm", 32_768);
    assert_eq!(
        infer_supported_console_slug(&path, None).as_deref(),
        Some("3do")
    );
}

#[test]
fn a_virtual_jaguar_eeprom_save_is_atari_jaguar() {
    let (_tmp, path) = save_with("atarijaguar", "Doom (World).srm", 128);
    assert_eq!(
        infer_supported_console_slug(&path, None).as_deref(),
        Some("atari-jaguar")
    );
}

#[test]
fn a_jaguar_save_in_a_retroarch_core_folder_is_atari_jaguar() {
    let (_tmp, path) = save_with("Virtual Jaguar", "Iron Soldier (World).srm", 128);
    assert_eq!(
        infer_supported_console_slug(&path, None).as_deref(),
        Some("atari-jaguar")
    );
}
