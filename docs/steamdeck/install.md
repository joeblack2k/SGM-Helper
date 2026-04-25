# Steam Deck Install Guide (Phase 1)

## 1. Download

Download the latest Linux x86_64 release artifact: `sgm-steamdeck-helper-x86_64-unknown-linux-gnu.tar.gz`.

## 2. Extract

Extract files to a folder, for example:

- `/home/deck/SGM-Helper/sgm-steamdeck-helper`
- `/home/deck/SGM-Helper/config.ini`

By default, the helper looks for `config.ini` in the same directory as the executable.

When no custom source config exists, the helper auto-detects EmuDeck and prefers `.../Emulation/saves`.

## 3. Create config.ini

Example:

```ini
URL="192.168.1.1"
PORT="9096"
EMAIL="you@example.com"
APP_PASSWORD="your-app-password"
ROOT="/home/deck/.steam/steam/steamapps/compatdata"
STATE_DIR="./state"
WATCH="false"
WATCH_INTERVAL="30"
FORCE_UPLOAD="false"
DRY_RUN="false"

[source.steamdeck_emudeck]
LABEL="SteamDeck EmuDeck"
KIND="steamdeck"
SAVE_PATH="/home/deck/Emulation/saves"
ROM_PATH="/home/deck/Emulation/roms"
RECURSIVE="true"
MANAGED="false"
ORIGIN="manual"
```

## 4. Login and sync

```bash
./sgm-steamdeck-helper login --email you@example.com --app-password your-app-password
./sgm-steamdeck-helper sync
```

Known-path rescan:

```bash
./sgm-steamdeck-helper sync --scan
```

Deep scan (review only):

```bash
./sgm-steamdeck-helper sync --deep-scan
```

Deep scan and apply:

```bash
./sgm-steamdeck-helper sync --deep-scan --apply-scan
```

Supported family filter in sync:

- Nintendo
- Sega
- NeoGeo
- Sony (PS1/PS2/PSP/PS3/PS Vita/PS4/PS5)

The helper does not upload files blindly by `.sav` extension. It classifies candidates as real saves using console-specific extension/size rules plus binary payload checks.

For PS1 formats, sync uses canonical normalization:

- `.gme` and `.vmp` are validated and converted to canonical raw memory card bytes for hash/upload
- on download, saves are written back in the original local container format where supported
- PS1 uploads use `device_type=retroarch`; PS2 uploads use `device_type=pcsx2`
- slot is resolved to `Memory Card 1` or `Memory Card 2` from `--slot-name` or filename/path hints (`memory_card_1`, `Mcd001.ps2`), defaulting to `Memory Card 1`

Service mode (recommended for always-on sync):

```bash
./sgm-steamdeck-helper service install
./sgm-steamdeck-helper service status
```

The service sends backend heartbeat sensors, reacts to backend sync events, and still reconciles every 30 minutes by default.

Watch mode:

```bash
./sgm-steamdeck-helper watch --watch-interval 30
```

Scheduler fallback (every 30 min):

```bash
./sgm-steamdeck-helper schedule install --every-minutes 30
./sgm-steamdeck-helper schedule status
```

Manual PS1 container conversion:

```bash
./sgm-steamdeck-helper convert --input ./card.mcr --output ./card.gme --from raw --to gme
```
