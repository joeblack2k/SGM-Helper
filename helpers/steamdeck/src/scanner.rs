use std::collections::{BTreeSet, HashMap};
use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use aes::Aes128;
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit, generic_array::GenericArray};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha1::{Digest as Sha1Digest, Sha1};
use sha2::Sha256;
use walkdir::WalkDir;

const SAVE_EXTENSIONS: &[&str] = &[
    "sav", "srm", "eep", "fla", "sa1", "rtc", "ram", "sra", "mpk", "cpk", "dsv", "gme", "mcr",
    "mc", "mcd", "vmp", "psv", "ps2", "bin", "vms", "dci", "bkr",
];

const MAX_SAVE_BYTES: u64 = 512 * 1024 * 1024;

const PS1_MEMCARD_SIZE: usize = 131_072;
const PS1_FRAME_SIZE: usize = 128;
const PS1_HEADER_BLOCK_SIZE: usize = 8192;
const PS1_DEXDRIVE_HEADER_LENGTH: usize = 3904;
const PS1_DEXDRIVE_MAGIC: &[u8] = b"123-456-STD";
const PS1_PSP_VMP_HEADER_LENGTH: usize = 0x80;
const PS1_PSP_VMP_MAGIC: [u8; 12] = [0, 0x50, 0x4D, 0x56, 0x80, 0, 0, 0, 0, 0, 0, 0];
const PS1_PSP_VMP_SALT_SEED_OFFSET: usize = 0x0C;
const PS1_PSP_VMP_SALT_SEED_LEN: usize = 0x14;
const PS1_PSP_VMP_SIGNATURE_OFFSET: usize = 0x20;
const PS1_PSP_VMP_SIGNATURE_LEN: usize = 0x14;
const PS1_PSP_VMP_SALT_LEN: usize = 0x40;
const PS1_PSP_VMP_KEY: [u8; 16] = [
    0xAB, 0x5A, 0xBC, 0x9F, 0xC1, 0xF4, 0x9D, 0xE6, 0xA0, 0x51, 0xDB, 0xAE, 0xFA, 0x51, 0x88, 0x59,
];
const PS1_PSP_VMP_IV_PRETEND: [u8; 16] = [
    0xB3, 0x0F, 0xFE, 0xED, 0xB7, 0xDC, 0x5E, 0xB7, 0x13, 0x3D, 0xA6, 0x0D, 0x1B, 0x6B, 0x2C, 0xDC,
];
const PS1_PSP_VMP_SALT_SEED_INIT: [u8; 20] = [
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F,
    0x10, 0x11, 0x12, 0x13,
];

const PS2_MEMCARD_MAGIC: &[u8] = b"Sony PS2 Memory Card Format ";
const DC_BLOCK_SIZE: usize = 512;
const DC_VMU_BLOCK_COUNT: usize = 256;
const DC_VMU_SIZE: usize = DC_BLOCK_SIZE * DC_VMU_BLOCK_COUNT;
const DC_ROOT_BLOCK: usize = 255;
const DC_DIR_ENTRY_SIZE: usize = 32;
const DC_DCI_HEADER_SIZE: usize = 32;
const DC_FILETYPE_DATA: u8 = 0x33;
const DC_FILETYPE_GAME: u8 = 0xCC;
const DC_FAT_FREE: u16 = 0xFFFC;
const DC_FAT_END: u16 = 0xFFFA;
const DC_NVMEM_MAGIC: &[u8] = b"KATANA_FLASH____";
const SATURN_HEADER_MAGIC: &[u8] = b"BackUpRam Format";
const SATURN_MIN_MAGIC_BYTES: usize = 0x40;
const SATURN_INTERNAL_RAW_SIZE: usize = 0x8000;
const SATURN_INTERNAL_INTERLEAVED_SIZE: usize = SATURN_INTERNAL_RAW_SIZE * 2;
const SATURN_CARTRIDGE_RAW_SIZE: usize = 0x80000;
const SATURN_CARTRIDGE_INTERLEAVED_SIZE: usize = SATURN_CARTRIDGE_RAW_SIZE * 2;
const SATURN_YABASANSHIRO_RAW_SIZE: usize = 0x400000;
const SATURN_YABASANSHIRO_INTERLEAVED_SIZE: usize = SATURN_YABASANSHIRO_RAW_SIZE * 2;
const SATURN_COMBINED_RAW_SIZE: usize = SATURN_INTERNAL_RAW_SIZE + SATURN_CARTRIDGE_RAW_SIZE;
const SATURN_COMBINED_INTERLEAVED_SIZE: usize =
    SATURN_INTERNAL_INTERLEAVED_SIZE + SATURN_CARTRIDGE_INTERLEAVED_SIZE;
const SATURN_INTERNAL_BLOCK_SIZE: usize = 0x40;
const SATURN_CARTRIDGE_BLOCK_SIZE: usize = 0x200;
const SATURN_ARCHIVE_ENTRY_MARKER: u32 = 0x8000_0000;
const N64_RETROARCH_SRM_SIZE: u64 = 0x48800;
const WII_DATA_BIN_BACKUP_HEADER_OFFSET: usize = 0xF0C0;
const WII_DATA_BIN_FILE_HEADER_OFFSET: usize = 0xF140;
const WII_DATA_BIN_FILE_HEADER_MAGIC: u32 = 0x03AD_F17E;

