# sgm-windows-helper

Windows helper CLI for SGM self-hosted save sync.

## Commands

- `signup --email <email> --display-name <name>`
- `login --email <email> --app-password <password>`
- `login --email <email> --password <password>`
- `login --device` (or just `login` when no password is configured)
- `device-auth --poll-interval 5`
- `resend-verification --email <email>`
- `logout`
- `token`
- `sync [--scan] [--deep-scan] [--apply-scan]`
- `convert --input <path> --output <path> --from auto|raw|gme|vmp --to raw|gme|vmp`
- `watch [--scan] [--deep-scan] [--apply-scan]`
- `source list`
- `source add ...`
- `source remove --name <name>`
- `state list`
- `state clean`
- `config show`
- `schedule install --every-minutes 30`
- `schedule status`
- `schedule uninstall`

## Config

Default config location: `./config.ini` in the same directory as the binary.

Minimum required keys:

```ini
URL="192.168.1.1"
PORT="9096"
```

Full example: `config/config.ini.example`.

`config.ini` source records live in `[source.<id>]` blocks:

```ini
[source.retroarch]
LABEL="RetroArch Windows"
KIND="retroarch"
SAVE_PATH="C:\\RetroArch\\saves"
ROM_PATH="C:\\RetroArch\\roms"
RECURSIVE="true"
MANAGED="false"
ORIGIN="manual"
```

Autoscan behavior:

- first `sync`/`watch` run writes managed source blocks when none exist
- `--scan` refreshes known emulator paths and replaces only `MANAGED=true` entries
- `--deep-scan` scans the full disk and writes `state/scan_report.json` (review mode)
- `--deep-scan --apply-scan` applies deep-scan candidates into managed source blocks

For compatibility with existing 1Retro-style deployments you can also set:

- `ONE_RETRO_API_URL=http://host:port`
- `API_URL=http://host:port`
