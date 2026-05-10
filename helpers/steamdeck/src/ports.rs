use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortSaveMatch {
    pub path: PathBuf,
    pub port_id: String,
    pub port_name: String,
    pub origin_system_slug: String,
    pub runtime_profile: String,
    pub relative_path: String,
    pub root_relative_path: String,
    pub slot_id: String,
    pub display_title: String,
}

#[derive(Debug, Clone, Copy)]
struct PortManifest {
    id: &'static str,
    name: &'static str,
    origin_system_slug: &'static str,
    runtime_profile: &'static str,
    root_aliases: &'static [&'static str],
    rules: &'static [PortSaveRule],
}

#[derive(Debug, Clone, Copy)]
struct PortSaveRule {
    pattern: &'static str,
    allow_empty: bool,
}

const PORT_MANIFESTS: &[PortManifest] = &[
    PortManifest {
        id: "ship-of-harkinian",
        name: "The Legend of Zelda: Ocarina of Time (Ship of Harkinian)",
        origin_system_slug: "n64",
        runtime_profile: "port/ship-of-harkinian",
        root_aliases: &["OcarinaOfTime", "ShipOfHarkinian", "Shipwright"],
        rules: &[
            PortSaveRule::progress("Save/global.sav"),
            PortSaveRule::progress("Save/file*.sav"),
            PortSaveRule::progress("portable_home/Save/global.sav"),
            PortSaveRule::progress("portable_home/Save/file*.sav"),
        ],
    },
    PortManifest {
        id: "starship",
        name: "Star Fox 64 (Starship)",
        origin_system_slug: "n64",
        runtime_profile: "port/starship",
        root_aliases: &["StarFox64", "Starship"],
        rules: &[
            PortSaveRule::progress("default.sav"),
            PortSaveRule::progress("portable_home/default.sav"),
        ],
    },
    PortManifest {
        id: "spaghettikart",
        name: "Mario Kart 64 (SpaghettiKart)",
        origin_system_slug: "n64",
        runtime_profile: "port/spaghettikart",
        root_aliases: &["MarioKart64", "SpaghettiKart"],
        rules: &[
            PortSaveRule::progress("default.sav"),
            PortSaveRule::progress("portable_home/default.sav"),
        ],
    },
    PortManifest {
        id: "super-metroid-native",
        name: "Super Metroid (Native Port)",
        origin_system_slug: "snes",
        runtime_profile: "port/super-metroid-native",
        root_aliases: &["SuperMetroid"],
        rules: &[
            PortSaveRule::progress("saves/*.srm"),
            PortSaveRule::progress("portable_home/saves/*.srm"),
        ],
    },
    PortManifest {
        id: "sonic1-forever",
        name: "Sonic 1 Forever",
        origin_system_slug: "genesis",
        runtime_profile: "port/sonic1-forever",
        root_aliases: &["Sonic1Forever"],
        rules: &[
            PortSaveRule::progress("Scripts/Save/SaveSel.txt"),
            PortSaveRule::progress("Scripts/Save/SaveSlot.txt"),
            PortSaveRule::progress("portable_home/Scripts/Save/SaveSel.txt"),
            PortSaveRule::progress("portable_home/Scripts/Save/SaveSlot.txt"),
        ],
    },
    PortManifest {
        id: "sonic3-air",
        name: "Sonic 3 A.I.R.",
        origin_system_slug: "genesis",
        runtime_profile: "port/sonic3-air",
        root_aliases: &["Sonic3AIR", "Sonic3A.I.R"],
        rules: &[
            PortSaveRule::progress("saves/*.sav"),
            PortSaveRule::progress("saves/*.srm"),
            PortSaveRule::progress("saves/*.bin"),
            PortSaveRule::progress("portable_home/saves/*.sav"),
            PortSaveRule::progress("portable_home/saves/*.srm"),
            PortSaveRule::progress("portable_home/saves/*.bin"),
        ],
    },
    PortManifest {
        id: "opengoal-jak1",
        name: "Jak and Daxter: The Precursor Legacy (OpenGOAL)",
        origin_system_slug: "ps2",
        runtime_profile: "port/opengoal-jak1",
        root_aliases: &["OpenGOAL-jak1"],
        rules: &[],
    },
    PortManifest {
        id: "opengoal-jak2",
        name: "Jak II (OpenGOAL)",
        origin_system_slug: "ps2",
        runtime_profile: "port/opengoal-jak2",
        root_aliases: &["OpenGOAL-jak2"],
        rules: &[],
    },
];

