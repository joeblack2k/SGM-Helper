# Backend Config Sync Contract

This document defines how SGM helpers share their `config.ini` state with the backend and how backend-side device policy must be returned to helpers.

## Goal

Helpers must not be the only place where sync policy is configured. The backend Devices UI should become the place where a user can review and change helper/source settings, especially which console systems are synced per helper.

The helper sends a parsed, structured config snapshot. The backend returns an effective policy. The helper applies backend policy at runtime before scanning, uploading, downloading, or restoring saves.

Do not treat `config.ini` as raw text in the backend. Store structured fields.

## Implemented Helper Behavior

As of helper `v0.4.12`:

- Helpers call `POST /helpers/config/sync` during `sync` and every `watch` sync cycle.
- The request contains parsed global config, parsed source config, helper identity, and helper capabilities.
- The helper does not send the raw app password. It sends `appPasswordConfigured: true|false`.
- The endpoint is best-effort for backwards compatibility. If it is missing or fails, sync continues with local config.
- If the backend returns policy, the helper applies it in memory for the current run.
- Backend source policy applies even when the local source has `MANAGED=false`.
- `MANAGED=false` means only: do not write backend changes back into the local `config.ini` file for that source.

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
    "version": "0.4.12",
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
      "manualManagedPolicy": "MANAGED=false prevents config-file writeback only; backend policy still applies at runtime."
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

Currently applied immediately by helpers during `sync`:

| Response field | Runtime effect |
| --- | --- |
| `forceUpload` | Overrides local `FORCE_UPLOAD` for the current sync run. |
| `dryRun` | Overrides local `DRY_RUN` for the current sync run. |

Backend should still store all global fields so the Devices UI can manage them. Fields such as `URL`, `PORT`, `ROOT`, and `STATE_DIR` are safer as explicit write-back operations because changing them during an active request can disconnect the helper.

## Response Source Policy

Currently applied immediately by helpers during `sync`:

| Response field | Runtime effect |
| --- | --- |
| `id` / `sourceId` | Preferred source match key. |
| `name` / `label` | Fallback source match key. |
| `enabled=false` | Clears `systems`, causing the source to scan no systems. |
| `kind` | Overrides runtime source kind. |
| `profile` | Overrides runtime emulator profile/conversion target. |
| `savePath` / `savePaths` | Overrides runtime save roots. |
| `romPath` / `romPaths` | Overrides runtime ROM roots. |
| `recursive` | Overrides runtime recursive scan flag. |
| `systems` | Overrides runtime console allow-list. |
| `createMissingSystemDirs` | Overrides runtime cloud-restore folder creation policy. |

Important: this runtime policy is applied for every source, including `MANAGED=false` sources.

## MANAGED Semantics

`MANAGED` is local write-back metadata only.

- `MANAGED=true`: helper/autoscan owns the source section and it may be refreshed by scan operations.
- `MANAGED=false`: user manually owns the local source section.
- Backend policy must still apply at runtime to both values.
- Backend UI may edit policy for manual sources, but helper must not silently rewrite those manual source sections unless a future explicit write-back contract says so.

This lets a power user keep local paths hand-edited while still allowing backend UI to disable Wii on MiSTer, enable PSX on Steam Deck, or change conversion profile for N64.

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
- `MANAGED=false` badge: “manual local config; backend policy applies, local file is not rewritten”

## Safety Rules

- Never blindly copy every backend save to every helper.
- Always intersect backend policy with helper capabilities.
- For MiSTer, do not enable Wii/GameCube/PS2/etc. unless the user explicitly overrides and understands it is not native MiSTer support.
- Do not expose `APP_PASSWORD` back to the UI from helper snapshots. Show only configured/not configured.
- Backend policy should be deterministic and idempotent. Repeated helper syncs must not flap source systems or cause upload/download loops.
