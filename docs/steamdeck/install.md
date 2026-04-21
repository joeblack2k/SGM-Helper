# Steam Deck Install Guide (Phase 1)

## 1. Download

Download the latest Linux x86_64 release artifact: `sgm-steamdeck-helper-x86_64-unknown-linux-gnu.tar.gz`.

## 2. Extract

Extract files to a folder, for example:

- `/home/deck/SGM-Helper/sgm-steamdeck-helper`
- `/home/deck/SGM-Helper/config.ini`

By default, the helper looks for `config.ini` in the same directory as the executable.

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
```

## 4. Login and sync

```bash
./sgm-steamdeck-helper login --email you@example.com --app-password your-app-password
./sgm-steamdeck-helper sync
```

Watch mode:

```bash
./sgm-steamdeck-helper watch --watch-interval 30
```
