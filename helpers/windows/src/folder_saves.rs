//! Saves an emulator keeps as a folder of files, not one file: Dolphin keeps a Wii title's
//! save in its NAND at `title/<hi>/<lo>/data/`, and Vita3K keeps a Vita title's under
//! `ux0/user/00/savedata/<TITLEID>/`. Each folder is synced as one record, packed the way a
//! PPSSPP SAVEDATA folder is (`pack_psp_savedata`), with `folderSaveDir` naming the folder so
//! any device can put it back in the same place.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::scanner::{build_stored_zip, sha256_bytes, unpack_psp_savedata};

/// The two systems that keep folder saves, and where each emulator keeps them under a
/// RetroDECK-style save root (`<saves>/wii/dolphin/...`, `<saves>/psvita/vita3k/...`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderSaveKind {
    /// A Dolphin NAND title: `<root>/title/<hi>/<lo>/data/`. `folderSaveDir` is `<hi>/<lo>`.
    WiiNand,
    /// A Vita3K savedata folder: `<root>/<TITLEID>/`. `folderSaveDir` is the title ID.
    Vita3k,
}

impl FolderSaveKind {
    pub fn system_slug(self) -> &'static str {
        match self {
            FolderSaveKind::WiiNand => "wii",
            FolderSaveKind::Vita3k => "psvita",
        }
    }

    /// Where the emulator keeps these saves under a save root, in each layout it is found in:
    /// RetroDECK's (`<saves>/wii/dolphin`, `<saves>/psvita/vita3k`) first, then the emulator's
    /// own folder used as the root (Dolphin's `User/Wii`, Vita3K's `ux0/user/00/savedata`).
    pub fn roots_under(self, save_root: &Path) -> Vec<PathBuf> {
        match self {
            FolderSaveKind::WiiNand => {
                vec![save_root.join("wii").join("dolphin"), save_root.join("Wii")]
            }
            FolderSaveKind::Vita3k => vec![
                save_root.join("psvita").join("vita3k"),
                save_root
                    .join("ux0")
                    .join("user")
                    .join("00")
                    .join("savedata"),
            ],
        }
    }

    /// The emulator's folder under a save root: the first layout that exists, otherwise
    /// RetroDECK's. A restore needs it to exist and never creates it.
    pub fn root_under(self, save_root: &Path) -> PathBuf {
        let roots = self.roots_under(save_root);
        roots
            .iter()
            .find(|root| root.is_dir())
            .cloned()
            .unwrap_or_else(|| roots[0].clone())
    }

    /// The folder a record restores into, under the emulator's folder.
    pub fn folder_for(self, emulator_root: &Path, save_dir: &str) -> Option<PathBuf> {
        if !valid_folder_save_dir(self, save_dir) {
            return None;
        }
        Some(match self {
            FolderSaveKind::WiiNand => {
                let (hi, lo) = save_dir.split_once('/')?;
                emulator_root.join("title").join(hi).join(lo).join("data")
            }
            FolderSaveKind::Vita3k => emulator_root.join(save_dir),
        })
    }

    pub fn from_system_slug(slug: &str) -> Option<FolderSaveKind> {
        match slug {
            "wii" => Some(FolderSaveKind::WiiNand),
            "psvita" => Some(FolderSaveKind::Vita3k),
            _ => None,
        }
    }

    fn filename_suffix(self) -> &'static str {
        // Never `.zip`: the server explodes a zip it is not told is a bundle.
        match self {
            FolderSaveKind::WiiNand => "wiinand",
            FolderSaveKind::Vita3k => "vitasave",
        }
    }
}

/// The NAND title groups that hold game saves: discs (00010000), channels and WiiWare
/// (00010001) and game data channels (00010004). System titles (00000001) and the rest are
/// the console's own and are never synced.
const WII_GAME_TITLE_GROUPS: &[&str] = &["00010000", "00010001", "00010004"];

fn is_hex8(value: &str) -> bool {
    value.len() == 8
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn is_vita_title_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 9
        && bytes[..4].iter().all(u8::is_ascii_uppercase)
        && bytes[4..].iter().all(u8::is_ascii_digit)
}

