# Changelog

All notable changes to this project are documented in this file.

## [Unreleased]

## [0.4.8] - 2026-04-24

### Fixed

- MiSTer cloud restore now prefers MiSTer-native directory casing (`N64`, `SNES`, `MegaDrive`, etc.) before generic aliases, keeping logs and sync-state aligned with the paths MiSTer users expect.
- GitHub Releases now publish the matching `CHANGELOG.md` section as release notes automatically.

## [0.4.7] - 2026-04-24

### Fixed

- RetroArch N64 combined `.srm` saves are now accepted by the strict scanner (`0x48800` bytes), so KNULLI/Batocera and Steam Deck N64 saves are no longer skipped as unsupported.
- Cloud restore no longer lets stale sync-state records block backend projections when the local file is missing or invalid.
- Existing valid local saves still win; only invalid/truncated placeholders are overwritten during restore.

## [0.4.6] - 2026-04-24

### Fixed

- Save path hints now win over stale/wrong same-stem ROM matches, fixing MiSTer Saturn saves like `Saturn/Quake (USA).sav` when an unrelated `N64/Quake` ROM also exists.
- Cloud restore now ignores invalid/truncated local placeholders instead of treating them as already restored, allowing backend projections to repair files such as short KNULLI `megadrive/*.srm` placeholders.
- Cloud restore target selection checks existing system-directory aliases before creating a new canonical directory, so KNULLI/Batocera `megadrive` paths are reused instead of duplicating into `genesis`.

## [0.4.5] - 2026-04-24

### Fixed

- MiSTer/SteamDeck/Windows scanner now accepts NeoGeo MiSTer backup RAM saves with the common `0x12000` byte layout.
- Blank/all-`0xFF` NeoGeo backup RAM files are still rejected, so empty frontend-created placeholders do not pollute the backend.

## [0.4.4] - 2026-04-24

### Added

- Cloud-aware restore pass for MiSTer, Steam Deck, and Windows helpers.
- Helpers now page through `GET /saves` after the local scan and restore backend-only saves into the configured source path.
- Runtime-profile downloads are requested automatically per source/emulator profile, for example `n64/mister`, `snes/snes9x`, `genesis/retroarch-genesis-plus-gx`, and `saturn/mister`.
- Release workflow now publishes an Anbernic/KNULLI ARM64 artifact: `sgm-anbernic-helper-aarch64-unknown-linux-musl.tar.gz`.

### Changed

- Restored saves are written in the local helper format instead of blindly preserving the backend filename extension.
- MiSTer cartridge restores now land as `.sav` even when the backend download profile exposes `.srm`.
- N64 controller-pak restores target MiSTer `.cpk`, RetroArch `.srm`, and other emulator `.mpk` projections based on the selected runtime profile.
- Existing local files remain local-first: the helper does not silently overwrite them during the cloud-only restore pass.

### Tests

- Added syncer tests for MiSTer SNES `.srm` to `.sav` restore targeting.
- Added syncer tests for N64 controller-pak `.cpk` restore targeting.
- Added syncer tests to keep single-system emulator roots, such as Snes9x, direct instead of appending another console directory.

## [0.4.3] - 2026-04-23

### Added

- Auto-enroll startup flow for all Rust helpers (MiSTer, Steam Deck, Windows):
- `sync` and `watch` now check `GET /auth/app-passwords/auto-enroll` when no local auth token exists.
- If the gate is active, helpers self-register via `POST /auth/token/app-password` using stable helper identity and runtime metadata.
- Provisioned token is saved to `STATE_DIR/auth.json` and reused in normal sync flow.
- New API client support for:
- auto-enroll status endpoint
- auto-provision token response parsing (`token` / `plainTextKey` / `plainTextToken`)

### Changed

- `sync` / `watch` no longer hard-fail immediately on missing token; they now attempt auto-enroll first and give explicit guidance to press `Add helper` when gate is closed.
- Helper metadata now reports current binary version `0.4.3` during auto-provision.

### Tests

- Added cross-helper contract tests for gate-open onboarding:
- `sync` succeeds without prior `login`
- helper requests auto-enroll status and auto-provision token endpoints
- token persistence is verified in local state

### Added

- Dreamcast scanner support across MiSTer, Steam Deck, and Windows helpers:
- accepts VMU images (`.bin`), VMS packages (`.vms`), and DCI dumps (`.dci`)
- strict VMU validation (root/FAT/directory chain checks) and NVRAM rejection (`dc_nvmem.bin`)
- metadata enrichment in detection evidence (container type, entry count, icon frame count, sample title/app)
- Dreamcast sync identity keys per slot/device (`dc-line:<system>:<device_type>:<slot>`) for deterministic cross-device sync.
- Sega expansion in strict scanner support:
- Saturn save detection (`.bkr/.sav/.srm/.ram`) with emulator/path hints (`saturn`, `yabasanshiro`, `beetle saturn`, etc.)
- Mega-CD and 32X detection with dedicated slugs (`sega-cd`, `sega-32x`)
- ROM extension mapping for `.32x` in ROM-index assisted classification.
- New Swiss-startable GameCube helper module at `helpers/gamecube` implemented in C + libogc.
- Wii Homebrew Launcher compatibility for the same helper module (`PLATFORM=wii` build and HBC package target).
- GameCube UI flow with server discovery, password prompt, `Save per game`, and `Restore from backend`.
- mDNS-first discovery with `/24` fallback scan and manual IP fallback path.
- Encrypted local device password storage for GameCube helper.
- Per-game Memory Card Slot A flow:
  - list local card saves
  - export selected save as GCI
  - upload selected save to backend
