use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use sha1::{Digest as Sha1Digest, Sha1};
use sha2::Sha256;
use walkdir::WalkDir;

const SAVE_EXTENSIONS: &[&str] = &[
    "sav", "srm", "state", "eep", "fla", "sa1", "dat", "rtc", "ram", "ss", "fgp", "mcr", "mc",
    "sra", "dsv", "gme",
];

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
    infer_supported_console_slug(path, None)
}

pub fn infer_supported_console_slug(save_path: &Path, rom_path: Option<&Path>) -> Option<String> {
    if let Some(slug) = rom_path
        .and_then(path_extension)
        .and_then(system_slug_from_rom_extension)
    {
        return Some(slug.to_string());
    }

    let save_lower = save_path.to_string_lossy().to_ascii_lowercase();
    let rom_lower = rom_path
        .map(|path| path.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let combined = format!("{} {}", save_lower, rom_lower);

    if contains_any(&combined, &["gameboy advance", "/gba/", "\\gba\\"]) {
        return Some("gba".to_string());
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
            "nintendo",
        ],
    ) {
        return Some(infer_nintendo_slug(&combined).to_string());
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
        return Some(infer_sega_slug(&combined).to_string());
    }

    if contains_any(
        &combined,
        &[
            "neo geo", "neogeo", "neo-geo", "/mvs/", "\\mvs\\", "/aes/", "\\aes\\",
        ],
    ) {
        return Some("neogeo".to_string());
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
        let snes = PathBuf::from("/media/fat/saves/SNES/zelda.srm");
        let sega = PathBuf::from("/media/fat/saves/Sega/sonic.srm");
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
        let save = PathBuf::from("/tmp/anything.sav");
        let rom = PathBuf::from("/roms/gb/pokemon.gb");
        assert_eq!(
            infer_supported_console_slug(&save, Some(&rom)).as_deref(),
            Some("gameboy")
        );
    }
}
