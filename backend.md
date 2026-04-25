# Backend Config Sync Contract

This document defines how SGM helpers share their `config.ini` state with the backend and how backend-side device policy must be returned to helpers.

## Goal

Helpers must not be the only place where sync policy is configured. The backend Devices UI should become the place where a user can review and change helper/source settings, especially which console systems are synced per helper.

The helper sends a parsed, structured config snapshot. The backend returns an effective policy. The helper writes that policy back to `config.ini` and applies it at runtime before scanning, uploading, downloading, or restoring saves.

Do not treat `config.ini` as raw text in the backend. Store structured fields.

## Implemented Helper Behavior

As of helper `v0.4.14`:

- Helpers call `POST /helpers/config/sync` during `sync` and every `watch` sync cycle.
- Helpers in `service run` mode also call `POST /helpers/config/sync` during startup, backend-triggered sync, and periodic reconcile cycles.
- The request contains parsed global config, parsed source config, helper identity, and helper capabilities.
- The helper does not send the raw app password. It sends `appPasswordConfigured: true|false`.
- The endpoint is best-effort for backwards compatibility. If it is missing or fails, sync continues with local config.
- If the backend returns policy, the helper writes it to `config.ini` and applies it in memory for the current run.
- Before writeback, the helper creates a timestamped backup next to the config file, for example `config.ini.backend.20260425123000`.
- Backend source policy applies and writes back even when the local source has `MANAGED=false`.
- `MANAGED=false` means only: autoscan does not own that source. It no longer blocks backend writeback.

For the always-on service/heartbeat contract, see [`service.md`](service.md). Service mode adds `POST /helpers/heartbeat`, backend online sensors, and push events over `GET /events`.

## Endpoint

`POST /helpers/config/sync`

Authentication:

- Bearer token when the helper has one.
- App-password fallback header when configured: `X-RSM-App-Password`.
- If backend auth is disabled, accept unauthenticated LAN requests according to existing backend mode.

Recommended response codes:

- `200 OK` with JSON policy response.
- `204 No Content` means accepted with no policy changes.
- `401/403` normal auth failures.
- `422` schema validation failure.

## Request Shape

Example:

```json
{
  "schemaVersion": 1,
  "helper": {
    "name": "sgm-mister-helper",
    "version": "0.4.14",
    "deviceType": "mister",
    "defaultKind": "mister-fpga",
    "hostname": "MiSTer",
    "platform": "linux",
    "arch": "arm",
    "configPath": "/media/fat/1retro/config.ini",
    "binaryDir": "/media/fat/1retro"
  },
  "config": {
    "url": "192.168.2.10",
    "port": 80,
    "baseUrl": "http://192.168.2.10:80",
    "email": "",
    "appPasswordConfigured": true,
    "root": "/media/fat",
    "stateDir": "./state",
    "watch": false,
    "watchInterval": 30,
    "forceUpload": false,
    "dryRun": false,
    "routePrefix": "",
    "sources": [
      {
        "id": "mister_default",
        "label": "MiSTer Default",
        "kind": "mister-fpga",
        "profile": "mister",
        "savePaths": ["/media/fat/saves"],
        "romPaths": ["/media/fat/games"],
        "recursive": true,
        "systems": ["nes", "snes", "n64", "genesis", "saturn", "psx"],
        "createMissingSystemDirs": false,
        "managed": false,
        "origin": "manual"
      }
    ]
  },
  "capabilities": {
    "sourceKinds": [
      {
        "kind": "mister-fpga",
        "deviceType": "mister",
        "defaultProfile": "mister",
        "defaultSystems": ["nes", "snes", "gameboy", "gba", "n64", "genesis", "master-system", "game-gear", "sega-cd", "sega-32x", "saturn", "neogeo", "psx"]
      }
    ],
    "profiles": ["mister", "retroarch", "snes9x", "zsnes", "everdrive", "project64", "mupen-family", "generic"],
    "policy": {
      "supportsSystemsAllowList": true,
      "supportsCreateMissingSystemDirs": true,
      "supportsConfigWriteback": true,
      "manualManagedPolicy": "MANAGED indicates autoscan ownership only; backend policy can still write config.ini."
    },
    "service": {
      "supportsDaemonMode": true,
      "heartbeatEndpoint": "POST /helpers/heartbeat",
      "controlChannel": "GET /events",
      "controlEvents": ["sync.requested", "scan.requested", "deep_scan.requested", "config.changed", "save.changed"]
    }
  }
}
```

## Global Config Fields

These are all global keys currently represented from `config.ini`.

