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
    let lower_parts: Vec<String> = path
        .components()
        .filter_map(|part| part.as_os_str().to_str())
        .map(|part| part.to_ascii_lowercase())
        .collect();

    let lookup = |needles: &[&str]| -> bool {
        lower_parts
            .iter()
            .any(|part| needles.iter().any(|needle| part.contains(needle)))
    };

    if lookup(&["snes", "super nintendo", "sfc"]) {
        return Some("snes".to_string());
    }
    if lookup(&["nes", "famicom"]) {
        return Some("nes".to_string());
    }
    if lookup(&["gameboy", "gbc", "gb"]) {
        return Some("gameboy".to_string());
    }
    if lookup(&["gba", "gameboy advance"]) {
        return Some("gba".to_string());
    }
    if lookup(&["n64", "nintendo 64"]) {
        return Some("n64".to_string());
    }
    if lookup(&["genesis", "megadrive", "mega drive", "md"]) {
        return Some("genesis".to_string());
    }
    if lookup(&["psx", "ps1", "playstation"]) {
        return Some("psx".to_string());
    }
    if lookup(&["nds", "nintendo ds"]) {
        return Some("nds".to_string());
    }

    None
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
        let psx = PathBuf::from("/media/fat/saves/PSX/ff7.mcr");
        assert_eq!(infer_system_slug(&snes).as_deref(), Some("snes"));
        assert_eq!(infer_system_slug(&psx).as_deref(), Some("psx"));
    }
}
