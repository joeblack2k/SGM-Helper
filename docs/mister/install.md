# MiSTer Install Guide (Phase 1)

## 1. Download

Download the latest ARMv7 release artifact: `sgm-mister-helper-armv7.tar.gz`.

## 2. Copy to MiSTer

Copy the binary and config to your MiSTer SD card, for example:

- Binary: `/media/fat/sgm-mister-helper`
- Config: `/media/fat/config.ini`

By default, the helper looks for `config.ini` in the same directory as the binary.

## 3. Create config.ini

Example:

```ini
URL="192.168.1.1"
PORT="9096"
EMAIL="you@example.com"
APP_PASSWORD="your-app-password"
ROOT="/media/fat"
STATE_DIR="./state"
WATCH="false"
WATCH_INTERVAL="30"
FORCE_UPLOAD="false"
DRY_RUN="false"

[source.mister_default]
LABEL="MiSTer Default"
KIND="mister-fpga"
SAVE_PATH="/media/fat/saves"
ROM_PATH="/media/fat/games"
RECURSIVE="true"
MANAGED="false"
ORIGIN="manual"
```

## 4. Login and sync

```bash
./sgm-mister-helper login --email you@example.com --app-password your-app-password
./sgm-mister-helper sync
```

Known-path rescan:

```bash
./sgm-mister-helper sync --scan
```

Deep scan (review only):

```bash
./sgm-mister-helper sync --deep-scan
```

Deep scan and apply:

```bash
./sgm-mister-helper sync --deep-scan --apply-scan
```

Service mode (recommended for always-on sync):

```bash
./sgm-mister-helper service install
./sgm-mister-helper service status
```

The service sends backend heartbeat sensors, reacts to backend sync events, and still reconciles every 30 minutes by default.

Watch mode:

```bash
./sgm-mister-helper watch --watch-interval 30
```

Scheduler fallback (every 30 min):

```bash
./sgm-mister-helper schedule install --every-minutes 30
./sgm-mister-helper schedule status
```

Manual PS1 container conversion:

```bash
./sgm-mister-helper convert --input ./card.mcr --output ./card.gme --from raw --to gme
```

PlayStation behavior:

- Helper always uploads full memory cards (no local entry extraction/merge).
- PS1 uploads use `device_type=mister`; PS2 uploads use `device_type=pcsx2`.
- Slot is resolved to `Memory Card 1` or `Memory Card 2` from `--slot-name` or filename/path hints (`memory_card_1`, `Mcd001.ps2`), defaulting to `Memory Card 1`.
