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

[source.retroarch]
LABEL="RetroArch Windows"
KIND="retroarch"
SAVE_PATH="C:\\RetroArch\\saves"
ROM_PATH="C:\\RetroArch\\roms"
RECURSIVE="true"
MANAGED="false"
ORIGIN="manual"
```

## 4. Login and sync

```powershell
.\sgm-windows-helper.exe login --email you@example.com --app-password your-app-password
.\sgm-windows-helper.exe sync
```

Known-path rescan:

```powershell
.\sgm-windows-helper.exe sync --scan
```

Deep scan (review only):

```powershell
.\sgm-windows-helper.exe sync --deep-scan
```

Deep scan and apply:

```powershell
.\sgm-windows-helper.exe sync --deep-scan --apply-scan
```

Watch mode:

```powershell
.\sgm-windows-helper.exe watch --watch-interval 30
```

Scheduler install (every 30 min):

```powershell
.\sgm-windows-helper.exe schedule install --every-minutes 30
.\sgm-windows-helper.exe schedule status
```

Manual PS1 container conversion:

```powershell
.\sgm-windows-helper.exe convert --input .\card.mcr --output .\card.gme --from raw --to gme
```

PlayStation behavior:

- Helper always uploads full memory cards (no local entry extraction/merge).
- PS1 uploads use `device_type=retroarch`; PS2 uploads use `device_type=pcsx2`.
- Slot is resolved to `Memory Card 1` or `Memory Card 2` from `--slot-name` or filename/path hints (`memory_card_1`, `Mcd001.ps2`), defaulting to `Memory Card 1`.
