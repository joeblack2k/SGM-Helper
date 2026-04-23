# SGM-Helper

Open-source helper binaries for self-hosted retro save sync.

This repository is a multi-helper monorepo. Every helper keeps the same CLI contract, while
platform defaults differ for MiSTer, Steam Deck, and Windows.

## Repository Layout

- `helpers/mister` - MiSTer FPGA helper (`sgm-mister-helper`)
- `helpers/steamdeck` - Steam Deck helper (`sgm-steamdeck-helper`)
- `helpers/windows` - Windows helper (`sgm-windows-helper.exe`)
- `helpers/gamecube` - Swiss-startable GameCube helper with Wii Homebrew Launcher support (`sgm-gamecube-helper.dol`, `sgm-wii-helper.dol`)
- `helpers/anbernic` - reserved subfolder for future Anbernic helper
- `docs/mister` - MiSTer install + protocol notes
- `docs/steamdeck` - Steam Deck install + protocol notes
- `docs/windows` - Windows install + protocol notes
- `docs/gamecube` - GameCube install + protocol + backend worker notes

## Release Artifacts

- MiSTer ARMv7: `sgm-mister-helper-armv7.tar.gz`
- Steam Deck Linux x86_64: `sgm-steamdeck-helper-x86_64-unknown-linux-gnu.tar.gz`
- Windows x86_64: `sgm-windows-helper-x86_64-pc-windows-gnu.zip`
- GameCube DOL: `sgm-gamecube-helper.dol`
- Checksums: `sha256.txt`

## Supported Sync Model

The helpers sync through a canonical backend representation:

1. Local save candidates are scanned and validated.
2. Local emulator/container data is normalized to canonical raw bytes.
3. Backend stores canonical/raw bytes.
4. Download restores to the local format when possible.
5. Adapter metadata is persisted in `state/sync_state.json` for deterministic restore.

Validation is strict. Files are not accepted by extension alone. Scanner checks include:

- binary payload verification (rejects obvious text/junk)
- size/profile checks where known
- console/path hints
- Saturn backup RAM validation (real header + entry parsing, empty images skipped)
- PS1 container validation and conversion (`raw`, `.gme`, `.vmp`)
- PS2 memory card header validation
- Dreamcast VMU/DCI/VMS validation and metadata extraction (entries/icons/title/app)
- Dreamcast NVRAM blob rejection (`dc_nvmem.bin`)

Saturn helper policy:

- `saturn` classification requires structural backup RAM validation, not only path or extension hints
- accepted Saturn payloads must contain at least one active save entry
- empty backup RAM images are skipped and should not be uploaded
- supported Saturn backup RAM sizes are `32768`, `65536`, `524288`, `1048576`, `557056`, `1114112`, `4194304`, and `8388608` bytes
- helper diagnostics now emit explicit Saturn skip reasons such as `skip_empty_saturn_backup_ram`, `skip_invalid_saturn_backup_ram`, and `skip_saturn_without_structural_evidence(...)`

PlayStation helper contract:

- Helpers upload full memory-card images only (`.mcr/.mcd/.mc/.gme/.vmp/.psv` for PS1, `.ps2` for PS2).
- Helpers do not extract/merge logical saves client-side; backend parsing/projection is authoritative.
- PlayStation sync identity is runtime profile + slot, not ROM lookup.
- PS1 runtime `device_type`: `mister` (MiSTer profile) or `retroarch` (other PS1 profiles).
- PS2 runtime `device_type`: `pcsx2`.
- Slot is always resolved to `Memory Card 1` or `Memory Card 2` from `--slot-name` or filename/path hints (`memory_card_1`, `Mcd001.ps2`), with default `Memory Card 1`.
- For PlayStation lines helpers use deterministic backend keys: `ps-line:<system>:<device_type>:<slot>`.

Extension policy for cartridge-style systems (Nintendo/Sega/NeoGeo):

- Decision is now based on per-source `PROFILE` (not only `KIND`)
- `PROFILE="mister"` prefers `.sav`
- `PROFILE="retroarch"`, `PROFILE="snes9x"`, `PROFILE="zsnes"`, `PROFILE="everdrive"`, `PROFILE="generic"` prefer `.srm`
- If both variants exist for the same ROM stem, sync prioritizes the preferred extension per source profile
- N64 exception: `PROFILE="mister"` and `PROFILE="everdrive"` preserve/target native save types by size (`.eep`, `.sra`, `.fla`)

Supported console families in strict classification:

