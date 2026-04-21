# Windows Install Guide (Phase 1)

## 1. Download

Download the latest Windows release artifact: `sgm-windows-helper-x86_64-pc-windows-gnu.zip`.

## 2. Extract

Extract files to a folder, for example:

- `C:\\SGM-Helper\\sgm-windows-helper.exe`
- `C:\\SGM-Helper\\config.ini`

By default, the helper looks for `config.ini` in the same directory as the executable.

## 3. Create config.ini

Example:

```ini
URL="192.168.1.1"
PORT="9096"
EMAIL="you@example.com"
APP_PASSWORD="your-app-password"
ROOT="./saves"
STATE_DIR="./state"
WATCH="false"
WATCH_INTERVAL="30"
FORCE_UPLOAD="false"
DRY_RUN="false"
```

## 4. Login and sync

```powershell
.\sgm-windows-helper.exe login --email you@example.com --app-password your-app-password
.\sgm-windows-helper.exe sync
```

Watch mode:

```powershell
.\sgm-windows-helper.exe watch --watch-interval 30
```

Manual PS1 container conversion:

```powershell
.\sgm-windows-helper.exe convert --input .\card.mcr --output .\card.gme --from raw --to gme
```