- Restore flow:
  - list GameCube saves from backend
  - select version first
  - require overwrite confirmation before import
- GameCube documentation set:
  - `docs/gamecube/install.md`
  - `docs/gamecube/protocol-notes.md`
  - `docs/gamecube/backend-worker-notes.md`
- Host-side unit tests for secure store and JSON/grouping logic in `helpers/gamecube/tests/host`.

### Changed

- Dreamcast slot detection now resolves from configured slot or path hints (`A1`..`D4`) with `A1` fallback.
- Cartridge-style extension policy now also covers `sega-cd` and `sega-32x` profiles (`.sav` on MiSTer profile, `.srm` on RetroArch-like profiles).
- Saturn keeps native extension by default (no forced restore rewrite), allowing `.bkr`-style workflows to remain stable.
- N64 extension preference now respects MiSTer-native save types by payload size:
- `.eep` for 512/2048-byte EEPROM saves
- `.sra` for 32KB SRAM saves
- `.fla` for 128KB FlashRAM saves
- `.sav` fallback for 786432-byte controller pak saves
- Non-MiSTer sources keep `.sav` preference for N64.
- Save extension preference is now profile-driven per source (`PROFILE`) instead of only source kind.
- Added emulator profiles (`mister`, `retroarch`, `snes9x`, `zsnes`, `everdrive`, `generic`) to source config and CLI `source add --profile`.
- PlayStation helper sync now follows backend card contract:
- full-card upload/download only (no client-side entry extraction/merge)
- runtime-based `device_type` mapping (`mister`/`retroarch` for PS1, `pcsx2` for PS2)
- slot resolution to `Memory Card 1/2` from config/path hints
- deterministic PS line keys (`ps-line:<system>:<device_type>:<slot>`) instead of ROM-lookup fallback

### Notes

- Backend must explicitly accept Dreamcast payloads; helper-side detection/validation is now in place, but unsupported backend validators return `422`.

## [0.3.1] - 2026-04-21

### Added

- Extension preference policy for cartridge-style systems (Nintendo/Sega/NeoGeo):
- MiSTer sources prefer `.sav`
- RetroArch/SteamDeck/Windows/Custom sources prefer `.srm`
- Duplicate save variants with the same ROM stem are now de-duplicated per source by preferred extension before sync.
- Download restore path update in sync flow:
- when cloud data is applied to an existing local save path, non-PSX native saves are written using the preferred extension for the source kind.

### Fixed

- Prevented duplicate stem variants (for example `.sav` + `.srm`) from racing each other during a single sync cycle.
- Added syncer tests for extension-preference and target-path selection behavior.

## [0.3.0] - 2026-04-21

### Added

- Steam Deck helper crate and release artifact (`sgm-steamdeck-helper-x86_64-unknown-linux-gnu.tar.gz`).
- Config-first source management with `[source.<id>]` blocks in `config.ini`.
- Source migration path from legacy `state/sources.json` to `config.ini` with timestamped backup.
- First-run known-path autoscan when no source sections exist.
- `--scan` support for managed-source refresh.
- `--deep-scan` review scan mode.
- `--deep-scan --apply-scan` apply mode.
- Scan review output at `STATE_DIR/scan_report.json`.
- Built-in scheduler command group with `install`, `status`, and `uninstall` actions.
- Sync lockfile protection with `STATE_DIR/sync.lock`.
- Adapter metadata persistence for deterministic restore of missing local files.
- `convert` command for PS1 container transforms (`raw`, `gme`, `vmp`).
- Device login and parity command set (`login --device`, `device-auth`, `signup`, `resend-verification`).

### Changed

- Canonical sync now treats backend payloads as canonical/raw and performs local format restoration where possible.
- CLI parity across MiSTer, Steam Deck, and Windows helpers.
- Documentation expanded with install guides, full root command reference, scan behavior, and scheduler usage.
- Release pipeline now builds and publishes MiSTer ARMv7 + Steam Deck Linux x86_64 + Windows x86_64 artifacts in one tag workflow.

### Validation And Compatibility

- Strict save validation expanded to avoid extension-only syncing.
- Classification and filtering support Nintendo, Sega, NeoGeo, and Sony console families.
- Sony-specific validation and conversion for PS1 raw memcards, PS1 DexDrive (`.gme`), PS1 PSP VMP (`.vmp`), and PS2 memory card superblocks.

### Fixed

- Restored missing local save files from canonical backend state using stored adapter metadata.
- Improved per-host auth/session behavior and command parity for login/token/logout flows.

## [0.2.0] - 2026-04-21

### Added

- Initial Windows helper port (`sgm-windows-helper`).
- Windows release artifact generation in GitHub Actions.