const ROM_EXTENSIONS: &[&str] = &[
    "nes", "fds", "sfc", "smc", "gb", "gbc", "gba", "n64", "z64", "v64", "nds", "md", "gen", "32x",
    "sms", "gg", "cue", "iso", "chd", "gdi", "cdi", "rvz", "wbfs", "wad", "pce", "a26", "a78",
    "col", "bin", "zip", "7z", "pbp", "cso", "vpk",
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

pub fn wii_title_code_from_path(path: &Path) -> Option<String> {
    let normalized = path.to_string_lossy().replace('\\', "/");
    let parts: Vec<&str> = normalized.split('/').collect();
    for (idx, part) in parts.iter().enumerate() {
        let Some(code) = normalize_wii_title_code(part) else {
            continue;
        };
        if parts
            .get(idx + 1)
            .map(|value| value.eq_ignore_ascii_case("data.bin"))
            .unwrap_or(false)
        {
            return Some(code);
        }
    }
    None
}

pub fn infer_system_slug(path: &Path) -> Option<String> {
    classify_supported_save(path, None).map(|value| value.system_slug)
}

pub fn infer_supported_console_slug(save_path: &Path, rom_path: Option<&Path>) -> Option<String> {
    classify_supported_save(save_path, rom_path).map(|value| value.system_slug)
}

pub fn saturn_skip_reason(save_path: &Path, rom_path: Option<&Path>) -> Option<String> {
    let ext = path_extension(save_path)?;
    if !matches!(ext.as_str(), "sav" | "srm" | "ram" | "bkr") {
        return None;
    }

    let save_size = save_path.metadata().ok()?.len();
    let save_lower = save_path.to_string_lossy().to_ascii_lowercase();
    let rom_lower = rom_path
        .map(|path| path.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let combined = format!("{} {}", save_lower, rom_lower);
    let saturn_hint = contains_any(
        &combined,
        &[
            "saturn",
            "/saturn/",
            "\\saturn\\",
            "yabause",
            "yabasanshiro",
            "kronos",
            "ssf",
            "beetle saturn",
            "mednafen saturn",
        ],
    );
    let saturn_rom_hint = rom_path
        .and_then(path_extension)
        .map(|rom_ext| matches!(rom_ext.as_str(), "cue" | "iso" | "chd"))
        .unwrap_or(false);
    if !saturn_hint && !saturn_rom_hint {
        return None;
    }

    if looks_plain_text(save_path) || looks_like_executable_or_archive(save_path) {
        return Some("skip_invalid_saturn_backup_ram".to_string());
    }

    let expected_sizes = [
        SATURN_INTERNAL_RAW_SIZE,
        SATURN_INTERNAL_INTERLEAVED_SIZE,
        SATURN_CARTRIDGE_RAW_SIZE,
        SATURN_CARTRIDGE_INTERLEAVED_SIZE,
        SATURN_COMBINED_RAW_SIZE,
        SATURN_COMBINED_INTERLEAVED_SIZE,
        SATURN_YABASANSHIRO_RAW_SIZE,
        SATURN_YABASANSHIRO_INTERLEAVED_SIZE,
    ];
    if !expected_sizes.contains(&(save_size as usize)) {
        return Some(format!(
            "skip_saturn_without_structural_evidence(size={})",
            save_size
        ));
    }

    match inspect_saturn_metadata(save_path) {
        Some(metadata) if metadata.save_entries > 0 => None,
        Some(_) => Some("skip_empty_saturn_backup_ram".to_string()),
        None => Some("skip_invalid_saturn_backup_ram".to_string()),
    }
}

pub fn dreamcast_skip_reason(save_path: &Path, rom_path: Option<&Path>) -> Option<String> {
    let ext = path_extension(save_path)?;
    if !matches!(ext.as_str(), "bin" | "vms" | "dci") {
        return None;
    }

    let save_size = save_path.metadata().ok()?.len();
    let save_lower = save_path.to_string_lossy().to_ascii_lowercase();
    let rom_lower = rom_path
        .map(|path| path.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let combined = format!("{} {}", save_lower, rom_lower);
    let dreamcast_hint = contains_any(
        &combined,
        &[
            "dreamcast",
            "/dreamcast/",
            "\\dreamcast\\",
            "flycast",
            "reicast",
            "vmu",
            ".a1.bin",
            ".a2.bin",
            ".a3.bin",
            ".a4.bin",
            ".b1.bin",
            ".b2.bin",
            ".b3.bin",
            ".b4.bin",
            ".c1.bin",
            ".c2.bin",
            ".c3.bin",
            ".c4.bin",
            ".d1.bin",
            ".d2.bin",
            ".d3.bin",
            ".d4.bin",
        ],
    );
    let dreamcast_rom_hint = rom_path
        .and_then(path_extension)
        .map(|rom_ext| matches!(rom_ext.as_str(), "gdi" | "cdi" | "chd" | "cue" | "iso"))
        .unwrap_or(false);
    if ext == "bin" && !dreamcast_hint && !dreamcast_rom_hint {
        return None;
    }

    if looks_plain_text(save_path) || looks_like_executable_or_archive(save_path) {
        return Some("skip_invalid_dreamcast_container".to_string());
    }

    if !is_plausible_save_for_system(&ext, save_size, "dreamcast") {
        return Some(format!(
            "skip_dreamcast_without_structural_evidence(size={})",
            save_size
        ));
    }

    match inspect_dreamcast_metadata(save_path, &ext) {
        Some(metadata) if metadata.save_entries > 0 => None,
        Some(_) => Some("skip_empty_dreamcast_vmu".to_string()),
        None => Some("skip_invalid_dreamcast_container".to_string()),
    }
}

pub fn wii_skip_reason(save_path: &Path) -> Option<String> {
    let ext = path_extension(save_path)?;
    if ext != "bin" {
        return None;
    }
    let lower = save_path.to_string_lossy().to_ascii_lowercase();
    if !looks_like_wii_data_bin_path(save_path)
        && !contains_any(
            &lower,
            &["/wii/", "\\wii\\", "nintendo wii", "dolphin", "rvl"],
        )
    {
        return None;
    }
    match inspect_wii_metadata(save_path) {
        Some(_) => None,
        None => Some("skip_invalid_wii_data_bin".to_string()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveClassification {
    pub system_slug: String,
    pub evidence: String,
}

#[derive(Debug, Clone)]
struct DreamcastPkgHeader {
    short_desc: String,
    app_id: String,
    icon_count: u16,
}

#[derive(Debug, Clone)]
struct DreamcastMetadata {
    container: &'static str,
    save_entries: usize,
    icon_frames: usize,
    sample_title: Option<String>,
    sample_app: Option<String>,
}

#[derive(Debug, Clone)]
struct SaturnMetadata {
    format: &'static str,
    save_entries: usize,
    has_internal: bool,
    has_cartridge: bool,
}

#[derive(Debug, Clone)]
struct WiiMetadata {
    title_code: Option<String>,
    file_count: u32,
    declared_data_size: u32,
    embedded_file_name: Option<String>,
    embedded_file_size: u32,
    certificate_present: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SaveContainerFormat {
    Native,
    Ps1Raw,
    Ps1DexDrive,
    Ps1Vmp,
}

impl SaveContainerFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Ps1Raw => "ps1-raw",
            Self::Ps1DexDrive => "ps1-dexdrive",
            Self::Ps1Vmp => "ps1-vmp",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SaveAdapterProfile {
    Identity,
    Ps1Raw,
    Ps1DexDrive,
    Ps1Vmp,
}

impl SaveAdapterProfile {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Identity => "identity",
            Self::Ps1Raw => "ps1-raw",
            Self::Ps1DexDrive => "ps1-dexdrive",
            Self::Ps1Vmp => "ps1-vmp",
        }
    }
}

#[derive(Debug, Clone)]
pub struct NormalizedSave {
    pub canonical_bytes: Vec<u8>,
    pub local_container: SaveContainerFormat,
    pub adapter_profile: SaveAdapterProfile,
}

pub fn normalize_save_for_sync(
    save_path: &Path,
    system_slug: &str,
) -> Result<Option<NormalizedSave>> {
    let bytes = std::fs::read(save_path)
        .with_context(|| format!("kan save bestand niet lezen: {}", save_path.display()))?;
    normalize_save_bytes_for_sync(save_path, system_slug, &bytes)
}

pub fn normalize_save_bytes_for_sync(
    save_path: &Path,
    system_slug: &str,
    bytes: &[u8],
) -> Result<Option<NormalizedSave>> {
    if system_slug != "psx" {
        return Ok(Some(NormalizedSave {
            canonical_bytes: bytes.to_vec(),
            local_container: SaveContainerFormat::Native,
            adapter_profile: SaveAdapterProfile::Identity,
        }));
    }

    let ext = path_extension(save_path).unwrap_or_default();
    let normalized = match ext.as_str() {
        "gme" => decode_ps1_dexdrive(bytes).map(|payload| NormalizedSave {
            canonical_bytes: payload,
            local_container: SaveContainerFormat::Ps1DexDrive,
            adapter_profile: SaveAdapterProfile::Ps1DexDrive,
        }),
        "vmp" => decode_ps1_vmp(bytes).map(|payload| NormalizedSave {
            canonical_bytes: payload,
            local_container: SaveContainerFormat::Ps1Vmp,
            adapter_profile: SaveAdapterProfile::Ps1Vmp,
        }),
        _ => {
            if validate_ps1_raw_memcard(bytes) {
                Some(NormalizedSave {
                    canonical_bytes: bytes.to_vec(),
                    local_container: SaveContainerFormat::Ps1Raw,
                    adapter_profile: SaveAdapterProfile::Ps1Raw,
                })
            } else {
                None
            }
        }
    };
    Ok(normalized)
}

pub fn encode_download_for_local_container(
    canonical_bytes: &[u8],
    local_container: SaveContainerFormat,
) -> Result<Vec<u8>> {
    if !validate_ps1_raw_memcard(canonical_bytes) && local_container != SaveContainerFormat::Native
    {
        anyhow::bail!(
            "canonieke PS1 save is ongeldig en kan niet worden teruggezet naar lokaal formaat"
        );
    }

    let encoded = match local_container {
        SaveContainerFormat::Native | SaveContainerFormat::Ps1Raw => canonical_bytes.to_vec(),
        SaveContainerFormat::Ps1DexDrive => encode_ps1_dexdrive(canonical_bytes),
        SaveContainerFormat::Ps1Vmp => encode_ps1_vmp(canonical_bytes)?,
    };
    Ok(encoded)
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

    if save_ext == "bin"
        && is_plausible_save_for_system(&save_ext, save_size, "wii")
        && inspect_wii_metadata(save_path).is_some()
    {
        return classify_if_valid(
            save_path,
            &save_ext,
            save_size,
            "wii",
            format!(
                "wii data.bin backup header + .{} ({} bytes)",
                save_ext, save_size
            ),
        );
    }

    if let Some(slug) = rom_path
        .and_then(path_extension)
        .and_then(system_slug_from_rom_extension)
        && is_plausible_save_for_system(&save_ext, save_size, slug)
    {
        return classify_if_valid(
            save_path,
            &save_ext,
            save_size,
            slug,
            format!("rom-extension + .{} ({} bytes)", save_ext, save_size),
        );
    }

    let save_lower = save_path.to_string_lossy().to_ascii_lowercase();
    if contains_any(&save_lower, &["gameboy advance", "/gba/", "\\gba\\"]) {
        if is_plausible_save_for_system(&save_ext, save_size, "gba") {
            return classify_if_valid(
                save_path,
                &save_ext,
                save_size,
                "gba",
                format!("path hint gba + .{} ({} bytes)", save_ext, save_size),
            );
        }
        return None;
    }
    if contains_any(
        &save_lower,
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
            "nintendo wii",
            "/wii/",
            "\\wii\\",
            "rvl",
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
        let slug = infer_nintendo_slug(&save_lower);
        if is_plausible_save_for_system(&save_ext, save_size, slug) {
            return classify_if_valid(
                save_path,
                &save_ext,
                save_size,
                slug,
                format!("path hint nintendo + .{} ({} bytes)", save_ext, save_size),
            );
        }
        return None;
    }

    if contains_any(
        &save_lower,
        &[
            "master system",
            "/sms/",
            "\\sms\\",
            "game gear",
            "/gg/",
            "\\gg\\",
            "sega 32x",
            "sega-32x",
            "sega32x",
            "/32x/",
            "\\32x\\",
            "mega cd",
            "mega-cd",
            "megacd",
            "sega cd",
            "sega-cd",
            "segacd",
            "/megacd/",
            "\\megacd\\",
            "/mega-cd/",
            "\\mega-cd\\",
            "/segacd/",
            "\\segacd\\",
            "/sega-cd/",
            "\\sega-cd\\",
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
        let slug = infer_sega_slug(&save_lower);
        if is_plausible_save_for_system(&save_ext, save_size, slug) {
            return classify_if_valid(
                save_path,
                &save_ext,
                save_size,
                slug,
                format!("path hint sega + .{} ({} bytes)", save_ext, save_size),
            );
        }
        return None;
    }

    if contains_any(
        &save_lower,
        &[
            "neo geo", "neogeo", "neo-geo", "/mvs/", "\\mvs\\", "/aes/", "\\aes\\",
        ],
    ) {
        if is_plausible_save_for_system(&save_ext, save_size, "neogeo") {
            return classify_if_valid(
                save_path,
                &save_ext,
                save_size,
                "neogeo",
                format!("path hint neogeo + .{} ({} bytes)", save_ext, save_size),
            );
        }
        return None;
    }

    if contains_any(
        &save_lower,
        &[
            "playstation",
            "sony",
            "/psx/",
            "\\psx\\",
            "/ps1/",
            "\\ps1\\",
            "/ps2/",
            "\\ps2\\",
            "/psp/",
            "\\psp\\",
            "/ps3/",
            "\\ps3\\",
            "/psvita/",
            "\\psvita\\",
            "/vita/",
            "\\vita\\",
            "/ps4/",
            "\\ps4\\",
            "/ps5/",
            "\\ps5\\",
            "duckstation",
            "pcsx",
            "epsxe",
            "mednafen-psx",
            "beetle psx",
            "pcsx2",
            "ppsspp",
            "rpcs3",
            "vita3k",
        ],
    ) {
        let slug = infer_sony_slug(&save_lower);
        if is_plausible_save_for_system(&save_ext, save_size, slug) {
            return classify_if_valid(
                save_path,
                &save_ext,
                save_size,
                slug,
                format!("path hint sony + .{} ({} bytes)", save_ext, save_size),
            );
        }
        return None;
    }

    if let Some(slug) = system_slug_from_save_extension(save_ext.as_str())
        && is_plausible_save_for_system(&save_ext, save_size, slug)
    {
        return classify_if_valid(
            save_path,
            &save_ext,
            save_size,
            slug,
            format!("save extension .{} ({} bytes)", save_ext, save_size),
        );
    }

    None
}

fn classify_if_valid(
    save_path: &Path,
    save_ext: &str,
    save_size: u64,
    slug: &str,
    mut evidence: String,
) -> Option<SaveClassification> {
    if !passes_binary_validation(save_path, save_ext, save_size, slug) {
        return None;
    }
    if slug == "dreamcast"
        && let Some(metadata) = inspect_dreamcast_metadata(save_path, save_ext)
    {
        let title = metadata.sample_title.unwrap_or_else(|| "-".to_string());
        let app = metadata.sample_app.unwrap_or_else(|| "-".to_string());
        evidence = format!(
            "{} [{} entries={} icons={} title={} app={}]",
            evidence, metadata.container, metadata.save_entries, metadata.icon_frames, title, app
        );
    }
    if slug == "saturn"
        && let Some(metadata) = inspect_saturn_metadata(save_path)
    {
        evidence = format!(
            "{} [{} entries={} internal={} cartridge={}]",
            evidence,
            metadata.format,
            metadata.save_entries,
            metadata.has_internal,
            metadata.has_cartridge
        );
    }
    if slug == "wii"
        && let Some(metadata) = inspect_wii_metadata(save_path)
    {
        let title_code = metadata.title_code.unwrap_or_else(|| "-".to_string());
        let embedded_file = metadata
            .embedded_file_name
            .unwrap_or_else(|| "-".to_string());
        evidence = format!(
            "{} [wii-data-bin titleCode={} files={} declared={} embedded={} embeddedSize={} certificate={}]",
            evidence,
            title_code,
            metadata.file_count,
            metadata.declared_data_size,
            embedded_file,
            metadata.embedded_file_size,
            metadata.certificate_present
        );
    }
    Some(SaveClassification {
        system_slug: slug.to_string(),
        evidence,
    })
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
        "32x" => Some("sega-32x"),
        "sms" => Some("master-system"),
        "gg" => Some("game-gear"),
        "gdi" | "cdi" => Some("dreamcast"),
        "rvz" | "wbfs" | "wad" => Some("wii"),
        "pbp" | "cso" => Some("psp"),
        "vpk" => Some("psvita"),
        _ => None,
    }
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

fn infer_nintendo_slug(haystack: &str) -> &'static str {
    if contains_any(
        haystack,
        &["nintendo wii", "/wii/", "\\wii\\", "dolphin", "rvl"],
    ) {
        return "wii";
    }
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
    if contains_any(
        haystack,
        &[
            "dreamcast",
            "/dreamcast/",
            "\\dreamcast\\",
            "/dc/",
            "\\dc\\",
            "flycast",
            "redream",
            "demul",
            "reicast",
            "vmu",
        ],
    ) {
        return "dreamcast";
    }
    if contains_any(haystack, &["master system", "/sms/", "\\sms\\"]) {
        return "master-system";
    }
    if contains_any(haystack, &["game gear", "/gg/", "\\gg\\"]) {
        return "game-gear";
    }
    if contains_any(
        haystack,
        &[
            "saturn",
            "/saturn/",
            "\\saturn\\",
            "yabause",
            "yabasanshiro",
            "kronos",
            "ssf",
            "beetle saturn",
            "mednafen saturn",
        ],
    ) {
        return "saturn";
    }
    if contains_any(
        haystack,
        &[
            "mega cd",
            "mega-cd",
            "megacd",
            "sega cd",
            "sega-cd",
            "segacd",
            "/megacd/",
            "\\megacd\\",
            "/mega-cd/",
            "\\mega-cd\\",
            "/segacd/",
            "\\segacd\\",
            "/sega-cd/",
            "\\sega-cd\\",
        ],
    ) {
        return "sega-cd";
    }
    if contains_any(
        haystack,
        &["sega 32x", "sega-32x", "sega32x", "32x", "/32x/", "\\32x\\"],
    ) {
        return "sega-32x";
    }
    "genesis"
}

fn infer_sony_slug(haystack: &str) -> &'static str {
    if contains_any(
        haystack,
        &["playstation 5", "/ps5/", "\\ps5\\", "sony ps5", "ps5"],
    ) {
        return "ps5";
    }
    if contains_any(
        haystack,
        &["playstation 4", "/ps4/", "\\ps4\\", "sony ps4", "ps4"],
    ) {
        return "ps4";
    }
    if contains_any(
        haystack,
        &[
            "playstation 3",
            "/ps3/",
            "\\ps3\\",
            "sony ps3",
            "rpcs3",
            "ps3",
        ],
    ) {
        return "ps3";
    }
    if contains_any(
        haystack,
        &[
            "playstation vita",
            "ps vita",
            "/psvita/",
            "\\psvita\\",
            "vita3k",
            "/vita/",
            "\\vita\\",
            "psvita",
        ],
    ) {
        return "psvita";
    }
    if contains_any(
        haystack,
        &[
            "playstation portable",
            "/psp/",
            "\\psp\\",
            "ppsspp",
            "sony psp",
            "psp",
        ],
    ) {
        return "psp";
    }
    if contains_any(
        haystack,
        &[
            "playstation 2",
            "/ps2/",
            "\\ps2\\",
            "pcsx2",
            "sony ps2",
            "ps2",
        ],
    ) {
        return "ps2";
    }
    "psx"
}

fn system_slug_from_save_extension(ext: &str) -> Option<&'static str> {
    match ext {
        "eep" | "fla" | "sra" | "mpk" | "cpk" => Some("n64"),
        "dsv" => Some("nds"),
        "mcr" | "mc" | "mcd" | "vmp" | "psv" => Some("psx"),
        "ps2" | "bin" => Some("ps2"),
        "vms" | "dci" => Some("dreamcast"),
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
        "n64" => matches!(ext, "eep" | "fla" | "sra" | "mpk" | "cpk" | "srm"),
        "nds" => matches!(ext, "sav" | "dsv"),
        "genesis" => matches!(ext, "sav" | "srm" | "ram"),
        "master-system" | "game-gear" | "sega-cd" | "sega-32x" => {
            matches!(ext, "sav" | "srm" | "ram")
        }
        "saturn" => matches!(ext, "sav" | "srm" | "ram" | "bkr"),
        "dreamcast" => matches!(ext, "bin" | "vms" | "dci"),
        "neogeo" => matches!(ext, "sav" | "srm" | "ram"),
        "wii" => ext == "bin",
        "psx" => matches!(
            ext,
            "sav" | "srm" | "ram" | "mcr" | "mc" | "mcd" | "vmp" | "psv" | "gme"
        ),
        "ps2" => matches!(ext, "ps2" | "bin"),
        "psp" | "psvita" | "ps3" | "ps4" | "ps5" => matches!(ext, "sav" | "srm" | "ram"),
        _ => false,
    };
    if !extension_ok {
        return false;
    }

    match slug {
        "nes" => matches!(
            size,
            512 | 1024 | 2048 | 4096 | 8192 | 16384 | 32768 | 65536 | 131072
        ),
        "snes" => matches!(
            size,
            512 | 1024 | 2048 | 4096 | 8192 | 16384 | 32768 | 65536 | 131072
        ),
        "gameboy" => matches!(
            size,
            512 | 1024 | 2048 | 4096 | 8192 | 16384 | 32768 | 65536
        ),
        "gba" => matches!(size, 512 | 8192 | 32768 | 65536 | 131072),
        "n64" => match ext {
            "eep" => size == 512 || size == 2048,
            "sra" | "mpk" | "cpk" => size == 32 * 1024,
            "fla" => size == 128 * 1024,
            "srm" => size == N64_RETROARCH_SRM_SIZE,
            _ => false,
        },
        "nds" => size.is_power_of_two() && (512..=16_777_216).contains(&size),
        "genesis" | "master-system" | "game-gear" | "sega-cd" | "sega-32x" => {
            matches!(
                size,
                64 | 128 | 256 | 512 | 1024 | 2048 | 4096 | 8192 | 16384 | 32768 | 65536 | 131072
            )
        }
        "saturn" => matches!(
            size as usize,
            SATURN_INTERNAL_RAW_SIZE
                | SATURN_INTERNAL_INTERLEAVED_SIZE
                | SATURN_CARTRIDGE_RAW_SIZE
                | SATURN_CARTRIDGE_INTERLEAVED_SIZE
                | SATURN_COMBINED_RAW_SIZE
                | SATURN_COMBINED_INTERLEAVED_SIZE
                | SATURN_YABASANSHIRO_RAW_SIZE
                | SATURN_YABASANSHIRO_INTERLEAVED_SIZE
        ),
        "dreamcast" => {
            if ext == "bin" {
                size as usize == DC_VMU_SIZE
            } else if ext == "dci" {
                (DC_DCI_HEADER_SIZE + DC_BLOCK_SIZE) as u64 <= size
                    && size <= (DC_DCI_HEADER_SIZE + DC_VMU_SIZE) as u64
                    && (size as usize - DC_DCI_HEADER_SIZE).is_multiple_of(DC_BLOCK_SIZE)
            } else {
                (DC_BLOCK_SIZE as u64..=DC_VMU_SIZE as u64).contains(&size)
                    && size.is_multiple_of(DC_BLOCK_SIZE as u64)
            }
        }
        "neogeo" => {
            // MiSTer NeoGeo backup RAM commonly includes a 0x2000 metadata/padding area
            // around the 64 KiB payload, so 0x12000 is valid even though it is not
            // a power-of-two size.
            size == 0x12000 || (size.is_power_of_two() && (512..=2_097_152).contains(&size))
        }
        "wii" => (WII_DATA_BIN_FILE_HEADER_OFFSET + 0x80..=MAX_SAVE_BYTES as usize)
            .contains(&(size as usize)),
        "psx" => {
            let size = size as usize;
            size == PS1_MEMCARD_SIZE
                || size == PS1_DEXDRIVE_HEADER_LENGTH + PS1_MEMCARD_SIZE
                || size == PS1_PSP_VMP_HEADER_LENGTH + PS1_MEMCARD_SIZE
        }
        "ps2" => (8 * 1024 * 1024..=128 * 1024 * 1024).contains(&size) && size.is_multiple_of(512),
        "psp" | "psvita" | "ps3" => (1024..=67_108_864).contains(&size),
        "ps4" | "ps5" => (1024..=268_435_456).contains(&size),
        _ => false,
    }
}

fn passes_binary_validation(save_path: &Path, save_ext: &str, save_size: u64, slug: &str) -> bool {
    if looks_like_executable_or_archive(save_path) {
        return false;
    }

    match slug {
        "n64" => validate_n64_save_media(save_path, save_ext, save_size),
        "dreamcast" => validate_dreamcast_container(save_path, save_ext),
        "saturn" => validate_saturn_backup_ram(save_path),
        "neogeo" => validate_non_blank_payload(save_path),
        "wii" => validate_wii_data_bin(save_path),
        "psx" => validate_psx_container(save_path, save_ext),
        "ps2" => validate_ps2_memory_card_image(save_path),
        _ => true,
    }
}

fn validate_wii_data_bin(path: &Path) -> bool {
    inspect_wii_metadata(path).is_some()
}

fn inspect_wii_metadata(path: &Path) -> Option<WiiMetadata> {
    let bytes = fs::read(path).ok()?;
    parse_wii_data_bin(&bytes, wii_title_code_from_path(path))
}

fn parse_wii_data_bin(bytes: &[u8], title_code: Option<String>) -> Option<WiiMetadata> {
    if bytes.len() < WII_DATA_BIN_FILE_HEADER_OFFSET + 0x80 {
        return None;
    }
    if bytes.iter().all(|value| *value == 0x00) || bytes.iter().all(|value| *value == 0xFF) {
        return None;
    }
    if &bytes[WII_DATA_BIN_BACKUP_HEADER_OFFSET + 4..WII_DATA_BIN_BACKUP_HEADER_OFFSET + 8]
        != b"Bk\0\x01"
    {
        return None;
    }
    let header_size = read_be_u32(bytes, WII_DATA_BIN_BACKUP_HEADER_OFFSET)?;
    if header_size != 0x70 {
        return None;
    }
    let file_count = read_be_u32(bytes, WII_DATA_BIN_BACKUP_HEADER_OFFSET + 0x0C)?;
    if file_count == 0 || file_count > 64 {
        return None;
    }
    let declared_data_size = read_be_u32(bytes, WII_DATA_BIN_BACKUP_HEADER_OFFSET + 0x10)?;
    if declared_data_size == 0 || declared_data_size as usize > bytes.len() {
        return None;
    }
    let file_header_magic = read_be_u32(bytes, WII_DATA_BIN_FILE_HEADER_OFFSET)?;
    if file_header_magic != WII_DATA_BIN_FILE_HEADER_MAGIC {
        return None;
    }
    let embedded_file_size = read_be_u32(bytes, WII_DATA_BIN_FILE_HEADER_OFFSET + 4)?;
    if embedded_file_size == 0 || embedded_file_size as usize > bytes.len() {
        return None;
    }
    let header_end = bytes.len().min(WII_DATA_BIN_FILE_HEADER_OFFSET + 0x80);
    Some(WiiMetadata {
        title_code,
        file_count,
        declared_data_size,
        embedded_file_name: extract_wii_embedded_file_name(
            &bytes[WII_DATA_BIN_FILE_HEADER_OFFSET..header_end],
        ),
        embedded_file_size,
        certificate_present: bytes.windows(7).any(|window| window == b"Root-CA")
            || bytes.windows(5).any(|window| window == b"AP000"),
    })
}

fn read_be_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let chunk = bytes.get(offset..offset + 4)?;
    Some(u32::from_be_bytes(chunk.try_into().ok()?))
}

fn extract_wii_embedded_file_name(header: &[u8]) -> Option<String> {
    let mut best = String::new();
    let mut start: Option<usize> = None;
    let flush = |end: usize, start: &mut Option<usize>, best: &mut String| {
        let Some(begin) = start.take() else {
            return;
        };
        if end <= begin {
            return;
        }
        let candidate = String::from_utf8_lossy(&header[begin..end])
            .trim()
            .to_string();
        if candidate.len() < 3 {
            return;
        }
        if !candidate.contains('.') && !candidate.contains('/') {
            return;
        }
        if candidate.len() > best.len() {
            *best = candidate;
        }
    };

    for (idx, value) in header.iter().enumerate() {
        if (0x20..=0x7E).contains(value) {
            start.get_or_insert(idx);
        } else {
            flush(idx, &mut start, &mut best);
        }
    }
    flush(header.len(), &mut start, &mut best);

    if best.is_empty() { None } else { Some(best) }
}

fn normalize_wii_title_code(value: &str) -> Option<String> {
    let clean = value.trim().to_ascii_uppercase();
    if clean.len() != 4 {
        return None;
    }
    if clean
        .chars()
        .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit())
    {
        Some(clean)
    } else {
        None
    }
}

fn looks_like_wii_data_bin_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|value| value.to_str())
        .map(|value| value.eq_ignore_ascii_case("data.bin"))
        .unwrap_or(false)
        && wii_title_code_from_path(path).is_some()
}

