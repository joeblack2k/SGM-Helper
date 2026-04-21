use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use sha1::{Digest as Sha1Digest, Sha1};
use sha2::Sha256;
use walkdir::WalkDir;

const SAVE_EXTENSIONS: &[&str] = &[
    "sav", "srm", "eep", "fla", "sa1", "rtc", "ram", "sra", "dsv", "gme",
];

const MAX_SAVE_BYTES: u64 = 16 * 1024 * 1024;

const ROM_EXTENSIONS: &[&str] = &[
    "nes", "fds", "sfc", "smc", "gb", "gbc", "gba", "n64", "z64", "v64", "nds", "md", "gen", "sms",
    "gg", "cue", "iso", "chd", "pce", "a26", "a78", "col", "bin", "zip", "7z",
];

#[derive(Debug, Clone)]
pub struct RomIndexEntry {
    pub stem: String,
    pub path: PathBuf,
}

pub fn discover_save_files(roots: &[PathBuf], recursive: bool) -> Result<Vec<PathBuf>> {
    discover_files_with_extensions(roots, SAVE_EXTENSIONS, recursive)
}

pub fn discover_rom_index(
    roots: &[PathBuf],
    recursive: bool,
) -> Result<HashMap<String, RomIndexEntry>> {
    let files = discover_files_with_extensions(roots, ROM_EXTENSIONS, recursive)?;
    let mut index = HashMap::new();
    for path in files {
        let stem = filename_stem(&path);
        if stem == "Unknown" {
            continue;
        }
        let key = stem.to_ascii_lowercase();
        index.entry(key).or_insert(RomIndexEntry { stem, path });
    }
    Ok(index)
}

