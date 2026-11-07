//! Saves an emulator keeps as a folder of files, not one file: Dolphin keeps a Wii title's
//! save in its NAND at `title/<hi>/<lo>/data/`, and Vita3K keeps a Vita title's under
//! `ux0/user/00/savedata/<TITLEID>/`, and a PortMaster port keeps its own under
//! `ports/<port>/conf/`. Each folder is synced as one record, packed the way a
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
    /// A PortMaster port's `conf/` folder: `<ports>/<port>/conf/`, where the port's engine
    /// keeps its save (`conf/love/<Game>/` for LÖVE, `conf/godot/app_userdata/<Game>/` for
    /// Godot). `folderSaveDir` is the port's name: port.json's `name` without `.zip`.
    PortConf,
}

impl FolderSaveKind {
    pub fn system_slug(self) -> &'static str {
        match self {
            FolderSaveKind::WiiNand => "wii",
            FolderSaveKind::Vita3k => "psvita",
            FolderSaveKind::PortConf => "ports",
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
            // A Ports source's save root is the ports folder itself, PortMaster's folder
            // above it, or the RetroDECK home above that.
            FolderSaveKind::PortConf => vec![
                save_root.to_path_buf(),
                save_root.join("ports"),
                save_root.join("PortMaster").join("ports"),
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

    /// The emulator's folder a record for `save_dir` restores under. For a port, the ports
    /// folder that holds that port; otherwise [`Self::root_under`].
    pub fn restore_root_under(self, save_root: &Path, save_dir: &str) -> PathBuf {
        match self {
            FolderSaveKind::PortConf => {
                let roots = self.roots_under(save_root);
                roots
                    .iter()
                    .find(|root| root.join(save_dir).join("port.json").is_file())
                    .cloned()
                    .unwrap_or_else(|| roots[0].clone())
            }
            _ => self.root_under(save_root),
        }
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
            FolderSaveKind::PortConf => emulator_root.join(save_dir).join("conf"),
        })
    }

    pub fn from_system_slug(slug: &str) -> Option<FolderSaveKind> {
        match slug {
            "wii" => Some(FolderSaveKind::WiiNand),
            "psvita" => Some(FolderSaveKind::Vita3k),
            "ports" => Some(FolderSaveKind::PortConf),
            _ => None,
        }
    }

    fn filename_suffix(self) -> &'static str {
        // Never `.zip`: the server explodes a zip it is not told is a bundle.
        match self {
            FolderSaveKind::WiiNand => "wiinand",
            FolderSaveKind::Vita3k => "vitasave",
            FolderSaveKind::PortConf => "portconf",
        }
    }

    /// A file or folder the emulator writes that is not part of the save. A port writes its
    /// logs into `conf/`; they change every launch and are never carried.
    fn leaves_out(self, name: &str, is_dir: bool) -> bool {
        match self {
            FolderSaveKind::WiiNand | FolderSaveKind::Vita3k => false,
            FolderSaveKind::PortConf => {
                if is_dir {
                    name == "logs"
                } else {
                    name == ".gitkeep" || name == "log.txt" || name.ends_with(".log")
                }
            }
        }
    }

    /// Whether a restore may write `target` under `emulator_root`. Dolphin's NAND and
    /// Vita3K's savedata folder must exist; a port must be installed (its folder holds
    /// port.json), so a record never creates a port that is not on the device.
    pub fn restore_target_ready(self, emulator_root: &Path, target: &Path) -> bool {
        match self {
            FolderSaveKind::WiiNand | FolderSaveKind::Vita3k => emulator_root.is_dir(),
            FolderSaveKind::PortConf => target
                .parent()
                .map(|port| port.join("port.json").is_file())
                .unwrap_or(false),
        }
    }

    /// Whether the device already holds this save, so the upload pass compares it rather
    /// than a cloud-only restore writing it. A port ships an empty `conf/` (a `.gitkeep`), so
    /// for a port only a save file counts.
    pub fn holds_save(self, target: &Path) -> bool {
        match self {
            FolderSaveKind::WiiNand | FolderSaveKind::Vita3k => target.is_dir(),
            FolderSaveKind::PortConf => has_save_file(self, target),
        }
    }
}

