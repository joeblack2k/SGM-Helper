# SGM-Helper

Open-source helper apps for self-hosted retro save sync.

This repo is built for homelab emulator users. You run a helper on each device (MiSTer, Steam Deck, Windows, and more), and each helper syncs local saves with your self-hosted backend.

## What You Need

1. A running Save Game Manager backend reachable on your LAN.
2. A device helper binary from [Releases](https://github.com/joeblack2k/SGM-Helper/releases).
3. A `config.ini` file next to the helper binary.
4. One auth method:
   - App password (`login --email --app-password`), or
   - Backend "Add helper" enroll flow.

If backend auth is disabled, you can usually run `sync` directly.

## Supported Helpers and Release Files

- MiSTer: `sgm-mister-helper-armv7.tar.gz`
- Anbernic/KNULLI/Batocera ARM64: `sgm-anbernic-helper-aarch64-unknown-linux-musl.tar.gz`
- Steam Deck: `sgm-steamdeck-helper-x86_64-unknown-linux-gnu.tar.gz`
- Windows: `sgm-windows-helper-x86_64-pc-windows-gnu.zip`
- GameCube/Wii: `sgm-gamecube-helper.dol`
- Nintendo 3DS: `sgm-3ds-helper.3dsx`
- Checksums: `sha256.txt`

## Minimal First Setup (MiSTer)

1. Copy `sgm-mister-helper` to your MiSTer (for example `/media/fat/`).
2. Create `/media/fat/config.ini`:

```ini
URL="192.168.2.10"
PORT="80"
```

3. First sync:

```bash
cd /media/fat
./sgm-mister-helper sync
```

4. If auth is enabled, do one of these first:

```bash
./sgm-mister-helper login --email you@example.com --app-password YOUR_APP_PASSWORD
./sgm-mister-helper sync
```

Or:

1. Open backend UI.
2. Click `Add helper`.
3. Run `./sgm-mister-helper sync` within 15 minutes.

### MiSTer Minimal Options Explained

- `URL`: backend IP or hostname, no `http://`.
- `PORT`: backend port.

That is enough for first run. The helper will auto-scan known save locations on first sync when no sources are configured.

## Minimal First Setup (Steam Deck / EmuDeck)

1. Copy `sgm-steamdeck-helper` to a folder, for example `/home/deck/SGM-Helper/`.
2. Create `/home/deck/SGM-Helper/config.ini`:

```ini
URL="192.168.2.10"
PORT="80"
```

3. First sync:

```bash
cd /home/deck/SGM-Helper
./sgm-steamdeck-helper sync
```

4. If needed, login once:

```bash
./sgm-steamdeck-helper login --email you@example.com --app-password YOUR_APP_PASSWORD
./sgm-steamdeck-helper sync
```

### Steam Deck Minimal Options Explained

- `URL`: backend IP or hostname, no `http://`.
- `PORT`: backend port.

On first run, the helper tries known paths (including common EmuDeck-style save locations), then stores sources in `config.ini`.

## First-Run Behavior and Save Discovery

When you run `sync` or `watch` for the first time and no `[source.*]` sections exist:

1. The helper runs a known-path scan.
2. Found sources are written into `config.ini` as `MANAGED="true"`.
3. Next runs use those stored sources.

Use these scan controls:

- `--scan`: rescan known emulator paths and refresh only `MANAGED="true"` sources.
- `--deep-scan`: broad scan, write candidates to `STATE_DIR/scan_report.json` (review only).
- `--deep-scan --apply-scan`: write deep-scan candidates into `config.ini`.

## `config.ini` Reference

Default path: same folder as the binary (`./config.ini`).

Precedence: `CLI flags > ENV > config.ini > defaults`.

### Global Keys

```ini
URL="192.168.2.10"
PORT="80"
EMAIL=""
APP_PASSWORD=""
ROOT="/media/fat"
STATE_DIR="./state"
WATCH="false"
WATCH_INTERVAL="30"
FORCE_UPLOAD="false"
DRY_RUN="false"
ROUTE_PREFIX=""
```

### Global Keys Explained

- `URL`: backend host/IP without scheme.
- `PORT`: backend port.
- `EMAIL`: optional default email for auth commands.
- `APP_PASSWORD`: optional default app password.
- `ROOT`: optional scan root.
- `STATE_DIR`: helper state folder (`auth.json`, sync state, lockfile).
- `WATCH`: default watch mode.
- `WATCH_INTERVAL`: polling interval in seconds for watch mode.
- `FORCE_UPLOAD`: force upload preference.
- `DRY_RUN`: dry-run preference.
- `ROUTE_PREFIX`: optional API prefix, for example `v1`.

### Platform Default `ROOT`

- MiSTer: `/media/fat`
- Steam Deck: `/home/deck/.steam/steam/steamapps/compatdata`
- Windows: `./saves`

### Source Sections

The helper stores save source mappings in `config.ini`:

```ini
[source.super_nintendo]
LABEL="Super Nintendo"
KIND="retroarch"
PROFILE="snes9x"
SAVE_PATH="/home/deck/Emulation/saves/snes"
ROM_PATH="/home/deck/Emulation/roms/snes"
RECURSIVE="true"
SYSTEMS="snes"
CREATE_MISSING_SYSTEM_DIRS="false"
MANAGED="false"
ORIGIN="manual"
```

Each key:

- `LABEL`: display name.
- `KIND`: runtime/source kind (`mister-fpga`, `retroarch`, `custom`, ...).
- `PROFILE`: emulator profile mapping (for extension behavior).
- `SAVE_PATH`: save folder path.
- `ROM_PATH`: ROM folder path (optional, recommended).
- `RECURSIVE`: include subfolders.
- `SYSTEMS`: comma-separated console allow-list for this source, for example `snes,n64,psx`.
- `CREATE_MISSING_SYSTEM_DIRS`: if `false`, cloud restore only writes into existing system folders.
- `MANAGED`: `true` if helper manages this source during scans.
- `ORIGIN`: metadata (`manual`, `scan`, `deep-scan`, `first-run`).

### Console Sync Policy

Helpers do not blindly download every save from the backend. Each source has a `SYSTEMS` allow-list.

- MiSTer defaults to FPGA-supported systems only: `nes,snes,gameboy,gba,n64,genesis,master-system,game-gear,sega-cd,sega-32x,saturn,neogeo,psx`.
- Steam Deck and Windows default to the broad helper list, including Wii and Sony systems where local emulator folders exist.
- `CREATE_MISSING_SYSTEM_DIRS="false"` prevents accidental folder creation, for example a MiSTer helper will not create `/media/fat/saves/Wii`.
- To opt in manually, add the console slug to `SYSTEMS` and create the target system folder yourself, or set `CREATE_MISSING_SYSTEM_DIRS="true"`.

## CLI Commands

The MiSTer, Steam Deck, and Windows helpers share the same command set.

- `signup`
- `login`
- `resend-verification`
- `logout`
- `token`
- `sync`
- `watch`
- `convert`
- `source list`
- `source add custom|mister-fpga|retroarch|openemu|analogue-pocket`
- `source remove --name <id>`
- `state list`
- `state clean --missing|--all`
- `config show`
- `schedule install|status|uninstall`
- `device-auth`

## CLI Flags (Global)

Use these before any command:

- `--config <path>`
- `--url <host>`
- `--api-url <host:port or url>`
- `--port <port>`
- `--email <email>`
- `--app-password <password>`
- `--root <path>`
- `--state-dir <path>`
- `--route-prefix <prefix>`
- `--verbose`
- `--quiet`

## CLI Flags (`sync` and `watch`)

- `--force-upload[=true|false]`
- `--dry-run[=true|false]`
- `--scan`
- `--deep-scan`
- `--apply-scan` (used with `--deep-scan`)
- `--slot-name <name>` (PlayStation slot hint)
- `watch` only: `--watch-interval <seconds>`

## How to Run Automatically (Recommended)

Use built-in scheduler commands. This is the easiest option.

### MiSTer

```bash
cd /media/fat
./sgm-mister-helper schedule install --every-minutes 30
./sgm-mister-helper schedule status
```

### Steam Deck

```bash
cd /home/deck/SGM-Helper
./sgm-steamdeck-helper schedule install --every-minutes 30
./sgm-steamdeck-helper schedule status
```

Remove schedule:

```bash
./sgm-mister-helper schedule uninstall
./sgm-steamdeck-helper schedule uninstall
```

Notes:

- Linux helpers use cron entries.
- Windows helper uses Task Scheduler.
- Scheduled runs execute `sync --quiet`.
- Overlapping runs are prevented via `STATE_DIR/sync.lock`.

## Optional: Continuous Watch as a Systemd Service (Linux)

If you prefer always-on watch mode instead of interval scheduler:

```ini
[Unit]
Description=SGM SteamDeck Helper Watch
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
WorkingDirectory=/home/deck/SGM-Helper
ExecStart=/home/deck/SGM-Helper/sgm-steamdeck-helper watch --watch-interval 30 --quiet
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
```

Then:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now sgm-steamdeck-helper.service
systemctl status sgm-steamdeck-helper.service
```

## Documentation

- MiSTer: [`docs/mister/install.md`](docs/mister/install.md)
- Steam Deck: [`docs/steamdeck/install.md`](docs/steamdeck/install.md)
- Windows: [`docs/windows/install.md`](docs/windows/install.md)
- GameCube: [`docs/gamecube/install.md`](docs/gamecube/install.md)
- 3DS: [`docs/3ds/install.md`](docs/3ds/install.md)
