# Changelog

All notable changes to this project are documented in this file.

## [Unreleased]

### Added

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