/// A `folderSaveDir` the server accepts for the kind: it keeps a restore inside the
/// emulator's folder.
pub fn valid_folder_save_dir(kind: FolderSaveKind, save_dir: &str) -> bool {
    match kind {
        FolderSaveKind::WiiNand => save_dir
            .split_once('/')
            .map(|(hi, lo)| is_hex8(hi) && is_hex8(lo) && WII_GAME_TITLE_GROUPS.contains(&hi))
            .unwrap_or(false),
        FolderSaveKind::Vita3k => is_vita_title_id(save_dir),
    }
}

/// One folder save found on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderSave {
    pub kind: FolderSaveKind,
    pub save_dir: String,
    pub path: PathBuf,
}

impl FolderSave {
    /// The line key a folder's records share on the server.
    pub fn line_key(&self) -> String {
        folder_line_key(self.kind, &self.save_dir)
    }

    pub fn filename(&self) -> String {
        format!(
            "{}.{}",
            self.save_dir.replace('/', "-"),
            self.kind.filename_suffix()
        )
    }
}

pub fn folder_line_key(kind: FolderSaveKind, save_dir: &str) -> String {
    format!("{}-folder:{}", kind.system_slug(), save_dir)
}

/// True when a folder holds at least one regular file: a folder an emulator made and never
/// wrote a file into is not a save.
fn has_regular_file(dir: &Path) -> bool {
    fs::read_dir(dir)
        .map(|entries| {
            entries.flatten().any(|entry| {
                entry
                    .file_type()
                    .map(|kind| kind.is_file())
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

fn sorted_subdirs(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
                .filter_map(|entry| {
                    entry
                        .file_name()
                        .into_string()
                        .ok()
                        .map(|name| (name, entry.path()))
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// Every folder save under `save_roots`: Dolphin NAND game titles with data, and Vita3K
/// savedata folders with a file in them.
pub fn discover_folder_saves(save_roots: &[PathBuf]) -> Vec<FolderSave> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for save_root in save_roots {
        for nand in FolderSaveKind::WiiNand.roots_under(save_root) {
            for group in WII_GAME_TITLE_GROUPS {
                for (lo, title_dir) in sorted_subdirs(&nand.join("title").join(group)) {
                    let data = title_dir.join("data");
                    let save_dir = format!("{group}/{lo}");
                    if valid_folder_save_dir(FolderSaveKind::WiiNand, &save_dir)
                        && has_regular_file(&data)
                        && seen.insert(data.clone())
                    {
                        out.push(FolderSave {
                            kind: FolderSaveKind::WiiNand,
                            save_dir,
                            path: data,
                        });
                    }
                }
            }
        }
        for vita in FolderSaveKind::Vita3k.roots_under(save_root) {
            for (title_id, dir) in sorted_subdirs(&vita) {
                if is_vita_title_id(&title_id) && has_regular_file(&dir) && seen.insert(dir.clone())
                {
                    out.push(FolderSave {
                        kind: FolderSaveKind::Vita3k,
                        save_dir: title_id,
                        path: dir,
                    });
                }
            }
        }
    }
    out
}

/// The emulator folder a discovered folder save sits in, so a restore over it writes back
/// to the same place: the NAND root above `title/<hi>/<lo>/data`, or the Vita3K savedata root.
pub fn emulator_root_of(save: &FolderSave) -> Option<PathBuf> {
    let levels = match save.kind {
        FolderSaveKind::WiiNand => 4,
        FolderSaveKind::Vita3k => 1,
    };
    save.path.ancestors().nth(levels).map(Path::to_path_buf)
}

/// Pack a folder's regular files into one payload, and its sha256: the same deterministic
/// stored zip a PSP SAVEDATA folder uses, so equal folders always hash equal.
pub fn pack_folder_save(dir: &Path) -> Result<(Vec<u8>, String)> {
    let mut entries = Vec::new();
    for entry in
        fs::read_dir(dir).with_context(|| format!("cannot read save folder: {}", dir.display()))?
    {
        let entry = entry.with_context(|| format!("cannot read save folder: {}", dir.display()))?;
        if !entry
            .file_type()
            .map(|kind| kind.is_file())
            .unwrap_or(false)
        {
            continue;
        }
        let name = entry
            .file_name()
            .into_string()
            .map_err(|name| anyhow::anyhow!("save file has no UTF-8 name: {:?}", name))?;
        let bytes = fs::read(entry.path())
            .with_context(|| format!("cannot read file: {}", entry.path().display()))?;
        entries.push((name, bytes));
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    let bytes = build_stored_zip(&entries)?;
    let sha = sha256_bytes(&bytes);
    Ok((bytes, sha))
}

fn flat_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name.trim() == name
        && !name.contains("..")
        && !name.chars().any(|ch| matches!(ch, '/' | '\\' | ':' | '\0'))
}

/// Replace a save folder with the files a bundle holds. Every entry is checked before
/// anything is written; the files go to `<folder>.rsm-tmp` beside it, the old folder is
/// removed, and the new one renamed into place. The emulator's folder (the NAND or Vita3K
/// savedata root) must already exist; a restore never creates it. Inside it, the title's own
/// folders are created as Dolphin and Vita3K would.
pub fn restore_folder_save(
    emulator_root: &Path,
    target_dir: &Path,
    bundle: &[u8],
) -> Result<usize> {
    if !emulator_root.is_dir() {
        anyhow::bail!(
            "no emulator save folder at {}; a restore does not create one",
            emulator_root.display()
        );
    }
    if !target_dir.starts_with(emulator_root) {
        anyhow::bail!(
            "{} is not inside {}; nothing was written",
            target_dir.display(),
            emulator_root.display()
        );
    }
    let entries = unpack_psp_savedata(bundle)?;
    if entries.is_empty() {
        anyhow::bail!("folder bundle holds no files; nothing was written");
    }
    let mut seen = HashSet::new();
    for (name, _) in &entries {
        if !flat_name(name) {
            anyhow::bail!(
                "folder bundle entry {:?} is not a plain file name; nothing was written",
                name
            );
        }
        if !seen.insert(name.clone()) {
            anyhow::bail!("folder bundle holds {:?} twice; nothing was written", name);
        }
    }
    let parent = target_dir
        .parent()
        .with_context(|| format!("no parent for {}", target_dir.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
    let leaf = target_dir
        .file_name()
        .and_then(|name| name.to_str())
        .with_context(|| format!("no folder name in {}", target_dir.display()))?;
    let staging = parent.join(format!("{leaf}.rsm-tmp"));
    remove_path_if_present(&staging)?;
    fs::create_dir(&staging).with_context(|| format!("cannot create {}", staging.display()))?;
    let written = entries.iter().try_for_each(|(name, bytes)| {
        fs::write(staging.join(name), bytes)
            .with_context(|| format!("cannot write {}", staging.join(name).display()))
    });
    if let Err(err) = written {
        let _ = fs::remove_dir_all(&staging);
        return Err(err);
    }
    remove_path_if_present(target_dir)?;
    fs::rename(&staging, target_dir).with_context(|| {
        format!(
            "cannot rename {} to {}",
            staging.display(),
            target_dir.display()
        )
    })?;
    Ok(entries.len())
}

fn remove_path_if_present(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path)
            .with_context(|| format!("cannot remove folder: {}", path.display())),
        Ok(_) => {
            fs::remove_file(path).with_context(|| format!("cannot remove file: {}", path.display()))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("cannot read path: {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    #[test]
    fn finds_dolphin_nand_game_titles_and_vita3k_savedata_and_nothing_else() {
        let root = tempfile::tempdir().unwrap();
        let saves = root.path();
        // RetroDECK's own layout: Dolphin's NAND root and Vita3K's savedata under saves/.
        write(
            &saves.join("wii/dolphin/title/00010001/57435645/data/savedata.dat"),
            b"cave",
        );
        write(
            &saves.join("wii/dolphin/title/00010001/57435645/data/banner.bin"),
            b"WIBN",
        );
        write(
            &saves.join("wii/dolphin/title/00000001/00000002/data/setting.txt"),
            b"sys",
        );
        fs::create_dir_all(saves.join("wii/dolphin/title/00010000/52534245/data")).unwrap();
        write(&saves.join("psvita/vita3k/PCSE00746/SAVEDATA.BIN"), b"vita");
        write(&saves.join("psvita/vita3k/not-a-title/x.bin"), b"x");

        let found = discover_folder_saves(&[saves.to_path_buf()]);
        let dirs: Vec<(&str, &str)> = found
            .iter()
            .map(|save| (save.kind.system_slug(), save.save_dir.as_str()))
            .collect();
        assert_eq!(
            dirs,
            vec![("wii", "00010001/57435645"), ("psvita", "PCSE00746")]
        );
    }

    #[test]
    fn a_dolphin_user_folder_and_a_vita3k_ux0_root_are_found_as_well() {
        let root = tempfile::tempdir().unwrap();
        let user = root.path();
        // Dolphin's own User folder as the save root (the arcade's layout), and Vita3K's.
        write(
            &user.join("Wii/title/00010000/52534245/data/RSBE.dat"),
            b"brawl",
        );
        write(
            &user.join("ux0/user/00/savedata/PCSE00746/SAVEDATA.BIN"),
            b"vita",
        );
        let found = discover_folder_saves(&[user.to_path_buf()]);
        let dirs: Vec<(&str, &str)> = found
            .iter()
            .map(|save| (save.kind.system_slug(), save.save_dir.as_str()))
            .collect();
        assert_eq!(
            dirs,
            vec![("wii", "00010000/52534245"), ("psvita", "PCSE00746")]
        );
        assert_eq!(FolderSaveKind::WiiNand.root_under(user), user.join("Wii"));
        assert_eq!(emulator_root_of(&found[0]).unwrap(), user.join("Wii"));
    }

    #[test]
    fn a_packed_folder_restores_to_the_same_bytes_in_the_same_place() {
        let device_a = tempfile::tempdir().unwrap();
        let data = device_a
            .path()
            .join("wii/dolphin/title/00010001/57435645/data");
        write(&data.join("savedata.dat"), &[7u8; 131104]);
        write(&data.join("banner.bin"), b"WIBN banner");
        let (bundle, sha) = pack_folder_save(&data).unwrap();
        assert_eq!(
            pack_folder_save(&data).unwrap().1,
            sha,
            "packing is deterministic"
        );

        let device_b = tempfile::tempdir().unwrap();
        let nand_b = FolderSaveKind::WiiNand.root_under(device_b.path());
        fs::create_dir_all(&nand_b).unwrap();
        let target = FolderSaveKind::WiiNand
            .folder_for(&nand_b, "00010001/57435645")
            .unwrap();
        assert_eq!(restore_folder_save(&nand_b, &target, &bundle).unwrap(), 2);
        assert_eq!(
            fs::read(target.join("savedata.dat")).unwrap(),
            vec![7u8; 131104]
        );
        assert_eq!(fs::read(target.join("banner.bin")).unwrap(), b"WIBN banner");
        assert_eq!(pack_folder_save(&target).unwrap().1, sha);
    }

    #[test]
    fn a_restore_never_creates_the_emulator_folder_or_leaves_it() {
        let device = tempfile::tempdir().unwrap();
        let nand = FolderSaveKind::WiiNand.root_under(device.path());
        let target = FolderSaveKind::WiiNand
            .folder_for(&nand, "00010001/57435645")
            .unwrap();
        let bundle = build_stored_zip(&[("a.dat".to_string(), vec![1, 2, 3])]).unwrap();
        assert!(restore_folder_save(&nand, &target, &bundle).is_err());
        assert!(!nand.exists());
        fs::create_dir_all(&nand).unwrap();
        let outside = device.path().join("elsewhere");
        assert!(restore_folder_save(&nand, &outside, &bundle).is_err());
    }

    #[test]
    fn only_folder_ids_the_server_accepts_name_a_restore_target() {
        let root = Path::new("/nand");
        assert!(
            FolderSaveKind::WiiNand
                .folder_for(root, "00010001/57435645")
                .is_some()
        );
        for bad in [
            "../../etc",
            "00000001/00000002",
            "57435645",
            "00010001/5743564Z",
        ] {
            assert!(
                FolderSaveKind::WiiNand.folder_for(root, bad).is_none(),
                "{bad}"
            );
        }
        assert!(
            FolderSaveKind::Vita3k
                .folder_for(root, "PCSE00746")
                .is_some()
        );
        for bad in ["pcse00746", "PCSE0074", "../PCSE00746"] {
            assert!(
                FolderSaveKind::Vita3k.folder_for(root, bad).is_none(),
                "{bad}"
            );
        }
    }
}