fn validate_non_blank_payload(path: &Path) -> bool {
    let Ok(bytes) = fs::read(path) else {
        return false;
    };
    if bytes.is_empty() {
        return false;
    }
    let all_zero = bytes.iter().all(|value| *value == 0x00);
    let all_ff = bytes.iter().all(|value| *value == 0xFF);
    !(all_zero || all_ff)
}

fn validate_n64_save_media(path: &Path, ext: &str, size: u64) -> bool {
    let expected_size_ok = match ext {
        "eep" => matches!(size, 512 | 2048),
        "sra" => size == 32 * 1024,
        "mpk" | "cpk" => size == 32 * 1024,
        "fla" => size == 128 * 1024,
        "srm" => size == N64_RETROARCH_SRM_SIZE,
        _ => false,
    };
    if !expected_size_ok {
        return false;
    }

    let Ok(bytes) = fs::read(path) else {
        return false;
    };
    if bytes.is_empty() {
        return false;
    }
    let all_zero = bytes.iter().all(|value| *value == 0x00);
    let all_ff = bytes.iter().all(|value| *value == 0xFF);
    !(all_zero || all_ff)
}

fn looks_like_executable_or_archive(path: &Path) -> bool {
    let Ok(file) = File::open(path) else {
        return false;
    };
    let mut reader = BufReader::new(file);
    let mut header = [0u8; 8];
    let Ok(read) = reader.read(&mut header) else {
        return false;
    };
    if read < 2 {
        return false;
    }

    let slice = &header[..read];
    slice.starts_with(b"MZ")
        || slice.starts_with(b"\x7fELF")
        || slice.starts_with(b"PK\x03\x04")
        || slice.starts_with(b"PK\x05\x06")
        || slice.starts_with(b"PK\x07\x08")
        || slice.starts_with(b"\x1f\x8b")
        || slice.starts_with(&[0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C])
}