- Nintendo
- Sega (Genesis/Mega Drive, Master System, Game Gear, Mega-CD, 32X, Saturn, Dreamcast)
- NeoGeo
- Sony (PS1, PS2, PSP, PS3, PS Vita, PS4, PS5)

## Quick Start

1. Download the correct artifact for your platform.
2. Put binary and `config.ini` in the same folder.
3. Configure backend host and port.
4. Login once.
5. Run sync.

Example `config.ini` minimum:

```ini
URL="192.168.1.1"
PORT="9096"
```

Example first run:

```bash
./sgm-mister-helper login --email you@example.com --app-password your-app-password
./sgm-mister-helper sync
```

```bash
./sgm-steamdeck-helper login --email you@example.com --app-password your-app-password
./sgm-steamdeck-helper sync
```

```powershell
.\sgm-windows-helper.exe login --email you@example.com --app-password your-app-password
.\sgm-windows-helper.exe sync
```

Auto-enroll first run (no manual login):

1. Start backend UI and click `Add helper` (opens 15-minute enroll window).
2. Start helper with `sync` or `watch`.
3. Helper auto-detects gate status, self-registers, stores token in `STATE_DIR/auth.json`, and continues syncing.

GameCube/Wii quick flow:

1. Launch helper from Swiss (GameCube) or Homebrew Launcher (Wii).
2. Select discovered `Save Game Manager` server.
3. Enter 6-character device password from web UI.
4. Choose `Save per game` or `Restore from backend`.

## Config Contract (`config.ini`)

Default location is the same folder as the executable:

- `./config.ini`

Precedence order:

- CLI flags
- environment variables
- `config.ini`
- internal defaults

Environment variable aliases are supported as both `SGM_<KEY>` and `<KEY>`:

- `SGM_URL` or `URL`
- `SGM_PORT` or `PORT`
- `SGM_EMAIL` or `EMAIL`
- `SGM_APP_PASSWORD` or `APP_PASSWORD`
- `SGM_ROOT` or `ROOT`
- `SGM_STATE_DIR` or `STATE_DIR`
- `SGM_WATCH` or `WATCH`
- `SGM_WATCH_INTERVAL` or `WATCH_INTERVAL`
- `SGM_FORCE_UPLOAD` or `FORCE_UPLOAD`
- `SGM_DRY_RUN` or `DRY_RUN`
- `SGM_ROUTE_PREFIX` or `ROUTE_PREFIX`
- `ONE_RETRO_API_URL` (host:port override)
- `API_URL` (host:port override)

Global keys:

- `URL` required host/IP without schema
- `PORT` required backend port
- `EMAIL` optional default email
- `APP_PASSWORD` optional default app-password
- `ROOT` optional scan root (platform default differs)
- `STATE_DIR` optional state directory (default `./state`)
- `WATCH` optional bool default `false`
- `WATCH_INTERVAL` optional seconds default `30`
- `FORCE_UPLOAD` optional bool default `false`
- `DRY_RUN` optional bool default `false`
- `ROUTE_PREFIX` optional API prefix, for example `v1`

Platform default `ROOT` values:

- MiSTer: `/media/fat`
- Steam Deck: `/home/deck/.steam/steam/steamapps/compatdata`
- Windows: `./saves`

## Source Sections (`[source.<id>]`)

Sources are stored in `config.ini` as first-class config:

```ini
[source.super_nintendo]
LABEL="Super Nintendo"
KIND="retroarch"
PROFILE="snes9x"
SAVE_PATH="/home/snes9x/save"
ROM_PATH="/home/roms/snes"
RECURSIVE="true"
MANAGED="false"
ORIGIN="manual"
```

Source keys:

- `LABEL` display name
- `KIND` platform/source kind
- `PROFILE` emulator profile used for save-extension mapping (`mister`, `retroarch`, `snes9x`, `zsnes`, `everdrive`, `generic`)
- `SAVE_PATH` save directory
- `ROM_PATH` ROM directory (optional but recommended)
- `RECURSIVE` include nested directories
- `MANAGED` `true` for autoscan-managed entries
- `ORIGIN` metadata (`manual`, `scan`, `deep-scan`, `first-run`, ...)

Legacy migration:

- If no `[source.*]` exists but `state/sources.json` exists, helper migrates once to `config.ini`.
- Old file is renamed to `sources.migrated.<timestamp>.json`.

## Scan Modes

First-run behavior:

- If no source sections exist, `sync` and `watch` trigger known-path autoscan.

Command flags:

- `--scan` reruns known-path scan and replaces only `MANAGED=true` sources.
- `--deep-scan` scans broader disk locations and writes review output.
- `--apply-scan` applies deep-scan candidates to config (only valid with `--deep-scan`).