fn discover_files_with_extensions(
    roots: &[PathBuf],
    extensions: &[&str],
    recursive: bool,
) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();

    for root in roots {
        if !root.exists() {
            continue;
        }

        let walker = if recursive {
            WalkDir::new(root)
        } else {
            WalkDir::new(root).max_depth(1)
        };

        for entry in walker
            .follow_links(false)
            .into_iter()
            .filter_map(|entry| entry.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            let Some(ext) = path.extension().and_then(|value| value.to_str()) else {
                continue;
            };
            if extensions
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(ext))
            {
                files.push(path.to_path_buf());
            }
        }
    }

    files.sort();
    files.dedup();
    Ok(files)
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let file =
        File::open(path).with_context(|| format!("kan bestand niet openen: {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];

    loop {
        let read = reader
            .read(&mut buffer)
            .with_context(|| format!("kan bestand niet lezen: {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }

    Ok(hex::encode(hasher.finalize()))
}

pub fn sha1_file(path: &Path) -> Result<String> {
    let file =
        File::open(path).with_context(|| format!("kan ROM niet openen: {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha1::new();
    let mut buffer = [0u8; 8192];

    loop {
        let read = reader
            .read(&mut buffer)
            .with_context(|| format!("kan ROM niet lezen: {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }

    Ok(hex::encode(hasher.finalize()))
}

pub fn md5_file(path: &Path) -> Result<String> {
    let bytes =
        std::fs::read(path).with_context(|| format!("kan ROM niet lezen: {}", path.display()))?;
    Ok(format!("{:x}", md5::compute(bytes)))
}

pub fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

pub fn filename_stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|value| value.to_str())
        .map(ToString::to_string)
        .unwrap_or_else(|| "Unknown".to_string())
}

pub fn known_save_extensions() -> BTreeSet<&'static str> {
    SAVE_EXTENSIONS.iter().copied().collect()
}

pub fn infer_system_slug(path: &Path) -> Option<String> {
    classify_supported_save(path, None).map(|value| value.system_slug)
}

pub fn infer_supported_console_slug(save_path: &Path, rom_path: Option<&Path>) -> Option<String> {
    classify_supported_save(save_path, rom_path).map(|value| value.system_slug)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveClassification {
    pub system_slug: String,
    pub evidence: String,
}

pub fn classify_supported_save(
    save_path: &Path,
    rom_path: Option<&Path>,
) -> Option<SaveClassification> {
    let save_ext = path_extension(save_path)?;

    let save_size = save_path.metadata().ok()?.len();
    if !is_plausible_save_size(save_size) || looks_plain_text(save_path) {
        return None;
    }

    if let Some(slug) = rom_path
        .and_then(path_extension)
        .and_then(system_slug_from_rom_extension)
        && is_plausible_save_for_system(&save_ext, save_size, slug)
    {
        return Some(SaveClassification {
            system_slug: slug.to_string(),
            evidence: format!("rom-extension + .{} ({} bytes)", save_ext, save_size),
        });
    }

    let save_lower = save_path.to_string_lossy().to_ascii_lowercase();
    let rom_lower = rom_path
        .map(|path| path.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let combined = format!("{} {}", save_lower, rom_lower);

    if contains_any(&combined, &["gameboy advance", "/gba/", "\\gba\\"]) {
        if is_plausible_save_for_system(&save_ext, save_size, "gba") {
            return Some(SaveClassification {
                system_slug: "gba".to_string(),
                evidence: format!("path hint gba + .{} ({} bytes)", save_ext, save_size),
            });
        }
        return None;
    }
    if contains_any(
        &combined,
        &[
            "game boy color",
            "gameboy color",
            "/gbc/",
            "\\gbc\\",
            "nintendo ds",
            "/nds/",
            "\\nds\\",
            "nintendo 64",
            "/n64/",
            "\\n64\\",
            "super nintendo",
            "/snes/",
            "\\snes\\",
            "/sfc/",
            "\\sfc\\",
            "famicom",
            "/nes/",
            "\\nes\\",
            "game boy",
            "gameboy",
            "/gb/",
            "\\gb\\",
            "melonds",
            "desmume",
            "ryujinx",
            "citra",
            "yuzu",
            "suyu",
            "dolphin",
            "mgba",
            "visualboyadvance",
            "vba",
            "bsnes",
            "snes9x",
            "fceux",
            "nestopia",
            "nintendo",
        ],
    ) {
        let slug = infer_nintendo_slug(&combined);
        if is_plausible_save_for_system(&save_ext, save_size, slug) {
            return Some(SaveClassification {
                system_slug: slug.to_string(),
                evidence: format!("path hint nintendo + .{} ({} bytes)", save_ext, save_size),
            });
        }
        return None;
    }

    if contains_any(
        &combined,
        &[
            "master system",
            "/sms/",
            "\\sms\\",
            "game gear",
            "/gg/",
            "\\gg\\",
            "genesis",
            "mega drive",
            "megadrive",
            "/md/",
            "\\md\\",
            "/gen/",
            "\\gen\\",
            "saturn",
            "dreamcast",
            "sega",
        ],
    ) {
        let slug = infer_sega_slug(&combined);
        if is_plausible_save_for_system(&save_ext, save_size, slug) {
            return Some(SaveClassification {
                system_slug: slug.to_string(),
                evidence: format!("path hint sega + .{} ({} bytes)", save_ext, save_size),
            });
        }
        return None;
    }

    if contains_any(
        &combined,
        &[
            "neo geo", "neogeo", "neo-geo", "/mvs/", "\\mvs\\", "/aes/", "\\aes\\",
        ],
    ) {
        if is_plausible_save_for_system(&save_ext, save_size, "neogeo") {
            return Some(SaveClassification {
                system_slug: "neogeo".to_string(),
                evidence: format!("path hint neogeo + .{} ({} bytes)", save_ext, save_size),
            });
        }
        return None;
    }

    if let Some(slug) = system_slug_from_save_extension(save_ext.as_str())
        && is_plausible_save_for_system(&save_ext, save_size, slug)
    {
        return Some(SaveClassification {
            system_slug: slug.to_string(),
            evidence: format!("save extension .{} ({} bytes)", save_ext, save_size),
        });
    }

    None
}

fn path_extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
}

fn system_slug_from_rom_extension(ext: String) -> Option<&'static str> {
    match ext.as_str() {
        "nes" | "fds" => Some("nes"),
        "sfc" | "smc" => Some("snes"),
        "n64" | "z64" | "v64" => Some("n64"),
        "gb" | "gbc" => Some("gameboy"),
        "gba" => Some("gba"),
        "nds" => Some("nds"),
        "md" | "gen" => Some("genesis"),
        "sms" => Some("master-system"),
        "gg" => Some("game-gear"),
        _ => None,
    }
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

fn infer_nintendo_slug(haystack: &str) -> &'static str {
    if contains_any(haystack, &["gameboy advance", "/gba/", "\\gba\\"]) {
        return "gba";
    }
    if contains_any(haystack, &["nintendo ds", "/nds/", "\\nds\\"]) {
        return "nds";
    }
    if contains_any(haystack, &["nintendo 64", "/n64/", "\\n64\\"]) {
        return "n64";
    }
    if contains_any(
        haystack,
        &["super nintendo", "/snes/", "\\snes\\", "/sfc/", "\\sfc\\"],
    ) {
        return "snes";
    }
    if contains_any(haystack, &["famicom", "/nes/", "\\nes\\"]) {
        return "nes";
    }
    "gameboy"
}

fn infer_sega_slug(haystack: &str) -> &'static str {
    if contains_any(haystack, &["master system", "/sms/", "\\sms\\"]) {
        return "master-system";
    }
    if contains_any(haystack, &["game gear", "/gg/", "\\gg\\"]) {
        return "game-gear";
    }
    "genesis"
}

fn system_slug_from_save_extension(ext: &str) -> Option<&'static str> {
    match ext {
        "eep" | "fla" | "sra" => Some("n64"),
        "dsv" => Some("nds"),
        _ => None,
    }
}

fn is_plausible_save_size(size: u64) -> bool {
    size > 0 && size <= MAX_SAVE_BYTES
}

fn is_plausible_save_for_system(ext: &str, size: u64, slug: &str) -> bool {
    if !is_plausible_save_size(size) {
        return false;
    }

    let extension_ok = match slug {
        "nes" => matches!(ext, "sav" | "srm" | "ram"),
        "snes" => matches!(ext, "srm" | "sav" | "sa1"),
        "gameboy" => matches!(ext, "sav" | "srm" | "gme" | "rtc" | "ram"),
        "gba" => matches!(ext, "sav" | "srm" | "sa1"),
        "n64" => matches!(ext, "sav" | "eep" | "fla" | "sra"),
        "nds" => matches!(ext, "sav" | "dsv"),
        "genesis" => matches!(ext, "sav" | "srm" | "ram"),
        "master-system" | "game-gear" => matches!(ext, "sav" | "srm" | "ram"),
        "neogeo" => matches!(ext, "sav" | "srm" | "ram"),
        _ => false,
    };
    if !extension_ok {
        return false;
    }

    match slug {
        "nes" => (512..=262_144).contains(&size),
        "snes" => (512..=524_288).contains(&size),
        "gameboy" => (512..=262_144).contains(&size),
        "gba" => (512..=1_048_576).contains(&size),
        "n64" => {
            if ext == "eep" {
                size == 512 || size == 2048
            } else {
                (512..=262_144).contains(&size)
            }
        }
        "nds" => (512..=16_777_216).contains(&size),
        "genesis" | "master-system" | "game-gear" => (512..=524_288).contains(&size),
        "neogeo" => (512..=2_097_152).contains(&size),
        _ => false,
    }
}

fn looks_plain_text(path: &Path) -> bool {
    let Ok(file) = File::open(path) else {
        return false;
    };
    let mut reader = BufReader::new(file);
    let mut buffer = [0u8; 4096];
    let Ok(read) = reader.read(&mut buffer) else {
        return false;
    };
    if read == 0 {
        return false;
    }

    let bytes = &buffer[..read];
    if bytes.contains(&0) {
        return false;
    }

    let printable = bytes
        .iter()
        .filter(|value| matches!(**value, b'\n' | b'\r' | b'\t' | 0x20..=0x7e))
        .count();
    printable.saturating_mul(100) >= bytes.len().saturating_mul(95)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn scanner_finds_supported_save_files() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("games")).unwrap();
        fs::write(root.join("games/test.sav"), b"a").unwrap();
        fs::write(root.join("games/ignore.txt"), b"a").unwrap();

        let files = discover_save_files(&[root.to_path_buf()], true).unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("test.sav"));
    }

    #[test]
    fn infer_system_slug_from_path() {
        let tmp = tempfile::tempdir().unwrap();
        let snes = tmp.path().join("saves/SNES/zelda.srm");
        let sega = tmp.path().join("saves/Sega/sonic.srm");
        fs::create_dir_all(snes.parent().unwrap()).unwrap();
        fs::create_dir_all(sega.parent().unwrap()).unwrap();
        fs::write(&snes, vec![0x00u8; 8192]).unwrap();
        fs::write(&sega, vec![0x00u8; 8192]).unwrap();
        assert_eq!(infer_system_slug(&snes).as_deref(), Some("snes"));
        assert_eq!(infer_system_slug(&sega).as_deref(), Some("genesis"));
    }

    #[test]
    fn unsupported_paths_are_not_classified() {
        let path = PathBuf::from("/home/deck/.steam/steam/steamapps/compatdata/242550/icudtl.dat");
        assert!(infer_supported_console_slug(&path, None).is_none());
    }

    #[test]
    fn rom_extension_can_classify_supported_console() {
        let tmp = tempfile::tempdir().unwrap();
        let save = tmp.path().join("anything.sav");
        fs::write(&save, vec![0x00u8; 32768]).unwrap();
        let rom = PathBuf::from("/roms/gb/pokemon.gb");
        assert_eq!(
            infer_supported_console_slug(&save, Some(&rom)).as_deref(),
            Some("gameboy")
        );
    }

    #[test]
    fn text_files_are_rejected_even_with_save_extension() {
        let tmp = tempfile::tempdir().unwrap();
        let save = tmp.path().join("Nintendo/notes.sav");
        fs::create_dir_all(save.parent().unwrap()).unwrap();
        fs::write(&save, b"this is clearly text and not a real save file").unwrap();

        assert!(infer_supported_console_slug(&save, None).is_none());
    }

    #[test]
    fn unsupported_extension_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let save = tmp.path().join("Nintendo/zelda.dat");
        fs::create_dir_all(save.parent().unwrap()).unwrap();
        fs::write(&save, [0x00u8; 1024]).unwrap();

        assert!(infer_supported_console_slug(&save, None).is_none());
    }
}