fn validate_ps2_memory_card_image(path: &Path) -> bool {
    let Ok(file) = File::open(path) else {
        return false;
    };
    let mut reader = BufReader::new(file);
    let mut magic = vec![0u8; PS2_MEMCARD_MAGIC.len()];
    if reader.read_exact(&mut magic).is_err() {
        return false;
    }
    magic == PS2_MEMCARD_MAGIC
}

fn validate_dreamcast_container(path: &Path, ext: &str) -> bool {
    inspect_dreamcast_metadata(path, ext)
        .map(|metadata| metadata.save_entries > 0)
        .unwrap_or(false)
}

fn validate_saturn_backup_ram(path: &Path) -> bool {
    inspect_saturn_metadata(path)
        .map(|metadata| metadata.save_entries > 0)
        .unwrap_or(false)
}

fn inspect_saturn_metadata(path: &Path) -> Option<SaturnMetadata> {
    let bytes = std::fs::read(path).ok()?;
    inspect_saturn_bytes(&bytes)
}

fn inspect_saturn_bytes(bytes: &[u8]) -> Option<SaturnMetadata> {
    match bytes.len() {
        SATURN_INTERNAL_RAW_SIZE => {
            let internal_entries = inspect_saturn_volume(bytes, SATURN_INTERNAL_BLOCK_SIZE)?;
            Some(SaturnMetadata {
                format: "internal-raw",
                save_entries: internal_entries,
                has_internal: true,
                has_cartridge: false,
            })
        }
        SATURN_CARTRIDGE_RAW_SIZE => {
            let cart_entries = inspect_saturn_volume(bytes, SATURN_CARTRIDGE_BLOCK_SIZE)?;
            Some(SaturnMetadata {
                format: "cartridge-raw",
                save_entries: cart_entries,
                has_internal: false,
                has_cartridge: true,
            })
        }
        SATURN_INTERNAL_INTERLEAVED_SIZE => {
            let collapsed = collapse_saturn_byte_expanded(bytes)?;
            let internal_entries = inspect_saturn_volume(&collapsed, SATURN_INTERNAL_BLOCK_SIZE)?;
            Some(SaturnMetadata {
                format: "mister-internal-interleaved",
                save_entries: internal_entries,
                has_internal: true,
                has_cartridge: false,
            })
        }
        SATURN_CARTRIDGE_INTERLEAVED_SIZE => {
            let collapsed = collapse_saturn_byte_expanded(bytes)?;
            let cart_entries = inspect_saturn_volume(&collapsed, SATURN_CARTRIDGE_BLOCK_SIZE)?;
            Some(SaturnMetadata {
                format: "cartridge-interleaved",
                save_entries: cart_entries,
                has_internal: false,
                has_cartridge: true,
            })
        }
        SATURN_COMBINED_RAW_SIZE => {
            let internal_entries = inspect_saturn_volume(
                &bytes[..SATURN_INTERNAL_RAW_SIZE],
                SATURN_INTERNAL_BLOCK_SIZE,
            )?;
            let cart_entries = inspect_optional_saturn_volume(
                &bytes[SATURN_INTERNAL_RAW_SIZE..],
                SATURN_CARTRIDGE_BLOCK_SIZE,
            )?;
            Some(SaturnMetadata {
                format: "combined-raw",
                save_entries: internal_entries + cart_entries,
                has_internal: true,
                has_cartridge: true,
            })
        }
        SATURN_COMBINED_INTERLEAVED_SIZE => {
            let internal =
                collapse_saturn_byte_expanded(&bytes[..SATURN_INTERNAL_INTERLEAVED_SIZE])?;
            let cart = collapse_saturn_byte_expanded(&bytes[SATURN_INTERNAL_INTERLEAVED_SIZE..])?;
            let internal_entries = inspect_saturn_volume(&internal, SATURN_INTERNAL_BLOCK_SIZE)?;
            let cart_entries = inspect_optional_saturn_volume(&cart, SATURN_CARTRIDGE_BLOCK_SIZE)?;
            Some(SaturnMetadata {
                format: "mister-combined-interleaved",
                save_entries: internal_entries + cart_entries,
                has_internal: true,
                has_cartridge: true,
            })
        }
        SATURN_YABASANSHIRO_RAW_SIZE => {
            let internal_entries = inspect_saturn_volume(bytes, SATURN_INTERNAL_BLOCK_SIZE)?;
            Some(SaturnMetadata {
                format: "yabasanshiro-raw",
                save_entries: internal_entries,
                has_internal: true,
                has_cartridge: false,
            })
        }
        SATURN_YABASANSHIRO_INTERLEAVED_SIZE => {
            let collapsed = collapse_saturn_byte_expanded(bytes)?;
            let internal_entries = inspect_saturn_volume(&collapsed, SATURN_INTERNAL_BLOCK_SIZE)?;
            Some(SaturnMetadata {
                format: "yabasanshiro-interleaved",
                save_entries: internal_entries,
                has_internal: true,
                has_cartridge: false,
            })
        }
        _ => None,
    }
}

fn inspect_optional_saturn_volume(raw: &[u8], block_size: usize) -> Option<usize> {
    if raw.is_empty() || raw.iter().all(|value| *value == 0) {
        return Some(0);
    }
    inspect_saturn_volume(raw, block_size)
}

fn inspect_saturn_volume(raw: &[u8], block_size: usize) -> Option<usize> {
    if block_size == 0 || raw.len() < block_size * 2 || !saturn_header_valid(raw, block_size) {
        return None;
    }

    let total_blocks = raw.len() / block_size;
    let mut save_entries = 0usize;
    for block in 2..total_blocks {
        let offset = block.checked_mul(block_size)?;
        let marker = be_u32(raw, offset)?;
        if marker != SATURN_ARCHIVE_ENTRY_MARKER {
            continue;
        }
        if validate_saturn_archive_entry(raw, block_size, total_blocks, block) {
            save_entries += 1;
        }
    }
    Some(save_entries)
}

fn validate_saturn_archive_entry(
    raw: &[u8],
    block_size: usize,
    total_blocks: usize,
    first_block: usize,
) -> bool {
    let offset = match first_block.checked_mul(block_size) {
        Some(value) => value,
        None => return false,
    };
    if offset + 0x22 > raw.len() {
        return false;
    }
    if raw[offset + 0x0F] > 5 {
        return false;
    }
    let save_size = match be_u32(raw, offset + 0x1E) {
        Some(value) => value as usize,
        None => return false,
    };
    if save_size > raw.len() {
        return false;
    }
    let blocks = match saturn_read_block_list(raw, block_size, total_blocks, first_block) {
        Some(value) if !value.is_empty() => value,
        _ => return false,
    };
    saturn_entry_data_present(raw, block_size, &blocks, save_size)
}

fn saturn_read_block_list(
    raw: &[u8],
    block_size: usize,
    total_blocks: usize,
    first_block: usize,
) -> Option<Vec<usize>> {
    let mut offset = first_block.checked_mul(block_size)?.checked_add(0x22)?;
    let mut blocks = vec![first_block];
    let mut list_index = 1usize;
    loop {
        let next_block = be_u16(raw, offset)? as usize;
        if next_block == 0 {
            break;
        }
        if next_block >= total_blocks {
            return None;
        }
        blocks.push(next_block);
        offset = offset.checked_add(2)?;
        if offset % block_size == 0 {
            let next_list_block = *blocks.get(list_index)?;
            offset = next_list_block.checked_mul(block_size)?.checked_add(4)?;
            list_index += 1;
        }
    }
    Some(blocks)
}