Deep-scan review report:

- `STATE_DIR/scan_report.json`

## Scheduler

Built-in schedule management:

- `schedule install --every-minutes <n>`
- `schedule status`
- `schedule uninstall`

Behavior:

- Linux uses `crontab` entries with a stable marker.
- Windows uses Task Scheduler (`schtasks`).
- Installed job executes `sync --quiet` with explicit `--config`.
- Sync overlap is prevented by lockfile `STATE_DIR/sync.lock`.

Note for Linux cron:

- `--every-minutes` must be between `1` and `59`.

## Command Reference (All Helpers)

The command set below is shared by MiSTer, Steam Deck, and Windows helpers.

Top-level:

- `signup`
- `login`
- `resend-verification`
- `logout`
- `token`
- `sync`
- `convert`
- `watch`
- `source`
- `state`
- `config`
- `schedule`
- `device-auth`

Global options:

- `--config <path>`
- `--url <host>`
- `--api-url <http://host:port>`
- `--port <u16>`
- `--email <email>`
- `--app-password <secret>`
- `--root <path>`
- `--state-dir <path>`
- `--route-prefix <prefix>`
- `--verbose`
- `--quiet`

### `signup`

Create account (when backend supports signup).

- `signup --email <email> --display-name <name>`
- Optional `--password <password>`
- Optional `--skip-verification`

### `login`

Supported login modes:

- `login --email <email> --app-password <password>`
- `login --email <email> --password <password>`
- `login --device`

### `resend-verification`

- `resend-verification --email <email>`

### `logout`

- `logout`

### `token`

- `token`
- `token --details`

### `sync`

Run one synchronization cycle.

- `sync`
- Optional `--force-upload[=true|false]`
- Optional `--dry-run[=true|false]`
- Optional `--scan`
- Optional `--deep-scan`
- Optional `--apply-scan`
- Optional `--slot-name <name>`

### `convert`

Manual conversion helper for PS1 save containers.

- `convert --input <path> --output <path> --from auto|raw|gme|vmp --to raw|gme|vmp`

### `watch`

Long-running sync loop with optional polling interval.

- `watch`
- Optional `--watch-interval <seconds>`
- Optional `--force-upload[=true|false]`
- Optional `--dry-run[=true|false]`
- Optional `--scan`
- Optional `--deep-scan`
- Optional `--apply-scan`
- Optional `--slot-name <name>`

### `source`

Source management commands:

- `source list`
- `source remove --name <source-id>`
- `source add custom --name <id> [--profile <mister|retroarch|snes9x|zsnes|everdrive|generic>] --saves <path>... [--roms <path>...] [--recursive[=true|false]]`
- `source add mister-fpga --name <id> [--profile <...>] --root <path> [--recursive[=true|false]]`
- `source add retroarch --name <id> [--profile <...>] --root <path> [--recursive[=true|false]]`
- `source add openemu --name <id> [--profile <...>] --root <path> [--recursive[=true|false]]`
- `source add analogue-pocket --name <id> [--profile <...>] --root <path> [--recursive[=true|false]]`

### `state`

State maintenance:

- `state list`
- `state clean --missing`
- `state clean --all`

### `config`

- `config show`

### `schedule`

- `schedule install --every-minutes <n>`
- `schedule status`
- `schedule uninstall`

### `device-auth`

- `device-auth --poll-interval <seconds>`

## End-to-End User Flow

Recommended onboarding flow for new users:

1. Download helper binary for platform.
2. Place `config.ini` next to the binary.
3. Set `URL` and `PORT`.
4. Run `login`.
5. Run `sync` once.
6. Inspect `source list`.
7. If needed run `sync --scan` to refresh known paths.
8. Optional: run `sync --deep-scan` and review `scan_report.json`.
9. Optional: apply deep results with `sync --deep-scan --apply-scan`.
10. Enable recurring sync with `schedule install --every-minutes 30`.

## Build From Source

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
```

Build helper binaries:

```bash
cargo build -p sgm-mister-helper --release
cargo build -p sgm-steamdeck-helper --release
cargo build -p sgm-windows-helper --release --target x86_64-pc-windows-gnu
```

## Per-Helper Guides

- MiSTer: `helpers/mister/README.md` and `docs/mister/install.md`
- Steam Deck: `helpers/steamdeck/README.md` and `docs/steamdeck/install.md`
- Windows: `helpers/windows/README.md` and `docs/windows/install.md`

## Roadmap

- Anbernic helper in `helpers/anbernic`
- Additional converter adapters
- Expanded emulator auto-discovery profiles
