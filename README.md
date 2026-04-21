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

Minimum `config.ini`:

```ini
URL="192.168.1.1"
PORT="9096"
```

See `helpers/mister/config/config.ini.example` and `docs/mister/install.md` for full setup.