fn saturn_entry_data_present(raw: &[u8], block_size: usize, blocks: &[usize], size: usize) -> bool {
    let mut block_list_remaining = blocks.len().saturating_mul(2);
    let mut remaining = size;
    for (index, block) in blocks.iter().enumerate() {
        if remaining == 0 {
            break;
        }
        let block_offset = match block.checked_mul(block_size) {
            Some(value) => value,
            None => return false,
        };
        let mut inner_offset = if index == 0 { 0x22 } else { 0x04 };
        let mut available = block_size.saturating_sub(inner_offset);
        if block_list_remaining >= available {
            block_list_remaining -= available;
            continue;
        }
        if block_list_remaining > 0 {
            inner_offset += block_list_remaining;
            available = available.saturating_sub(block_list_remaining);
            block_list_remaining = 0;
        }
        if block_offset
            .checked_add(inner_offset)
            .and_then(|value| value.checked_add(available))
            .filter(|value| *value <= raw.len())
            .is_none()
        {
            return false;
        }
        let take = available.min(remaining);
        remaining -= take;
    }
    remaining == 0
}

fn saturn_header_valid(raw: &[u8], block_size: usize) -> bool {
    if raw.len() < block_size * 2 {
        return false;
    }
    let limit = SATURN_MIN_MAGIC_BYTES.min(block_size);
    for index in 0..limit {
        if raw[index] != SATURN_HEADER_MAGIC[index % SATURN_HEADER_MAGIC.len()] {
            return false;
        }
    }
    raw[block_size..block_size * 2]
        .iter()
        .all(|value| *value == 0)
}

fn collapse_saturn_byte_expanded(bytes: &[u8]) -> Option<Vec<u8>> {
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let mut out = vec![0u8; bytes.len() / 2];
    for (index, value) in out.iter_mut().enumerate() {
        *value = bytes[index * 2 + 1];
    }
    Some(out)
}

fn be_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let a = *bytes.get(offset)?;
    let b = *bytes.get(offset + 1)?;
    Some(u16::from_be_bytes([a, b]))
}

fn be_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let a = *bytes.get(offset)?;
    let b = *bytes.get(offset + 1)?;
    let c = *bytes.get(offset + 2)?;
    let d = *bytes.get(offset + 3)?;
    Some(u32::from_be_bytes([a, b, c, d]))
}

fn inspect_dreamcast_metadata(path: &Path, ext: &str) -> Option<DreamcastMetadata> {
    let bytes = std::fs::read(path).ok()?;
    match ext {
        "bin" => inspect_dreamcast_vmu_image(&bytes),
        "vms" => inspect_dreamcast_vms_payload(&bytes),
        "dci" => inspect_dreamcast_dci_payload(&bytes),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy)]
struct DreamcastRoot {
    fat_loc: u16,
    fat_size: u16,
    dir_loc: u16,
    dir_size: u16,
    user_blocks: u16,
}

#[derive(Debug, Clone)]
struct DreamcastDirEntry {
    file_type: u8,
    first_block: u16,
    file_size_blocks: u16,
    header_offset_blocks: u16,
}

fn inspect_dreamcast_vmu_image(bytes: &[u8]) -> Option<DreamcastMetadata> {
    if bytes.len() != DC_VMU_SIZE || bytes.starts_with(DC_NVMEM_MAGIC) {
        return None;
    }

    let root = parse_dreamcast_root(bytes)?;
    let fat = parse_dreamcast_fat(bytes, root)?;
    let directory_blocks = collect_dreamcast_block_chain(
        root.dir_loc as usize,
        root.dir_size as usize,
        &fat,
        root.user_blocks as usize + root.dir_size as usize + root.fat_size as usize + 4,
    )?;

    let mut save_entries = 0usize;
    let mut icon_frames = 0usize;
    let mut sample_title: Option<String> = None;
    let mut sample_app: Option<String> = None;

    for block in directory_blocks {
        let start = block.checked_mul(DC_BLOCK_SIZE)?;
        let end = start.checked_add(DC_BLOCK_SIZE)?;
        if end > bytes.len() {
            return None;
        }
        for chunk in bytes[start..end].chunks_exact(DC_DIR_ENTRY_SIZE) {
            let entry = parse_dreamcast_dir_entry(chunk)?;
            if entry.file_type != DC_FILETYPE_DATA && entry.file_type != DC_FILETYPE_GAME {
                continue;
            }
            save_entries += 1;
            let file_bytes = collect_dreamcast_file_bytes(bytes, &fat, &entry)?;
            let header_offset = entry.header_offset_blocks as usize * DC_BLOCK_SIZE;
            if header_offset >= file_bytes.len() {
                continue;
            }
            if let Some(header) = parse_dreamcast_pkg_header(&file_bytes[header_offset..]) {
                icon_frames += header.icon_count as usize;
                if sample_title.is_none() && !header.short_desc.is_empty() {
                    sample_title = Some(header.short_desc);
                }
                if sample_app.is_none() && !header.app_id.is_empty() {
                    sample_app = Some(header.app_id);
                }
            }
        }
    }

    Some(DreamcastMetadata {
        container: "vmu-bin",
        save_entries,
        icon_frames,
        sample_title,
        sample_app,
    })
}

fn inspect_dreamcast_vms_payload(bytes: &[u8]) -> Option<DreamcastMetadata> {
    let header = parse_dreamcast_pkg_header(bytes)?;
    Some(DreamcastMetadata {
        container: "vms",
        save_entries: 1,
        icon_frames: header.icon_count as usize,
        sample_title: (!header.short_desc.is_empty()).then_some(header.short_desc),
        sample_app: (!header.app_id.is_empty()).then_some(header.app_id),
    })
}

fn inspect_dreamcast_dci_payload(bytes: &[u8]) -> Option<DreamcastMetadata> {
    if bytes.len() < DC_DCI_HEADER_SIZE + DC_BLOCK_SIZE {
        return None;
    }
    let dir_entry = parse_dreamcast_dir_entry(&bytes[..DC_DCI_HEADER_SIZE])?;
    if dir_entry.file_type != DC_FILETYPE_DATA && dir_entry.file_type != DC_FILETYPE_GAME {
        return None;
    }
    let file_blocks = dir_entry.file_size_blocks as usize;
    if file_blocks == 0 {
        return None;
    }
    let expected_len = DC_DCI_HEADER_SIZE + file_blocks * DC_BLOCK_SIZE;
    if bytes.len() < expected_len {
        return None;
    }
    let mut file_bytes = Vec::with_capacity(file_blocks * DC_BLOCK_SIZE);
    for block in bytes[DC_DCI_HEADER_SIZE..expected_len].chunks_exact(DC_BLOCK_SIZE) {
        file_bytes.extend_from_slice(&dreamcast_unswap_32bit_chunks(block));
    }

    let header_offset = dir_entry.header_offset_blocks as usize * DC_BLOCK_SIZE;
    if header_offset >= file_bytes.len() {
        return None;
    }
    let header = parse_dreamcast_pkg_header(&file_bytes[header_offset..])?;
    Some(DreamcastMetadata {
        container: "dci",
        save_entries: 1,
        icon_frames: header.icon_count as usize,
        sample_title: (!header.short_desc.is_empty()).then_some(header.short_desc),
        sample_app: (!header.app_id.is_empty()).then_some(header.app_id),
    })
}

fn parse_dreamcast_root(bytes: &[u8]) -> Option<DreamcastRoot> {
    if bytes.len() != DC_VMU_SIZE {
        return None;
    }
    let root_start = DC_ROOT_BLOCK * DC_BLOCK_SIZE;
    let root_end = root_start + DC_BLOCK_SIZE;
    let root = bytes.get(root_start..root_end)?;
    if !root[..16].iter().all(|value| *value == 0x55) {
        return None;
    }

    let fat_loc = le_u16(root, 0x46)?;
    let fat_size = le_u16(root, 0x48)?;
    let dir_loc = le_u16(root, 0x4A)?;
    let dir_size = le_u16(root, 0x4C)?;
    let user_blocks = le_u16(root, 0x50)?;

    if fat_size == 0 || dir_size == 0 || fat_loc as usize >= DC_VMU_BLOCK_COUNT {
        return None;
    }
    if dir_loc as usize >= DC_VMU_BLOCK_COUNT || user_blocks == 0 || user_blocks > 200 {
        return None;
    }

    Some(DreamcastRoot {
        fat_loc,
        fat_size,
        dir_loc,
        dir_size,
        user_blocks,
    })
}

fn parse_dreamcast_fat(bytes: &[u8], root: DreamcastRoot) -> Option<Vec<u16>> {
    let fat_start = root.fat_loc as usize * DC_BLOCK_SIZE;
    let fat_len = root.fat_size as usize * DC_BLOCK_SIZE;
    let fat_end = fat_start.checked_add(fat_len)?;
    let fat_bytes = bytes.get(fat_start..fat_end)?;
    if fat_bytes.len() < DC_VMU_BLOCK_COUNT * 2 {
        return None;
    }
    let mut out = Vec::with_capacity(DC_VMU_BLOCK_COUNT);
    for index in 0..DC_VMU_BLOCK_COUNT {
        out.push(le_u16(fat_bytes, index * 2)?);
    }
    Some(out)
}

fn parse_dreamcast_dir_entry(bytes: &[u8]) -> Option<DreamcastDirEntry> {
    if bytes.len() != DC_DIR_ENTRY_SIZE {
        return None;
    }
    let file_type = bytes[0];
    if file_type == 0 {
        return Some(DreamcastDirEntry {
            file_type,
            first_block: 0,
            file_size_blocks: 0,
            header_offset_blocks: 0,
        });
    }

    let first_block = le_u16(bytes, 0x02)?;
    let file_size_blocks = le_u16(bytes, 0x18)?;
    let header_offset_blocks = le_u16(bytes, 0x1A)?;
    if first_block as usize >= DC_VMU_BLOCK_COUNT || file_size_blocks == 0 {
        return None;
    }
    if header_offset_blocks > file_size_blocks {
        return None;
    }

    Some(DreamcastDirEntry {
        file_type,
        first_block,
        file_size_blocks,
        header_offset_blocks,
    })
}

fn collect_dreamcast_file_bytes(
    bytes: &[u8],
    fat: &[u16],
    entry: &DreamcastDirEntry,
) -> Option<Vec<u8>> {
    let chain = collect_dreamcast_block_chain(
        entry.first_block as usize,
        entry.file_size_blocks as usize,
        fat,
        entry.file_size_blocks as usize + 2,
    )?;
    if chain.is_empty() || chain.len() > entry.file_size_blocks as usize {
        return None;
    }

    let mut file_bytes = Vec::with_capacity(chain.len() * DC_BLOCK_SIZE);
    for block in chain {
        if block >= 200 {
            return None;
        }
        let start = block.checked_mul(DC_BLOCK_SIZE)?;
        let end = start.checked_add(DC_BLOCK_SIZE)?;
        file_bytes.extend_from_slice(bytes.get(start..end)?);
    }
    Some(file_bytes)
}

fn collect_dreamcast_block_chain(
    start_block: usize,
    expected_blocks: usize,
    fat: &[u16],
    hard_limit: usize,
) -> Option<Vec<usize>> {
    if start_block >= fat.len() || expected_blocks == 0 {
        return None;
    }
    let mut out = Vec::with_capacity(expected_blocks);
    let mut seen = [false; DC_VMU_BLOCK_COUNT];
    let mut current = start_block;

    while out.len() < hard_limit && current < fat.len() {
        if current >= DC_VMU_BLOCK_COUNT || seen[current] {
            return None;
        }
        seen[current] = true;
        out.push(current);
        if out.len() >= expected_blocks {
            break;
        }
        let next = fat[current];
        if next == DC_FAT_END {
            break;
        }
        if next == DC_FAT_FREE {
            return None;
        }
        current = next as usize;
    }

    if out.is_empty() { None } else { Some(out) }
}