| Field | INI key | Type | Default | Meaning |
| --- | --- | --- | --- | --- |
| `url` | `URL` | string | `127.0.0.1` | Backend host/IP without scheme. |
| `port` | `PORT` | number | `3001` | Backend HTTP port. |
| `baseUrl` | derived | string | derived | `http://{URL}:{PORT}`. |
| `email` | `EMAIL` | string | empty | Optional login email. |
| `appPasswordConfigured` | `APP_PASSWORD` | boolean | false | Current helper has an app password configured; value is not sent. |
| `root` | `ROOT` | path string | platform default | Base scan root. |
| `stateDir` | `STATE_DIR` | path string | `./state` | State folder for auth, sync state, reports and lockfile. |
| `watch` | `WATCH` | boolean | false | Default watch mode preference. |
| `watchInterval` | `WATCH_INTERVAL` | number | `30` | Watch polling interval in seconds. |
| `forceUpload` | `FORCE_UPLOAD` | boolean | false | Upload local saves even when cloud differs. |
| `dryRun` | `DRY_RUN` | boolean | false | Simulate sync without writes. |
| `routePrefix` | `ROUTE_PREFIX` | string | empty | Optional API prefix such as `/v1`. |

Platform `ROOT` defaults:

| Helper | Default root |
| --- | --- |
| MiSTer | `/media/fat` |
| Steam Deck | `/home/deck/.steam/steam/steamapps/compatdata` |
| Windows | `./saves` |

## Source Config Fields

Source sections use `[source.<id>]` in local config.

| Field | INI key | Type | Default | Meaning |
| --- | --- | --- | --- | --- |
| `id` | section id | string | required | Stable source ID from `[source.<id>]`. |
| `label` | `LABEL` | string | source id | Human display label. |
| `kind` | `KIND` | enum | `custom` | Source runtime kind. |
| `profile` | `PROFILE` | enum | based on kind | Emulator/profile mapping used for conversion. |
| `savePaths` | `SAVE_PATH` | path array | required | Save root(s). Current INI stores one primary path. |
| `romPaths` | `ROM_PATH` | path array | save path | ROM root(s), used for ROM hashing/matching. |
| `recursive` | `RECURSIVE` | boolean | true | Scan source recursively. |
| `systems` | `SYSTEMS` | string array | based on kind | Console allow-list for this source. |
| `createMissingSystemDirs` | `CREATE_MISSING_SYSTEM_DIRS` | boolean | false | Whether cloud restore may create missing system folders. |
| `managed` | `MANAGED` | boolean | false | Whether autoscan owns this source in local config. |
| `origin` | `ORIGIN` | string | `manual` or scan origin | Metadata about how the source was created. |

## Source Kinds

Backend must store a capabilities matrix per `kind` + `profile`.

Known source kinds:

| Kind | Device type | Default profile | Typical systems |
| --- | --- | --- | --- |
| `mister-fpga` | `mister` | `mister` | MiSTer-supported FPGA systems only. |
| `retroarch` | `retroarch` | `retroarch` | Broad libretro-capable set. |
| `steamdeck` | `steamdeck` | `generic` | Broad set, often EmuDeck/RetroArch. |
| `windows` | `windows` | `generic` | Broad set. |
| `openemu` | `openemu` | `generic` | macOS emulator set. |
| `analogue-pocket` | `analogue-pocket` | `generic` | Cartridge/FPGA handheld set. |
| `custom` | `custom` | `generic` | User-defined. |

Known profiles:

- `mister`
- `retroarch`
- `snes9x`
- `zsnes`
- `everdrive`
- `project64`
- `mupen-family`
- `generic`

## Console/System Slugs

Supported structured slugs currently used by helpers:

- Nintendo: `nes`, `snes`, `gameboy`, `gba`, `n64`, `nds`, `wii`
- Sega: `genesis`, `master-system`, `game-gear`, `sega-cd`, `sega-32x`, `saturn`, `dreamcast`
- SNK: `neogeo`
- Sony: `psx`, `ps2`, `psp`, `psvita`, `ps3`, `ps4`, `ps5`

MiSTer default allow-list excludes systems that MiSTer cannot run, such as `wii`, `ps2`, `psp`, `ps3`, `ps4`, and `ps5`.

## Response Shape

The backend may return policy in any of these top-level containers. Helpers accept all three to make backend iteration easier:

- `policy`
- `desiredConfig`
- `effectiveConfig`

The helper also accepts top-level `global` and `sources` as shorthand.

Example response:

```json
{
  "accepted": true,
  "policy": {
    "global": {
      "forceUpload": false,
      "dryRun": false
    },
    "sources": [
      {
        "id": "mister_default",
        "systems": ["nes", "snes", "n64", "genesis", "saturn", "psx"],
        "createMissingSystemDirs": false
      },
      {
        "id": "steamdeck_emudeck",
        "systems": ["snes", "n64", "wii", "psx", "ps2"],
        "createMissingSystemDirs": false
      }
    ]
  }
}
```

## Response Global Policy

Applied immediately by helpers during `sync` and written back to `config.ini` for subsequent runs:

| Response field | Runtime effect |
| --- | --- |
| `url` | Writes `URL`; takes effect on the next helper invocation/control cycle. |
| `port` | Writes `PORT`; takes effect on the next helper invocation/control cycle. |
| `email` | Writes `EMAIL`. |
| `root` | Writes `ROOT`. |
| `stateDir` | Writes `STATE_DIR`. |
| `watch` | Writes `WATCH`. |
| `watchInterval` | Writes `WATCH_INTERVAL`. |
| `forceUpload` | Overrides local `FORCE_UPLOAD` for the current sync run. |
| `dryRun` | Overrides local `DRY_RUN` for the current sync run. |
| `routePrefix` | Writes `ROUTE_PREFIX`. |

The current HTTP client keeps using the already loaded base URL for the active request. Changes such as `URL`, `PORT`, `ROOT`, and `STATE_DIR` are persisted and become authoritative on the next run/reload.

## Response Source Policy

Applied immediately by helpers during `sync` and written back to `config.ini`:

| Response field | Runtime effect |
| --- | --- |
| `id` / `sourceId` | Preferred source match key. |
| `name` / `label` | Fallback source match key. |
| `enabled=false` | Clears `systems` and persists as `SYSTEMS="none"`. |
| `kind` | Overrides runtime source kind. |
| `profile` | Overrides runtime emulator profile/conversion target. |
| `savePath` / `savePaths` | Overrides runtime save roots. |
| `romPath` / `romPaths` | Overrides runtime ROM roots. |
| `recursive` | Overrides runtime recursive scan flag. |
| `systems` | Overrides runtime console allow-list. |
| `createMissingSystemDirs` | Overrides runtime cloud-restore folder creation policy. |
| `managed` | Writes `MANAGED`; use mostly for autoscan ownership metadata. |
| `origin` | Writes `ORIGIN`; recommended value for UI-created sources is `backend-ui`. |

Important: this policy is applied and written for every source, including `MANAGED=false` sources.

If the backend returns a source that does not exist locally, and it includes at least an `id`/`label` and `savePath`/`savePaths`, the helper creates a new `[source.<id>]` section. This supports backend UI flows like "Add console -> Super Nintendo -> Snes9x -> `/media/snes9x/saves`" before any save exists locally.

## MANAGED Semantics

`MANAGED` is autoscan ownership metadata only.

- `MANAGED=true`: helper/autoscan owns the source section and it may be refreshed by scan operations.
- `MANAGED=false`: autoscan will not replace it during `--scan`; this is appropriate for backend/UI/manual sources.
- Backend policy applies to both values.
- Backend policy writes back to both values.

This lets autoscan refresh detected sources while still allowing the backend UI to disable Wii on MiSTer, enable PSX on Steam Deck, add an empty Snes9x source, or change conversion profile for N64.

## Backend Storage Recommendation

Store at least:

- helper identity: `deviceType`, `hostname`, `helper name`, `helper version`, `platform`, `arch`
- last seen timestamp
- last config snapshot, structured by field
- one row/document per source id
- effective source policy per source id
- capabilities matrix version from latest helper snapshot

Recommended unique key:

- authenticated user/device id when available
- else app-password id
- else `deviceType + hostname + configPath` for auth-disabled LAN mode

## Backend UI Recommendation

Devices UI should show:

- helper online/last seen
- helper version and platform
- source list with `LABEL`, `KIND`, `PROFILE`, `SAVE_PATH`, `ROM_PATH`
- console toggles from `systems`
- warning when a selected console is not in the helper capability matrix
- `CREATE_MISSING_SYSTEM_DIRS` as an advanced toggle
- `MANAGED=false` badge: “not autoscan-managed; backend policy may write this config”

## Safety Rules

- Never blindly copy every backend save to every helper.
- Always intersect backend policy with helper capabilities.
- For MiSTer, do not enable Wii/GameCube/PS2/etc. unless the user explicitly overrides and understands it is not native MiSTer support.
- Do not expose `APP_PASSWORD` back to the UI from helper snapshots. Show only configured/not configured.
- Backend policy should be deterministic and idempotent. Repeated helper syncs must not flap source systems or cause upload/download loops.