impl PortSaveRule {
    const fn progress(pattern: &'static str) -> Self {
        Self {
            pattern,
            allow_empty: false,
        }
    }
}

pub fn discover_port_save_matches(roots: &[PathBuf]) -> Result<Vec<PortSaveMatch>> {
    let mut matches = Vec::new();

    for root in roots {
        if !root.is_dir() {
            continue;
        }
        for manifest in PORT_MANIFESTS {
            for alias in manifest.root_aliases {
                let game_root = root.join(alias);
                if !game_root.is_dir() {
                    continue;
                }
                for rule in manifest.rules {
                    collect_rule_matches(root, &game_root, alias, manifest, rule, &mut matches)?;
                }
            }
        }
    }

    matches.sort_by(|left, right| left.path.cmp(&right.path));
    matches.dedup_by(|left, right| left.path == right.path);
    Ok(matches)
}

fn collect_rule_matches(
    scan_root: &Path,
    game_root: &Path,
    root_alias: &str,
    manifest: &PortManifest,
    rule: &PortSaveRule,
    matches: &mut Vec<PortSaveMatch>,
) -> Result<()> {
    if !rule.pattern.contains('*') {
        let path = game_root.join(rule.pattern);
        if path.is_file() {
            maybe_push_match(scan_root, &path, root_alias, manifest, rule, matches)?;
        }
        return Ok(());
    }

    let pattern_path = Path::new(rule.pattern);
    let file_pattern = pattern_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    if file_pattern.is_empty() {
        return Ok(());
    }
    let parent = pattern_path
        .parent()
        .filter(|value| value != &Path::new(""))
        .unwrap_or_else(|| Path::new("."));
    let search_dir = game_root.join(parent);
    if !search_dir.is_dir() {
        return Ok(());
    }

    for entry in fs::read_dir(&search_dir)
        .with_context(|| format!("kan port save map niet lezen: {}", search_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if wildcard_match(file_pattern, file_name) {
            maybe_push_match(scan_root, &path, root_alias, manifest, rule, matches)?;
        }
    }

    Ok(())
}

fn maybe_push_match(
    scan_root: &Path,
    path: &Path,
    root_alias: &str,
    manifest: &PortManifest,
    rule: &PortSaveRule,
    matches: &mut Vec<PortSaveMatch>,
) -> Result<()> {
    if !rule.allow_empty && path.metadata()?.len() == 0 {
        return Ok(());
    }

    let root_relative_path = slash_path(
        path.strip_prefix(scan_root)
            .with_context(|| format!("port save valt buiten scan root: {}", path.display()))?,
    );
    if !safe_relative_path(&root_relative_path) {
        return Ok(());
    }

    let root_prefix = root_alias.trim_matches('/');
    let relative_path = root_relative_path
        .strip_prefix(root_prefix)
        .and_then(|value| value.strip_prefix('/'))
        .unwrap_or(root_relative_path.as_str())
        .to_string();
    if !safe_relative_path(&relative_path) {
        return Ok(());
    }

    let slot_id = slot_id_from_relative_path(&relative_path);
    let slot_label = slot_label_from_relative_path(&relative_path);
    matches.push(PortSaveMatch {
        path: path.to_path_buf(),
        port_id: manifest.id.to_string(),
        port_name: manifest.name.to_string(),
        origin_system_slug: manifest.origin_system_slug.to_string(),
        runtime_profile: manifest.runtime_profile.to_string(),
        relative_path,
        root_relative_path,
        slot_id,
        display_title: format!("{} - {}", manifest.name, slot_label),
    });
    Ok(())
}

fn wildcard_match(pattern: &str, value: &str) -> bool {
    let pattern = pattern.to_ascii_lowercase();
    let value = value.to_ascii_lowercase();
    if !pattern.contains('*') {
        return pattern == value;
    }

    let mut remainder = value.as_str();
    let mut first = true;
    for part in pattern.split('*') {
        if part.is_empty() {
            continue;
        }
        if first && !pattern.starts_with('*') {
            if !remainder.starts_with(part) {
                return false;
            }
            remainder = &remainder[part.len()..];
        } else if let Some(index) = remainder.find(part) {
            remainder = &remainder[index + part.len()..];
        } else {
            return false;
        }
        first = false;
    }
    pattern.ends_with('*') || remainder.is_empty()
}

fn slash_path(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => value.to_str().map(ToString::to_string),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn safe_relative_path(value: &str) -> bool {
    let path = Path::new(value);
    !path.is_absolute()
        && !value.trim().is_empty()
        && path.components().all(|component| {
            matches!(component, Component::Normal(value) if value.to_str().map(|part| !part.trim().is_empty()).unwrap_or(false))
        })
}

fn slot_id_from_relative_path(relative_path: &str) -> String {
    let file_stem = Path::new(relative_path)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(relative_path);
    canonical_token(file_stem)
}

fn slot_label_from_relative_path(relative_path: &str) -> String {
    let file_stem = Path::new(relative_path)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(relative_path);
    match file_stem.to_ascii_lowercase().as_str() {
        "global" => "Global".to_string(),
        "savesel" => "Save Select".to_string(),
        "saveslot" => "Save Slot".to_string(),
        "default" => "Default".to_string(),
        value if value.starts_with("file") && value.len() > 4 => {
            format!("File {}", value.trim_start_matches("file"))
        }
        _ => titleize_token(file_stem),
    }
}

fn canonical_token(value: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

fn titleize_token(value: &str) -> String {
    let words = value
        .replace(['_', '-'], " ")
        .split_whitespace()
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
                None => String::new(),
            }
        })
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>();
    if words.is_empty() {
        "Default".to_string()
    } else {
        words.join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_known_progress_saves_and_excludes_assets() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("ports");
        fs::create_dir_all(root.join("OcarinaOfTime/Save")).unwrap();
        fs::create_dir_all(root.join("MarioKart64")).unwrap();
        fs::create_dir_all(root.join("Sonic1Forever/Scripts/Save")).unwrap();
        fs::create_dir_all(root.join("Sonic1Forever/Data")).unwrap();
        fs::create_dir_all(root.join("Sonic3AIR/saves/states")).unwrap();

        fs::write(root.join("OcarinaOfTime/Save/file1.sav"), [1u8, 2, 3]).unwrap();
        fs::write(root.join("OcarinaOfTime/Save/global.sav"), [4u8]).unwrap();
        fs::write(root.join("MarioKart64/default.sav"), [5u8, 6]).unwrap();
        fs::write(root.join("MarioKart64/controllerPak_header.sav"), []).unwrap();
        fs::write(
            root.join("Sonic1Forever/Scripts/Save/SaveSlot.txt"),
            b"slot=1",
        )
        .unwrap();
        fs::write(root.join("Sonic1Forever/Data/SData.bin"), [9u8]).unwrap();
        fs::write(
            root.join("Sonic3AIR/saves/states/level_select.state"),
            [7u8],
        )
        .unwrap();

        let found = discover_port_save_matches(&[root]).unwrap();
        let rels = found
            .iter()
            .map(|item| item.root_relative_path.as_str())
            .collect::<Vec<_>>();
        let oot_file1 = found
            .iter()
            .find(|item| item.root_relative_path == "OcarinaOfTime/Save/file1.sav")
            .unwrap();
        let sonic_slot = found
            .iter()
            .find(|item| item.root_relative_path == "Sonic1Forever/Scripts/Save/SaveSlot.txt")
            .unwrap();

        assert!(rels.contains(&"OcarinaOfTime/Save/file1.sav"));
        assert!(rels.contains(&"OcarinaOfTime/Save/global.sav"));
        assert!(rels.contains(&"MarioKart64/default.sav"));
        assert!(rels.contains(&"Sonic1Forever/Scripts/Save/SaveSlot.txt"));
        assert_eq!(oot_file1.slot_id, "file1");
        assert_eq!(sonic_slot.slot_id, "saveslot");
        assert!(!rels.contains(&"MarioKart64/controllerPak_header.sav"));
        assert!(!rels.contains(&"Sonic1Forever/Data/SData.bin"));
        assert!(!rels.contains(&"Sonic3AIR/saves/states/level_select.state"));
    }
}
