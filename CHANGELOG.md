# Changelog

All notable changes to this project are documented in this file.

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