fn parse_dreamcast_pkg_header(bytes: &[u8]) -> Option<DreamcastPkgHeader> {
    if bytes.len() < 0x80 {
        return None;
    }
    let short_desc = decode_printable_ascii(&bytes[0x00..0x10]);
    let app_id = decode_printable_ascii(&bytes[0x30..0x40]);
    let icon_count = le_u16(bytes, 0x40)?;
    if icon_count > 16 {
        return None;
    }
    let expected_min = 0x80usize.checked_add(icon_count as usize * 512)?;
    if expected_min > bytes.len() {
        return None;
    }
    Some(DreamcastPkgHeader {
        short_desc,
        app_id,
        icon_count,
    })
}

fn dreamcast_unswap_32bit_chunks(bytes: &[u8]) -> Vec<u8> {
    let mut out = bytes.to_vec();
    for chunk in out.chunks_exact_mut(4) {
        chunk.reverse();
    }
    out
}

fn le_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let a = *bytes.get(offset)?;
    let b = *bytes.get(offset + 1)?;
    Some(u16::from_le_bytes([a, b]))
}

fn decode_printable_ascii(bytes: &[u8]) -> String {
    let mut out = String::new();
    for value in bytes {
        if *value == 0 {
            break;
        }
        if (0x20..=0x7e).contains(value) {
            out.push(*value as char);
        }
    }
    out.trim().to_string()
}

fn validate_psx_container(path: &Path, ext: &str) -> bool {
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    match ext {
        "gme" => decode_ps1_dexdrive(&bytes).is_some(),
        "vmp" => decode_ps1_vmp(&bytes).is_some(),
        _ => validate_ps1_raw_memcard(&bytes),
    }
}

fn validate_ps1_raw_memcard(bytes: &[u8]) -> bool {
    if bytes.len() != PS1_MEMCARD_SIZE {
        return false;
    }

    let header = &bytes[..PS1_HEADER_BLOCK_SIZE];
    let frame_count = PS1_HEADER_BLOCK_SIZE / PS1_FRAME_SIZE;
    if frame_count < 64 {
        return false;
    }

    let frame0 = &header[..PS1_FRAME_SIZE];
    if !frame0.starts_with(b"MC") || !frame_checksum_ok(frame0) {
        return false;
    }

    for frame_index in 1..=15 {
        let start = frame_index * PS1_FRAME_SIZE;
        let end = start + PS1_FRAME_SIZE;
        if !frame_checksum_ok(&header[start..end]) {
            return false;
        }
    }

    let trailing_start = 63 * PS1_FRAME_SIZE;
    let trailing_end = trailing_start + PS1_FRAME_SIZE;
    let trailing = &header[trailing_start..trailing_end];
    trailing.starts_with(b"MC") && frame_checksum_ok(trailing)
}

fn frame_checksum_ok(frame: &[u8]) -> bool {
    if frame.len() != PS1_FRAME_SIZE {
        return false;
    }
    let checksum = frame[..PS1_FRAME_SIZE - 1]
        .iter()
        .fold(0u8, |acc, value| acc ^ value);
    checksum == frame[PS1_FRAME_SIZE - 1]
}

fn decode_ps1_dexdrive(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.len() == PS1_MEMCARD_SIZE && validate_ps1_raw_memcard(bytes) {
        return Some(bytes.to_vec());
    }
    if bytes.len() != PS1_DEXDRIVE_HEADER_LENGTH + PS1_MEMCARD_SIZE {
        return None;
    }

    let header = &bytes[..PS1_DEXDRIVE_HEADER_LENGTH];
    let header_magic = header.starts_with(PS1_DEXDRIVE_MAGIC);
    let blankish_header = header.iter().filter(|value| **value != 0).count() <= 16;
    if !header_magic && !blankish_header {
        return None;
    }

    let payload = &bytes[PS1_DEXDRIVE_HEADER_LENGTH..];
    if !validate_ps1_raw_memcard(payload) {
        return None;
    }
    Some(payload.to_vec())
}

fn encode_ps1_dexdrive(raw: &[u8]) -> Vec<u8> {
    let mut header = vec![0u8; PS1_DEXDRIVE_HEADER_LENGTH];
    header[..PS1_DEXDRIVE_MAGIC.len()].copy_from_slice(PS1_DEXDRIVE_MAGIC);
    header[18] = 0x01;
    header[20] = 0x01;
    header[21] = b'M';
    [header, raw.to_vec()].concat()
}

fn decode_ps1_vmp(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.len() != PS1_PSP_VMP_HEADER_LENGTH + PS1_MEMCARD_SIZE {
        return None;
    }

    let header = &bytes[..PS1_PSP_VMP_HEADER_LENGTH];
    if header[..PS1_PSP_VMP_MAGIC.len()] != PS1_PSP_VMP_MAGIC {
        return None;
    }

    let salt_seed = &header
        [PS1_PSP_VMP_SALT_SEED_OFFSET..PS1_PSP_VMP_SALT_SEED_OFFSET + PS1_PSP_VMP_SALT_SEED_LEN];
    let signature_found = &header
        [PS1_PSP_VMP_SIGNATURE_OFFSET..PS1_PSP_VMP_SIGNATURE_OFFSET + PS1_PSP_VMP_SIGNATURE_LEN];
    let signature_calculated = calculate_ps1_vmp_signature(bytes, salt_seed)?;
    if signature_found != signature_calculated {
        return None;
    }

    let payload = &bytes[PS1_PSP_VMP_HEADER_LENGTH..];
    if !validate_ps1_raw_memcard(payload) {
        return None;
    }
    Some(payload.to_vec())
}

fn encode_ps1_vmp(raw: &[u8]) -> Result<Vec<u8>> {
    if !validate_ps1_raw_memcard(raw) {
        anyhow::bail!("kan geen VMP maken: input is geen geldige PS1 memory card");
    }

    let mut output = vec![0u8; PS1_PSP_VMP_HEADER_LENGTH + raw.len()];
    output[..PS1_PSP_VMP_MAGIC.len()].copy_from_slice(&PS1_PSP_VMP_MAGIC);
    output[PS1_PSP_VMP_SALT_SEED_OFFSET..PS1_PSP_VMP_SALT_SEED_OFFSET + PS1_PSP_VMP_SALT_SEED_LEN]
        .copy_from_slice(&PS1_PSP_VMP_SALT_SEED_INIT);
    output[PS1_PSP_VMP_HEADER_LENGTH..].copy_from_slice(raw);

    let signature = calculate_ps1_vmp_signature(&output, &PS1_PSP_VMP_SALT_SEED_INIT)
        .context("kon VMP signature niet berekenen")?;
    output[PS1_PSP_VMP_SIGNATURE_OFFSET..PS1_PSP_VMP_SIGNATURE_OFFSET + PS1_PSP_VMP_SIGNATURE_LEN]
        .copy_from_slice(&signature);

    Ok(output)
}

fn calculate_ps1_vmp_signature(full_bytes: &[u8], salt_seed: &[u8]) -> Option<[u8; 20]> {
    if full_bytes.len() < PS1_PSP_VMP_SIGNATURE_OFFSET + PS1_PSP_VMP_SIGNATURE_LEN
        || salt_seed.len() < PS1_PSP_VMP_SALT_SEED_LEN
    {
        return None;
    }

    let mut salt = [0u8; PS1_PSP_VMP_SALT_LEN];
    let mut seed_head = [0u8; 16];
    seed_head.copy_from_slice(&salt_seed[..16]);

    let decrypt = aes128_ecb_decrypt(seed_head);
    let encrypt = aes128_ecb_encrypt(seed_head);
    salt[..16].copy_from_slice(&decrypt);
    salt[16..32].copy_from_slice(&encrypt);

    for (index, value) in PS1_PSP_VMP_IV_PRETEND.iter().enumerate() {
        salt[index] ^= value;
    }

    let mut work = [0xFFu8; 16];
    work[..PS1_PSP_VMP_SALT_SEED_LEN - 16]
        .copy_from_slice(&salt_seed[16..PS1_PSP_VMP_SALT_SEED_LEN]);
    for (index, value) in work.iter().enumerate() {
        salt[16 + index] ^= value;
    }

    for value in salt.iter_mut().skip(PS1_PSP_VMP_SALT_SEED_LEN) {
        *value = 0;
    }
    for value in &mut salt {
        *value ^= 0x36;
    }

    let mut hash_input = full_bytes.to_vec();
    hash_input
        [PS1_PSP_VMP_SIGNATURE_OFFSET..PS1_PSP_VMP_SIGNATURE_OFFSET + PS1_PSP_VMP_SIGNATURE_LEN]
        .fill(0);

    let mut hash1 = Sha1::new();
    hash1.update(salt);
    hash1.update(&hash_input);
    let digest1 = hash1.finalize();

    for value in &mut salt {
        *value ^= 0x6A;
    }

    let mut hash2 = Sha1::new();
    hash2.update(salt);
    hash2.update(digest1);
    let digest2 = hash2.finalize();

    let mut out = [0u8; 20];
    out.copy_from_slice(&digest2);
    Some(out)
}

fn aes128_ecb_encrypt(block: [u8; 16]) -> [u8; 16] {
    let cipher = Aes128::new(&GenericArray::from(PS1_PSP_VMP_KEY));
    let mut output = GenericArray::from(block);
    cipher.encrypt_block(&mut output);
    output.into()
}

