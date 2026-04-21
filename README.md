# SGM-Helper

Open-source helper tooling for self-hosted retro save sync.

## Monorepo layout

- `helpers/mister` - MiSTer FPGA helper (phase 1)
- `helpers/windows` - Windows helper (phase 1)
- `helpers/steamdeck` - Steam Deck helper (phase 1)
- `helpers/anbernic` - planned
- `docs/mister` - MiSTer install and protocol notes
- `docs/windows` - Windows install and protocol notes
- `docs/steamdeck` - Steam Deck install and protocol notes

## Current scope

Phase 1 implements MiSTer, Windows, and Steam Deck helpers with:

- app-password login flow
- sync + watch workflows
- local state tracking
- `config.ini` driven backend endpoint settings
- canonical backend sync model (`local emulator format -> canonical raw -> local emulator format`)
- adapter metadata persistence for deterministic restore/conversion behavior

Minimum `config.ini`:

```ini
URL="192.168.1.1"
PORT="9096"
```

See `helpers/mister/config/config.ini.example` and `docs/mister/install.md` for full setup.

## Canonical Sync Flow

The helpers use a canonical backend-first model:

1. Local save files are scanned and strictly validated.
2. Helper converts local emulator/container format to canonical bytes for upload.
3. Backend stores canonical/raw save bytes.
4. On download, helper converts canonical bytes back to the local container format.
5. Adapter metadata is stored in `state/sync_state.json` so missing local files can be restored
   in the correct local format on the next sync.
