use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

const SAVE_EXTENSIONS: &[&str] = &[
    "sav", "srm", "state", "eep", "fla", "sa1", "dat", "rtc", "ram", "ss", "fgp",
];

pub fn discover_save_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in WalkDir::new(root)
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
        if SAVE_EXTENSIONS
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(ext))
        {
            files.push(path.to_path_buf());
        }
    }

    files.sort();
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

pub fn known_extensions() -> BTreeSet<&'static str> {
    SAVE_EXTENSIONS.iter().copied().collect()
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

        let files = discover_save_files(root).unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("test.sav"));
    }
}