fn aes128_ecb_decrypt(block: [u8; 16]) -> [u8; 16] {
    let cipher = Aes128::new(&GenericArray::from(PS1_PSP_VMP_KEY));
    let mut output = GenericArray::from(block);
    cipher.decrypt_block(&mut output);
    output.into()
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
    use std::io::Write;

    fn set_frame_checksum(bytes: &mut [u8], frame_index: usize) {
        let start = frame_index * PS1_FRAME_SIZE;
        let end = start + PS1_FRAME_SIZE;
        let frame = &mut bytes[start..end];
        let checksum = frame[..PS1_FRAME_SIZE - 1]
            .iter()
            .fold(0u8, |acc, value| acc ^ value);
        frame[PS1_FRAME_SIZE - 1] = checksum;
    }

    fn build_valid_ps1_memcard() -> Vec<u8> {
        let mut bytes = vec![0u8; PS1_MEMCARD_SIZE];
        bytes[0] = b'M';
        bytes[1] = b'C';

        for frame_index in 1..=15 {
            let start = frame_index * PS1_FRAME_SIZE;
            bytes[start] = 0xA0;
            bytes[start + 8] = 0xFF;
            bytes[start + 9] = 0xFF;
            set_frame_checksum(&mut bytes, frame_index);
        }

        let trailing_start = 63 * PS1_FRAME_SIZE;
        bytes[trailing_start] = b'M';
        bytes[trailing_start + 1] = b'C';
        set_frame_checksum(&mut bytes, 0);
        set_frame_checksum(&mut bytes, 63);
        bytes
    }

    fn build_empty_saturn_internal_backup_ram() -> Vec<u8> {
        let mut bytes = vec![0u8; SATURN_INTERNAL_RAW_SIZE];
        for index in 0..SATURN_INTERNAL_BLOCK_SIZE {
            bytes[index] = SATURN_HEADER_MAGIC[index % SATURN_HEADER_MAGIC.len()];
        }
        bytes
    }

    fn build_valid_saturn_internal_backup_ram() -> Vec<u8> {
        let mut bytes = build_empty_saturn_internal_backup_ram();
        let offset = 2 * SATURN_INTERNAL_BLOCK_SIZE;
        bytes[offset..offset + 4].copy_from_slice(&SATURN_ARCHIVE_ENTRY_MARKER.to_be_bytes());
        let mut filename = [0u8; 11];
        filename[..8].copy_from_slice(b"TESTSAVE");
        bytes[offset + 0x04..offset + 0x0F].copy_from_slice(&filename);
        bytes[offset + 0x0F] = 1;
        let mut comment = [0u8; 10];
        comment[..9].copy_from_slice(b"test save");
        bytes[offset + 0x10..offset + 0x1A].copy_from_slice(&comment);
        bytes[offset + 0x1A..offset + 0x1E].copy_from_slice(&12345u32.to_be_bytes());
        let payload = b"SATURN-OK";
        bytes[offset + 0x1E..offset + 0x22].copy_from_slice(&(payload.len() as u32).to_be_bytes());
        bytes[offset + 0x22..offset + 0x24].copy_from_slice(&0u16.to_be_bytes());
        bytes[offset + 0x24..offset + 0x24 + payload.len()].copy_from_slice(payload);
        bytes
    }

    fn write_ps2_memory_card(path: &Path) {
        let mut file = fs::File::create(path).unwrap();
        file.write_all(PS2_MEMCARD_MAGIC).unwrap();
        file.set_len(8 * 1024 * 1024).unwrap();
    }

    fn build_empty_dreamcast_vmu() -> Vec<u8> {
        let mut bytes = vec![0u8; DC_VMU_SIZE];

        let root_offset = DC_ROOT_BLOCK * DC_BLOCK_SIZE;
        bytes[root_offset..root_offset + 16].fill(0x55);
        bytes[root_offset + 0x46..root_offset + 0x48].copy_from_slice(&(254u16).to_le_bytes());
        bytes[root_offset + 0x48..root_offset + 0x4A].copy_from_slice(&(1u16).to_le_bytes());
        bytes[root_offset + 0x4A..root_offset + 0x4C].copy_from_slice(&(253u16).to_le_bytes());
        bytes[root_offset + 0x4C..root_offset + 0x4E].copy_from_slice(&(13u16).to_le_bytes());
        bytes[root_offset + 0x50..root_offset + 0x52].copy_from_slice(&(200u16).to_le_bytes());

        let fat_offset = 254 * DC_BLOCK_SIZE;
        for block in 0..DC_VMU_BLOCK_COUNT {
            let offset = fat_offset + block * 2;
            bytes[offset..offset + 2].copy_from_slice(&DC_FAT_FREE.to_le_bytes());
        }

        for block in (241..=253).rev() {
            let value = if block == 241 {
                DC_FAT_END
            } else {
                (block - 1) as u16
            };
            let offset = fat_offset + block * 2;
            bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
        }
        for block in [254usize, 255] {
            let offset = fat_offset + block * 2;
            bytes[offset..offset + 2].copy_from_slice(&DC_FAT_END.to_le_bytes());
        }
        bytes
    }

    fn build_dreamcast_vmu_with_single_save() -> Vec<u8> {
        let mut bytes = build_empty_dreamcast_vmu();
        let fat_offset = 254 * DC_BLOCK_SIZE;
        let save_block = 10usize;
        let next_save_block = 11usize;
        let offset = fat_offset + save_block * 2;
        bytes[offset..offset + 2].copy_from_slice(&(next_save_block as u16).to_le_bytes());
        let next_offset = fat_offset + next_save_block * 2;
        bytes[next_offset..next_offset + 2].copy_from_slice(&DC_FAT_END.to_le_bytes());

        let dir_offset = 253 * DC_BLOCK_SIZE;
        bytes[dir_offset] = DC_FILETYPE_DATA;
        bytes[dir_offset + 1] = 0x00;
        bytes[dir_offset + 2..dir_offset + 4].copy_from_slice(&(save_block as u16).to_le_bytes());
        let mut filename = [0u8; 12];
        filename[..9].copy_from_slice(b"SONICADV2");
        bytes[dir_offset + 4..dir_offset + 16].copy_from_slice(&filename);
        bytes[dir_offset + 0x18..dir_offset + 0x1A].copy_from_slice(&(2u16).to_le_bytes());
        bytes[dir_offset + 0x1A..dir_offset + 0x1C].copy_from_slice(&(0u16).to_le_bytes());

        let save_offset = save_block * DC_BLOCK_SIZE;
        let mut short = [0u8; 16];
        short[..10].copy_from_slice(b"SONIC ADV2");
        bytes[save_offset..save_offset + 16].copy_from_slice(&short);
        let mut app = [0u8; 16];
        app[..7].copy_from_slice(b"FLYCAST");
        bytes[save_offset + 0x30..save_offset + 0x40].copy_from_slice(&app);
        bytes[save_offset + 0x40..save_offset + 0x42].copy_from_slice(&(1u16).to_le_bytes());
        bytes[save_offset + 0x42..save_offset + 0x44].copy_from_slice(&(8u16).to_le_bytes());
        bytes[save_offset + 0x44..save_offset + 0x46].copy_from_slice(&(0u16).to_le_bytes());
        bytes[save_offset + 0x48..save_offset + 0x4C].copy_from_slice(&(64u32).to_le_bytes());

        bytes
    }

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
        let saturn = tmp.path().join("saves/Saturn/nihts.bkr");
        let megacd = tmp.path().join("saves/Mega-CD/snatcher.srm");
        let sega32x = tmp.path().join("saves/32X/doom.srm");
        let psx = tmp.path().join("saves/PSX/ff7.mcr");
        fs::create_dir_all(snes.parent().unwrap()).unwrap();
        fs::create_dir_all(sega.parent().unwrap()).unwrap();
        fs::create_dir_all(saturn.parent().unwrap()).unwrap();
        fs::create_dir_all(megacd.parent().unwrap()).unwrap();
        fs::create_dir_all(sega32x.parent().unwrap()).unwrap();
        fs::create_dir_all(psx.parent().unwrap()).unwrap();
        fs::write(&snes, vec![0x00u8; 8192]).unwrap();
        fs::write(&sega, vec![0x00u8; 8192]).unwrap();
        fs::write(&saturn, build_valid_saturn_internal_backup_ram()).unwrap();
        fs::write(&megacd, vec![0x00u8; 8192]).unwrap();
        fs::write(&sega32x, vec![0x00u8; 8192]).unwrap();
        fs::write(&psx, build_valid_ps1_memcard()).unwrap();
        assert_eq!(infer_system_slug(&snes).as_deref(), Some("snes"));
        assert_eq!(infer_system_slug(&sega).as_deref(), Some("genesis"));
        assert_eq!(infer_system_slug(&saturn).as_deref(), Some("saturn"));
        assert_eq!(infer_system_slug(&megacd).as_deref(), Some("sega-cd"));
        assert_eq!(infer_system_slug(&sega32x).as_deref(), Some("sega-32x"));
        assert_eq!(infer_system_slug(&psx).as_deref(), Some("psx"));
    }

    #[test]
    fn unsupported_paths_are_not_classified() {
        let path = PathBuf::from("/home/deck/.steam/steam/steamapps/compatdata/242550/icudtl.dat");
        assert!(infer_supported_console_slug(&path, None).is_none());
    }

    #[test]
    fn empty_saturn_backup_ram_is_not_classified() {
        let tmp = tempfile::tempdir().unwrap();
        let save = tmp.path().join("saves/Saturn/Fighting Vipers (USA).bkr");
        fs::create_dir_all(save.parent().unwrap()).unwrap();
        fs::write(&save, build_empty_saturn_internal_backup_ram()).unwrap();
        assert!(infer_supported_console_slug(&save, None).is_none());
    }

    #[test]
    fn empty_saturn_backup_ram_reports_skip_reason() {
        let tmp = tempfile::tempdir().unwrap();
        let save = tmp.path().join("saves/Saturn/Fighting Vipers (USA).bkr");
        fs::create_dir_all(save.parent().unwrap()).unwrap();
        fs::write(&save, build_empty_saturn_internal_backup_ram()).unwrap();
        assert_eq!(
            saturn_skip_reason(&save, None).as_deref(),
            Some("skip_empty_saturn_backup_ram")
        );
    }

    #[test]
    fn valid_saturn_backup_ram_has_no_skip_reason() {
        let tmp = tempfile::tempdir().unwrap();
        let save = tmp.path().join("saves/Saturn/Quake (USA).bkr");
        fs::create_dir_all(save.parent().unwrap()).unwrap();
        fs::write(&save, build_valid_saturn_internal_backup_ram()).unwrap();
        assert!(saturn_skip_reason(&save, None).is_none());
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
    fn rom_extension_32x_can_classify_supported_console() {
        let tmp = tempfile::tempdir().unwrap();
        let save = tmp.path().join("anything.sav");
        fs::write(&save, vec![0x00u8; 32768]).unwrap();
        let rom = PathBuf::from("/roms/sega32x/doom.32x");
        assert_eq!(
            infer_supported_console_slug(&save, Some(&rom)).as_deref(),
            Some("sega-32x")
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

    #[test]
    fn sony_path_hints_are_supported() {
        let tmp = tempfile::tempdir().unwrap();
        let save = tmp.path().join("Emulation/saves/pcsx2/Gran Turismo 4.ps2");
        fs::create_dir_all(save.parent().unwrap()).unwrap();
        write_ps2_memory_card(&save);

        assert_eq!(
            infer_supported_console_slug(&save, None).as_deref(),
            Some("ps2")
        );
    }

    #[test]
    fn dreamcast_vmu_path_hints_are_supported_and_enriched() {
        let tmp = tempfile::tempdir().unwrap();
        let save = tmp
            .path()
            .join("Emulation/saves/dreamcast/Sonic Adventure 2.A1.bin");
        fs::create_dir_all(save.parent().unwrap()).unwrap();
        fs::write(&save, build_dreamcast_vmu_with_single_save()).unwrap();

        let classification = classify_supported_save(&save, None).expect("expected classification");
        assert_eq!(classification.system_slug, "dreamcast");
        assert!(classification.evidence.contains("vmu-bin"));
        assert!(classification.evidence.contains("icons=1"));
        assert!(classification.evidence.contains("title=SONIC ADV2"));
    }

    #[test]
    fn empty_dreamcast_vmu_is_not_classified() {
        let tmp = tempfile::tempdir().unwrap();
        let save = tmp
            .path()
            .join("Emulation/saves/dreamcast/Sonic Adventure 2.A1.bin");
        fs::create_dir_all(save.parent().unwrap()).unwrap();
        fs::write(&save, build_empty_dreamcast_vmu()).unwrap();

        assert!(infer_supported_console_slug(&save, None).is_none());
    }

    #[test]
    fn empty_dreamcast_vmu_reports_skip_reason() {
        let tmp = tempfile::tempdir().unwrap();
        let save = tmp
            .path()
            .join("Emulation/saves/dreamcast/Sonic Adventure 2.A1.bin");
        fs::create_dir_all(save.parent().unwrap()).unwrap();
        fs::write(&save, build_empty_dreamcast_vmu()).unwrap();

        assert_eq!(
            dreamcast_skip_reason(&save, None).as_deref(),
            Some("skip_empty_dreamcast_vmu")
        );
    }

    #[test]
    fn valid_dreamcast_vmu_has_no_skip_reason() {
        let tmp = tempfile::tempdir().unwrap();
        let save = tmp
            .path()
            .join("Emulation/saves/dreamcast/Sonic Adventure 2.A1.bin");
        fs::create_dir_all(save.parent().unwrap()).unwrap();
        fs::write(&save, build_dreamcast_vmu_with_single_save()).unwrap();

        assert!(dreamcast_skip_reason(&save, None).is_none());
    }

    #[test]
    fn dreamcast_nvmem_blob_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let save = tmp.path().join("Emulation/saves/dreamcast/dc_nvmem.bin");
        fs::create_dir_all(save.parent().unwrap()).unwrap();
        let mut bytes = vec![0u8; DC_VMU_SIZE];
        bytes[..DC_NVMEM_MAGIC.len()].copy_from_slice(DC_NVMEM_MAGIC);
        fs::write(&save, bytes).unwrap();

        assert!(infer_supported_console_slug(&save, None).is_none());
    }

    #[test]
    fn n64_blank_eeprom_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let save = tmp.path().join("MiSTer/N64/Wave Race 64 (USA).eep");
        fs::create_dir_all(save.parent().unwrap()).unwrap();
        fs::write(&save, vec![0u8; 512]).unwrap();

        assert!(infer_supported_console_slug(&save, None).is_none());
    }

    #[test]
    fn n64_blank_flashram_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let save = tmp.path().join("MiSTer/N64/Paper Mario (USA).fla");
        fs::create_dir_all(save.parent().unwrap()).unwrap();
        fs::write(&save, vec![0xFFu8; 131072]).unwrap();

        assert!(infer_supported_console_slug(&save, None).is_none());
    }

    #[test]
    fn n64_blank_controller_pak_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let save = tmp.path().join("MiSTer/N64/Mario Kart 64 (USA).mpk");
        fs::create_dir_all(save.parent().unwrap()).unwrap();
        fs::write(&save, vec![0u8; 32768]).unwrap();

        assert!(infer_supported_console_slug(&save, None).is_none());
    }

    #[test]
    fn n64_non_blank_native_media_is_supported() {
        let tmp = tempfile::tempdir().unwrap();
        let save = tmp.path().join("MiSTer/N64/Super Mario 64 (USA).eep");
        fs::create_dir_all(save.parent().unwrap()).unwrap();
        let mut payload = vec![0u8; 512];
        payload[0] = 0x11;
        payload[127] = 0x22;
        payload[511] = 0x33;
        fs::write(&save, payload).unwrap();

        assert_eq!(
            infer_supported_console_slug(&save, None).as_deref(),
            Some("n64")
        );
    }

    #[test]
    fn n64_non_blank_controller_pak_is_supported() {
        let tmp = tempfile::tempdir().unwrap();
        let save = tmp.path().join("MiSTer/N64/Mario Kart 64 (USA).mpk");
        fs::create_dir_all(save.parent().unwrap()).unwrap();
        let mut payload = vec![0u8; 32768];
        payload[0] = 0x5A;
        payload[4096] = 0x01;
        payload[32767] = 0xA5;
        fs::write(&save, payload).unwrap();

        assert_eq!(
            infer_supported_console_slug(&save, None).as_deref(),
            Some("n64")
        );
    }

    #[test]
    fn n64_non_blank_cpk_controller_pak_is_supported() {
        let tmp = tempfile::tempdir().unwrap();
        let save = tmp.path().join("MiSTer/N64/Mario Kart 64 (USA)_1.cpk");
        fs::create_dir_all(save.parent().unwrap()).unwrap();
        let mut payload = vec![0u8; 32768];
        payload[0] = 0x5A;
        payload[4096] = 0x01;
        payload[32767] = 0xA5;
        fs::write(&save, payload).unwrap();

        assert_eq!(
            infer_supported_console_slug(&save, None).as_deref(),
            Some("n64")
        );
    }

    #[test]
    fn n64_retroarch_combined_srm_is_supported() {
        let tmp = tempfile::tempdir().unwrap();
        let save = tmp
            .path()
            .join("Emulation/saves/n64/Super Mario 64 (USA).srm");
        fs::create_dir_all(save.parent().unwrap()).unwrap();
        let mut payload = vec![0u8; N64_RETROARCH_SRM_SIZE as usize];
        payload[11] = 0x01;
        payload[12] = 0x20;
        payload[0x20800] = 0x42;
        fs::write(&save, payload).unwrap();

        assert_eq!(
            infer_supported_console_slug(&save, None).as_deref(),
            Some("n64")
        );
    }

    #[test]
    fn wii_data_bin_is_supported_with_title_code_from_parent_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let save = tmp.path().join("Super Mario Galaxy 2/SB4P/data.bin");
        fs::create_dir_all(save.parent().unwrap()).unwrap();
        fs::write(&save, wii_data_bin_fixture()).unwrap();

        let classification = classify_supported_save(&save, None).unwrap();
        assert_eq!(classification.system_slug, "wii");
        assert!(classification.evidence.contains("titleCode=SB4P"));
        assert_eq!(wii_title_code_from_path(&save).as_deref(), Some("SB4P"));
    }

    #[test]
    fn invalid_wii_data_bin_is_rejected_even_with_wii_path_hint() {
        let tmp = tempfile::tempdir().unwrap();
        let save = tmp.path().join("private/wii/title/SB4P/data.bin");
        fs::create_dir_all(save.parent().unwrap()).unwrap();
        fs::write(&save, vec![0x44; 4096]).unwrap();

        assert!(classify_supported_save(&save, None).is_none());
        assert_eq!(
            wii_skip_reason(&save).as_deref(),
            Some("skip_invalid_wii_data_bin")
        );
    }

    #[test]
    fn neogeo_mister_backup_ram_size_is_supported_when_non_blank() {
        let tmp = tempfile::tempdir().unwrap();
        let save = tmp.path().join("MiSTer/NEOGEO/mslug5.sav");
        fs::create_dir_all(save.parent().unwrap()).unwrap();
        let mut payload = vec![0u8; 0x12000];
        payload[16..32].copy_from_slice(b"ABKCPUR MAO  K\x80!");
        fs::write(&save, payload).unwrap();

        assert_eq!(
            infer_supported_console_slug(&save, None).as_deref(),
            Some("neogeo")
        );
    }

    #[test]
    fn neogeo_blank_backup_ram_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let save = tmp.path().join("MiSTer/NEOGEO/doubledr.sav");
        fs::create_dir_all(save.parent().unwrap()).unwrap();
        fs::write(&save, vec![0xFFu8; 0x12000]).unwrap();

        assert!(infer_supported_console_slug(&save, None).is_none());
    }

    #[test]
    fn save_path_hint_wins_over_wrong_same_stem_rom_match() {
        let tmp = tempfile::tempdir().unwrap();
        let save = tmp.path().join("MiSTer/saves/Saturn/Quake (USA).sav");
        let rom = tmp.path().join("MiSTer/games/N64/Quake (USA).z64");
        fs::create_dir_all(save.parent().unwrap()).unwrap();
        fs::create_dir_all(rom.parent().unwrap()).unwrap();
        fs::write(&save, build_valid_saturn_internal_backup_ram()).unwrap();
        fs::write(&rom, [0x80, 0x37, 0x12, 0x40]).unwrap();

        assert_eq!(
            classify_supported_save(&save, Some(&rom))
                .map(|classification| classification.system_slug),
            Some("saturn".to_string())
        );
    }

    #[test]
    fn ps1_dexdrive_is_normalized_for_sync() {
        let tmp = tempfile::tempdir().unwrap();
        let save = tmp.path().join("Emulation/saves/duckstation/card.gme");
        fs::create_dir_all(save.parent().unwrap()).unwrap();
        let raw = build_valid_ps1_memcard();
        fs::write(&save, encode_ps1_dexdrive(&raw)).unwrap();

        let normalized = normalize_save_for_sync(&save, "psx")
            .unwrap()
            .expect("expected normalized save");
        assert_eq!(normalized.local_container, SaveContainerFormat::Ps1DexDrive);
        assert_eq!(normalized.adapter_profile, SaveAdapterProfile::Ps1DexDrive);
        assert_eq!(normalized.canonical_bytes, raw);
    }

    #[test]
    fn ps1_vmp_roundtrip_conversion_works() {
        let raw = build_valid_ps1_memcard();
        let encoded = encode_ps1_vmp(&raw).unwrap();
        let decoded = decode_ps1_vmp(&encoded).expect("expected valid vmp");
        assert_eq!(decoded, raw);

        let rewritten =
            encode_download_for_local_container(&raw, SaveContainerFormat::Ps1Vmp).unwrap();
        assert_eq!(rewritten, encoded);
    }

    #[test]
    fn non_ps1_saves_use_identity_adapter() {
        let tmp = tempfile::tempdir().unwrap();
        let save = tmp.path().join("Nintendo/mario.srm");
        fs::create_dir_all(save.parent().unwrap()).unwrap();
        let payload = vec![0x42u8; 8192];
        fs::write(&save, &payload).unwrap();

        let normalized = normalize_save_for_sync(&save, "snes")
            .unwrap()
            .expect("expected normalized save");
        assert_eq!(normalized.local_container, SaveContainerFormat::Native);
        assert_eq!(normalized.adapter_profile, SaveAdapterProfile::Identity);
        assert_eq!(normalized.canonical_bytes, payload);
    }

    fn wii_data_bin_fixture() -> Vec<u8> {
        let mut payload = vec![0x5A; 75_200];
        for (idx, value) in payload.iter_mut().enumerate() {
            *value = ((idx * 31 + 7) & 0xFF) as u8;
        }
        write_be_u32(&mut payload, WII_DATA_BIN_BACKUP_HEADER_OFFSET, 0x70);
        payload[WII_DATA_BIN_BACKUP_HEADER_OFFSET + 4..WII_DATA_BIN_BACKUP_HEADER_OFFSET + 8]
            .copy_from_slice(b"Bk\0\x01");
        write_be_u32(&mut payload, WII_DATA_BIN_BACKUP_HEADER_OFFSET + 0x0C, 1);
        write_be_u32(
            &mut payload,
            WII_DATA_BIN_BACKUP_HEADER_OFFSET + 0x10,
            0x3140,
        );
        write_be_u32(
            &mut payload,
            WII_DATA_BIN_FILE_HEADER_OFFSET,
            WII_DATA_BIN_FILE_HEADER_MAGIC,
        );
        write_be_u32(&mut payload, WII_DATA_BIN_FILE_HEADER_OFFSET + 4, 0x30A0);
        let name = b"GameData.bin";
        payload[WII_DATA_BIN_FILE_HEADER_OFFSET + 0x0B
            ..WII_DATA_BIN_FILE_HEADER_OFFSET + 0x0B + name.len()]
            .copy_from_slice(name);
        let cert = b"Root-CA00000001-MS00000002-NG02";
        let offset = payload.len() - 640;
        payload[offset..offset + cert.len()].copy_from_slice(cert);
        payload
    }

    fn write_be_u32(payload: &mut [u8], offset: usize, value: u32) {
        payload[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
    }
}