/// A port's name as its save is keyed: port.json's `name` without `.zip`, one plain
/// lowercase folder name.
fn is_port_name(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes.iter().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
        && !value.contains("..")
}

/// The name in a port folder's port.json, without `.zip`: `curseofthearrow.zip` keys as
/// `curseofthearrow`. None when there is no port.json or it names no port.
fn port_json_name(port_dir: &Path) -> Option<String> {
    let text = fs::read_to_string(port_dir.join("port.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let name = value.get("name")?.as_str()?.trim();
    let name = name.strip_suffix(".zip").unwrap_or(name);
    Some(name.to_string())
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
        FolderSaveKind::PortConf => is_port_name(save_dir),
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

/// True when a folder or any folder in it holds a file the kind carries: a port's `conf/`
/// holding only a `.gitkeep` or logs is not a save.
fn has_save_file(kind: FolderSaveKind, dir: &Path) -> bool {
    fs::read_dir(dir)
        .map(|entries| {
            entries.flatten().any(|entry| {
                let name = entry.file_name().to_string_lossy().to_string();
                match entry.file_type() {
                    Ok(t) if t.is_file() => !kind.leaves_out(&name, false),
                    Ok(t) if t.is_dir() => {
                        !kind.leaves_out(&name, true) && has_save_file(kind, &entry.path())
                    }
                    _ => false,
                }
            })
        })
        .unwrap_or(false)
}

/// True when a folder or any folder in it holds a regular file: a folder an emulator made
/// and never wrote a file into is not a save.
fn has_regular_file(dir: &Path) -> bool {
    fs::read_dir(dir)
        .map(|entries| {
            entries.flatten().any(|entry| match entry.file_type() {
                Ok(kind) if kind.is_file() => true,
                Ok(kind) if kind.is_dir() => has_regular_file(&entry.path()),
                _ => false,
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

/// Every PortMaster port under `save_roots` whose `conf/` holds a save: each folder with a
/// port.json naming it, whose folder name is that name. A port named otherwise is left out,
/// since a restore puts the save back by name.
pub fn discover_port_conf_saves(save_roots: &[PathBuf]) -> Vec<FolderSave> {
    let kind = FolderSaveKind::PortConf;
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for save_root in save_roots {
        for ports in kind.roots_under(save_root) {
            for (dir_name, port_dir) in sorted_subdirs(&ports) {
                let Some(name) = port_json_name(&port_dir) else {
                    continue;
                };
                if name != dir_name || !is_port_name(&name) {
                    if !name.is_empty() && has_save_file(kind, &port_dir.join("conf")) {
                        eprintln!(
                            "Leaving port {} out: port.json names it {:?}, not a plain lowercase name matching its folder",
                            port_dir.display(),
                            name
                        );
                    }
                    continue;
                }
                let conf = port_dir.join("conf");
                if has_save_file(kind, &conf) && seen.insert(conf.clone()) {
                    out.push(FolderSave {
                        kind,
                        save_dir: name,
                        path: conf,
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
        FolderSaveKind::PortConf => 2,
    };
    save.path.ancestors().nth(levels).map(Path::to_path_buf)
}

/// The most file bytes a folder bundle carries, the same cap the server enforces: a larger
/// folder is skipped rather than uploaded and refused.
pub const FOLDER_SAVE_MAX_BYTES: u64 = 64 << 20;

/// Every regular file in a save folder and the folders in it, as its path relative to the
/// folder (`sce_sys/param.sfo`) and its size, sorted by path. Symlinks and other entries
/// are not save files and are left out.
pub fn folder_save_files(dir: &Path) -> Result<Vec<(String, PathBuf, u64)>> {
    folder_save_files_of(FolderSaveKind::WiiNand, dir)
}

/// [`folder_save_files`], leaving out what the kind does not carry (a port's logs).
pub fn folder_save_files_of(
    kind: FolderSaveKind,
    dir: &Path,
) -> Result<Vec<(String, PathBuf, u64)>> {
    fn walk(
        save_kind: FolderSaveKind,
        dir: &Path,
        prefix: &str,
        out: &mut Vec<(String, PathBuf, u64)>,
    ) -> Result<()> {
        for entry in fs::read_dir(dir)
            .with_context(|| format!("cannot read save folder: {}", dir.display()))?
        {
            let entry =
                entry.with_context(|| format!("cannot read save folder: {}", dir.display()))?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|name| anyhow::anyhow!("save file has no UTF-8 name: {:?}", name))?;
            if !flat_name(&name) {
                anyhow::bail!(
                    "save file {:?} in {} has a name a bundle cannot carry",
                    name,
                    dir.display()
                );
            }
            let relative = format!("{prefix}{name}");
            let file_type = entry
                .file_type()
                .with_context(|| format!("cannot read {}", entry.path().display()))?;
            if save_kind.leaves_out(&name, file_type.is_dir()) {
                continue;
            }
            if file_type.is_dir() {
                walk(save_kind, &entry.path(), &format!("{relative}/"), out)?;
            } else if file_type.is_file() {
                let size = entry
                    .metadata()
                    .with_context(|| format!("cannot read {}", entry.path().display()))?
                    .len();
                out.push((relative, entry.path(), size));
            } else {
                eprintln!(
                    "Leaving {} out of the folder bundle: it is not a regular file or folder",
                    entry.path().display()
                );
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(kind, dir, "", &mut out)?;
    out.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(out)
}

/// Pack every file in a save folder and the folders in it into one payload, and its
/// sha256: the deterministic stored zip a PSP SAVEDATA folder uses, with each file named by
/// its relative path, so equal folders always hash equal.
pub fn pack_folder_save(dir: &Path) -> Result<(Vec<u8>, String)> {
    pack_folder_save_of(FolderSaveKind::WiiNand, dir)
}

/// [`pack_folder_save`], leaving out what the kind does not carry (a port's logs).
pub fn pack_folder_save_of(kind: FolderSaveKind, dir: &Path) -> Result<(Vec<u8>, String)> {
    let mut entries = Vec::new();
    for (relative, path, _) in folder_save_files_of(kind, dir)? {
        let bytes =
            fs::read(&path).with_context(|| format!("cannot read file: {}", path.display()))?;
        entries.push((relative, bytes));
    }
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

/// A bundle entry path that stays inside the save folder: `/`-separated plain names.
fn safe_entry_path(name: &str) -> bool {
    !name.is_empty() && name.split('/').all(flat_name)
}

/// Replace a save folder with the files a bundle holds. Every entry path is checked before
/// anything is written; the files go to `<folder>.rsm-tmp` beside it, the old folder is
/// renamed aside to `<folder>.rsm-old`, the new one renamed into place, and only then is the
/// old one removed, so the folder is never missing. The emulator's folder (the NAND or Vita3K
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
        if !safe_entry_path(name) {
            anyhow::bail!(
                "folder bundle entry {:?} is not a path inside the folder; nothing was written",
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
        let path = staging.join(name);
        if let Some(folder) = path.parent() {
            fs::create_dir_all(folder)
                .with_context(|| format!("cannot create {}", folder.display()))?;
        }
        fs::write(&path, bytes).with_context(|| format!("cannot write {}", path.display()))
    });
    if let Err(err) = written {
        let _ = fs::remove_dir_all(&staging);
        return Err(err);
    }
    let aside = parent.join(format!("{leaf}.rsm-old"));
    // A crash between the two renames below leaves the old folder at the aside path and no
    // folder at the target: that aside is then the only copy of the save. It goes back, and this
    // restore stops, so the next pass judges the recovered folder like any local one.
    if fs::symlink_metadata(target_dir).is_err() && fs::symlink_metadata(&aside).is_ok() {
        let _ = fs::remove_dir_all(&staging);
        fs::rename(&aside, target_dir).with_context(|| {
            format!(
                "cannot rename {} back to {}",
                aside.display(),
                target_dir.display()
            )
        })?;
        anyhow::bail!(
            "recovered {} from an interrupted restore; not replacing it this pass",
            target_dir.display()
        );
    }
    remove_path_if_present(&aside)?;
    let had_old = match fs::symlink_metadata(target_dir) {
        Ok(_) => {
            fs::rename(target_dir, &aside).with_context(|| {
                format!(
                    "cannot rename {} to {}",
                    target_dir.display(),
                    aside.display()
                )
            })?;
            true
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
        Err(err) => {
            return Err(err).with_context(|| format!("cannot read {}", target_dir.display()));
        }
    };
    if let Err(err) = fs::rename(&staging, target_dir) {
        if had_old {
            let _ = fs::rename(&aside, target_dir);
        }
        return Err(err).with_context(|| {
            format!(
                "cannot rename {} to {}",
                staging.display(),
                target_dir.display()
            )
        });
    }
    remove_path_if_present(&aside)?;
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

    fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
        folder_save_files(dir)
            .unwrap()
            .into_iter()
            .map(|(relative, path, _)| (relative, fs::read(path).unwrap()))
            .collect()
    }

    #[test]
    fn an_interrupted_restore_puts_the_aside_folder_back_and_writes_nothing() {
        let device = tempfile::tempdir().unwrap();
        let vita = FolderSaveKind::Vita3k.root_under(device.path());
        let target = FolderSaveKind::Vita3k
            .folder_for(&vita, "PCSE00746")
            .unwrap();
        // A crash left only the aside: the device's own save, and no folder at the target.
        let aside = vita.join("PCSE00746.rsm-old");
        write(&aside.join("SAVEDATA.BIN"), b"the only copy");
        let other = tempfile::tempdir().unwrap();
        let cloud = other.path().join("PCSE00746");
        write(&cloud.join("SAVEDATA.BIN"), b"cloud");
        let (bundle, _) = pack_folder_save(&cloud).unwrap();

        assert!(restore_folder_save(&vita, &target, &bundle).is_err());
        assert_eq!(
            fs::read(target.join("SAVEDATA.BIN")).unwrap(),
            b"the only copy"
        );
        assert!(!aside.exists());
    }

    #[test]
    fn a_vita_save_with_sce_sys_round_trips_every_file() {
        let device_a = tempfile::tempdir().unwrap();
        let save = device_a.path().join("psvita/vita3k/PCSE00746");
        write(&save.join("SAVEDATA.BIN"), b"progress");
        write(&save.join("sce_sys/param.sfo"), b"\0PSF sfo");
        write(&save.join("sce_sys/sdslot.dat"), &[3u8; 4096]);
        write(&save.join("sce_sys/keystone"), b"keystone");
        let (bundle, sha) = pack_folder_save(&save).unwrap();

        let device_b = tempfile::tempdir().unwrap();
        let vita_b = FolderSaveKind::Vita3k.root_under(device_b.path());
        let target = FolderSaveKind::Vita3k
            .folder_for(&vita_b, "PCSE00746")
            .unwrap();
        // An older save with its own sce_sys is replaced, not merged.
        write(&target.join("sce_sys/stale.dat"), b"old");
        write(&target.join("SAVEDATA.BIN"), b"old");
        assert_eq!(restore_folder_save(&vita_b, &target, &bundle).unwrap(), 4);
        assert_eq!(snapshot(&target), snapshot(&save));
        assert_eq!(pack_folder_save(&target).unwrap().1, sha);
        assert!(!vita_b.join("PCSE00746.rsm-tmp").exists());
        assert!(!vita_b.join("PCSE00746.rsm-old").exists());
    }

    #[test]
    fn a_wii_save_with_a_subfolder_round_trips() {
        let device_a = tempfile::tempdir().unwrap();
        let data = device_a.path().join("Wii/title/00010000/52534245/data");
        write(&data.join("banner.bin"), b"WIBN");
        write(&data.join("replays/rep01.bin"), &[9u8; 2048]);
        let (bundle, _) = pack_folder_save(&data).unwrap();

        let device_b = tempfile::tempdir().unwrap();
        let nand_b = device_b.path().join("wii/dolphin");
        fs::create_dir_all(&nand_b).unwrap();
        let target = FolderSaveKind::WiiNand
            .folder_for(&nand_b, "00010000/52534245")
            .unwrap();
        assert_eq!(restore_folder_save(&nand_b, &target, &bundle).unwrap(), 2);
        assert_eq!(snapshot(&target), snapshot(&data));
    }

    #[test]
    fn an_entry_leaving_the_folder_is_refused_and_the_save_is_untouched() {
        let device = tempfile::tempdir().unwrap();
        let vita = device.path().join("psvita/vita3k");
        let target = FolderSaveKind::Vita3k
            .folder_for(&vita, "PCSE00746")
            .unwrap();
        write(&target.join("SAVEDATA.BIN"), b"keep");
        write(&target.join("sce_sys/param.sfo"), b"keep sfo");
        let before = snapshot(&target);
        for bad in [
            "../escape.bin",
            "sce_sys/../../escape.bin",
            "/escape.bin",
            "sce_sys//x",
        ] {
            let bundle = build_stored_zip(&[
                ("SAVEDATA.BIN".to_string(), b"new".to_vec()),
                (bad.to_string(), b"x".to_vec()),
            ])
            .unwrap();
            assert!(
                restore_folder_save(&vita, &target, &bundle).is_err(),
                "{bad}"
            );
            assert_eq!(snapshot(&target), before, "{bad}");
            assert!(!vita.join("escape.bin").exists());
            assert!(!device.path().join("psvita/escape.bin").exists());
            assert!(!vita.join("PCSE00746.rsm-tmp").exists());
        }
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

    fn write_port(ports: &Path, dir: &str, zip_name: &str) -> PathBuf {
        let port = ports.join(dir);
        write(
            &port.join("port.json"),
            format!("{{\"version\": 4, \"name\": \"{zip_name}\"}}").as_bytes(),
        );
        port
    }

    #[test]
    fn a_portmaster_port_conf_folder_is_one_save_keyed_by_its_port_name_without_logs() {
        let root = tempfile::tempdir().unwrap();
        let ports = root.path().join("PortMaster/ports");
        // LÖVE: conf/love/<Game>/; Godot: conf/godot/app_userdata/<Game>/ with logs.
        let arrow = write_port(&ports, "curseofthearrow", "curseofthearrow.zip");
        write(&arrow.join("conf/love/CurseOfTheArrow/.gitkeep"), b"");
        write(
            &arrow.join("conf/love/CurseOfTheArrow/save.lua"),
            b"return {7}",
        );
        let tower = write_port(&ports, "atowerintheforest", "atowerintheforest.zip");
        let game = tower.join("conf/godot/app_userdata/A Tower in the Forest");
        write(&game.join("save_data.dat"), b"tower progress");
        write(&game.join("logs/godot.log"), b"launch log");
        write(&tower.join("log.txt"), b"port log");
        // An installed port that never wrote a save: only the shipped .gitkeep and a log.
        let fresh = write_port(&ports, "superhexagon", "superhexagon.zip");
        write(&fresh.join("conf/SuperHexagon/.gitkeep"), b"");
        write(&fresh.join("conf/run.log"), b"log");
        // A folder with no port.json is not a port.
        write(&ports.join("autoinstall/conf/x.dat"), b"x");

        let found = discover_port_conf_saves(&[root.path().to_path_buf()]);
        let names: Vec<&str> = found.iter().map(|save| save.save_dir.as_str()).collect();
        assert_eq!(names, vec!["atowerintheforest", "curseofthearrow"]);
        assert!(found.iter().all(|save| save.kind.system_slug() == "ports"));
        assert_eq!(found[1].path, arrow.join("conf"));
        assert_eq!(found[1].line_key(), "ports-folder:curseofthearrow");
        assert_eq!(emulator_root_of(&found[1]).unwrap(), ports);

        let files: Vec<String> = folder_save_files_of(FolderSaveKind::PortConf, &found[0].path)
            .unwrap()
            .into_iter()
            .map(|(relative, _, _)| relative)
            .collect();
        assert_eq!(
            files,
            vec!["godot/app_userdata/A Tower in the Forest/save_data.dat".to_string()]
        );
    }

    #[test]
    fn a_ports_save_root_may_be_the_ports_folder_portmaster_or_the_retrodeck_home() {
        for layout in ["", "ports", "PortMaster/ports"] {
            let root = tempfile::tempdir().unwrap();
            let ports = root.path().join(layout);
            let arrow = write_port(&ports, "curseofthearrow", "curseofthearrow.zip");
            write(
                &arrow.join("conf/love/CurseOfTheArrow/save.lua"),
                b"return {7}",
            );
            let found = discover_port_conf_saves(&[root.path().to_path_buf()]);
            assert_eq!(found.len(), 1, "{layout}");
            assert_eq!(found[0].save_dir, "curseofthearrow", "{layout}");
        }
    }

    #[test]
    fn a_port_conf_bundle_restores_only_into_an_installed_port_and_round_trips() {
        let device_a = tempfile::tempdir().unwrap();
        let arrow_a = write_port(device_a.path(), "curseofthearrow", "curseofthearrow.zip");
        write(
            &arrow_a.join("conf/love/CurseOfTheArrow/save.lua"),
            b"return {7}",
        );
        write(&arrow_a.join("conf/love/CurseOfTheArrow/run.log"), b"log");
        let (bundle, sha) =
            pack_folder_save_of(FolderSaveKind::PortConf, &arrow_a.join("conf")).unwrap();

        let device_b = tempfile::tempdir().unwrap();
        let ports_b = device_b.path().join("PortMaster/ports");
        let kind = FolderSaveKind::PortConf;
        let root = kind.restore_root_under(device_b.path(), "curseofthearrow");
        let target = kind.folder_for(&root, "curseofthearrow").unwrap();
        assert!(
            !kind.restore_target_ready(&root, &target),
            "port not installed"
        );

        let arrow_b = write_port(&ports_b, "curseofthearrow", "curseofthearrow.zip");
        write(&arrow_b.join("conf/love/CurseOfTheArrow/.gitkeep"), b"");
        let root = kind.restore_root_under(device_b.path(), "curseofthearrow");
        assert_eq!(root, ports_b);
        let target = kind.folder_for(&root, "curseofthearrow").unwrap();
        assert_eq!(target, arrow_b.join("conf"));
        assert!(
            !kind.holds_save(&target),
            "a shipped .gitkeep is not a save"
        );
        assert!(kind.restore_target_ready(&root, &target));
        assert_eq!(restore_folder_save(&root, &target, &bundle).unwrap(), 1);
        assert_eq!(
            fs::read(target.join("love/CurseOfTheArrow/save.lua")).unwrap(),
            b"return {7}"
        );
        assert!(kind.holds_save(&target));
        assert_eq!(pack_folder_save_of(kind, &target).unwrap().1, sha);
    }

    #[test]
    fn only_plain_lowercase_port_names_name_a_port_restore_target() {
        let root = Path::new("/ports");
        assert_eq!(
            FolderSaveKind::PortConf.folder_for(root, "curseofthearrow"),
            Some(PathBuf::from("/ports/curseofthearrow/conf"))
        );
        for bad in [
            "../etc",
            "curse/arrow",
            "CurseOfTheArrow",
            ".hidden",
            "",
            "a..b",
        ] {
            assert!(
                FolderSaveKind::PortConf.folder_for(root, bad).is_none(),
                "{bad}"
            );
        }
    }
}
