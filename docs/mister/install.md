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
```

## 4. Login and sync

```bash
./sgm-mister-helper login --email you@example.com --app-password your-app-password
./sgm-mister-helper sync
```

Watch mode:

```bash
./sgm-mister-helper watch --watch-interval 30
```

Manual PS1 container conversion:

```bash
./sgm-mister-helper convert --input ./card.mcr --output ./card.gme --from raw --to gme
```
