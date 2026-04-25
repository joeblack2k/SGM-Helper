# SGM Helper Service Mode Contract

This document is the backend handoff for helper service/daemon mode.

Service mode is implemented in helper version `0.4.13` for:

- MiSTer helper
- Steam Deck helper
- Windows helper
- Anbernic/KNULLI ARM64 release artifact, currently built from the MiSTer-compatible helper crate

The goal is to move from "run a sync command every X minutes" to an always-on helper connection:

1. The helper starts as a background service.
2. The backend shows the helper as online with a green status indicator.
3. The helper sends structured health/config/sync sensors.
4. The backend can push control events such as "sync now", "scan now", and "config changed".
5. The helper remains backwards-compatible with older backends and still supports normal CLI `sync`, `watch`, and `schedule`.

## Current Implementation State

Implemented in helper `0.4.13`:

- New CLI command group: `service run|install|status|uninstall`.
- New daemon loop: `service run`.
- New backend heartbeat call: `POST /helpers/heartbeat`.
- Service loop listens to the existing SSE stream: `GET /events`.
- Service loop triggers sync immediately on supported control events.
- Service loop still performs a periodic reconcile sync every 30 minutes by default.
- Heartbeat payload includes logical backend sensors.
- Heartbeat is best-effort: if the backend does not support it yet, the helper keeps syncing.
- Existing config policy sync remains active through `POST /helpers/config/sync`.
- Backend policy still wins at runtime during sync cycles.
- Local `config.ini` is not silently rewritten by backend policy in this phase.

Not implemented yet:

- Native Windows tray icon.
- Full Windows Service API integration.
- Backend-to-helper local `config.ini` writeback.
- WebSocket transport.

The implemented path intentionally uses HTTP + SSE first because it fits the current backend shape and works well on MiSTer, Steam Deck, KNULLI/Batocera, and Windows.

## CLI Surface

### Run Daemon

```bash
./sgm-mister-helper service run
./sgm-steamdeck-helper service run
.\sgm-windows-helper.exe service run
```

Default behavior:

- Sends heartbeat every `30` seconds.
- Performs a reconcile sync every `1800` seconds.
- Connects to `GET /events` for push events.
- Runs an initial sync on startup.
- Uses the same sync lockfile as normal sync: `STATE_DIR/sync.lock`.

Useful flags:

```bash
./sgm-mister-helper service run --heartbeat-interval 30 --reconcile-interval 1800
./sgm-mister-helper service run --scan
./sgm-mister-helper service run --deep-scan
./sgm-mister-helper service run --slot-name "Memory Card 1"
./sgm-mister-helper service run --quiet
./sgm-mister-helper service run --verbose
```

### Install Background Service

```bash
./sgm-mister-helper service install
./sgm-steamdeck-helper service install
.\sgm-windows-helper.exe service install
```

Install defaults:

- Heartbeat interval: `30` seconds.
- Reconcile interval: `1800` seconds.
- Command installed by the helper:

```bash
"<binary>" --config "<config.ini>" service run --quiet --heartbeat-interval 30 --reconcile-interval 1800
```

### Service Status

```bash
./sgm-mister-helper service status
./sgm-steamdeck-helper service status
.\sgm-windows-helper.exe service status
```

### Uninstall

```bash
./sgm-mister-helper service uninstall
./sgm-steamdeck-helper service uninstall
.\sgm-windows-helper.exe service uninstall
```

## Platform Install Behavior

### Linux With Systemd

If `systemctl` is available:

- Non-root users get a user service in:

```text
~/.config/systemd/user/<normalized-service-name>.service
```

- Root users get a system service in:

```text
/etc/systemd/system/<normalized-service-name>.service
```

The helper runs:

```bash
systemctl --user daemon-reload
systemctl --user enable --now <unit>
```

or, for root:

```bash
systemctl daemon-reload
systemctl enable --now <unit>
```

### Linux Without Systemd

If `systemctl` is not available, the helper falls back to a marked `@reboot` cron entry:

```cron
@reboot "<binary>" --config "<config.ini>" service run --quiet --heartbeat-interval 30 --reconcile-interval 1800 # sgm-helper-service:<service-name>
```

This is important for small Linux appliances where full systemd is not present.

### Windows

Windows currently uses Task Scheduler:

```text
schtasks /Create /F /SC ONLOGON /TN "<service-name>" /TR "<service command>"
```

This is intentionally called "service task" in docs because it is not yet a native Windows Service or tray app.

## Backend Endpoint: Heartbeat

Helpers call:

```http
POST /helpers/heartbeat
Content-Type: application/json
Authorization: Bearer <token>
X-RSM-App-Password: <optional app password>
X-CSRF-Protection: 1
```

If the backend returns one of these statuses, helpers treat heartbeat as unsupported and continue:

- `404 Not Found`
- `405 Method Not Allowed`
- `501 Not Implemented`

Accepted success responses:

- `200 OK` with JSON
- `204 No Content`

Recommended backend response:

```json
{
  "accepted": true,
  "serverTime": "2026-04-25T12:00:00Z",
  "helperId": "optional-backend-helper-id"
}
```

The helper does not require the response body yet.

## Heartbeat Payload

Example:

```json
{
  "schemaVersion": 1,
  "helper": {
    "name": "sgm-mister-helper",
    "version": "0.4.13",
    "deviceType": "mister",
    "defaultKind": "mister-fpga",
    "hostname": "MiSTer",
    "platform": "linux",
    "arch": "arm",
    "pid": 1234,
    "startedAt": "2026-04-25T12:00:00Z",
    "uptimeSeconds": 61,
    "binaryPath": "/media/fat/1retro/sgm-mister-helper",
    "binaryDir": "/media/fat/1retro",
    "configPath": "/media/fat/1retro/config.ini",
    "stateDir": "/media/fat/1retro/state"
  },
  "service": {
    "mode": "daemon",
    "status": "idle",
    "loop": "sse-plus-periodic-reconcile",
    "heartbeatInterval": 30,
    "reconcileInterval": 1800,
    "controlChannel": "GET /events",
    "lastSyncStartedAt": "2026-04-25T12:00:01Z",
    "lastSyncFinishedAt": "2026-04-25T12:00:04Z",
    "lastSyncOk": true,
    "lastError": null,
    "lastEvent": "startup",
    "syncCycles": 1
  },
  "sensors": {
    "online": true,
    "authenticated": true,
    "configHash": "sha256-of-redacted-structured-config",
    "configReadable": true,
    "configError": null,
    "sourceCount": 2,
    "savePathCount": 2,
    "romPathCount": 2,
    "configuredSystems": ["n64", "psx", "snes"],
    "supportedSystems": ["nes", "snes", "gameboy", "gba", "n64", "genesis", "master-system", "game-gear", "sega-cd", "sega-32x", "saturn", "neogeo", "psx"],
    "syncLockPresent": false,
    "lastSync": {
      "scanned": 24,
      "uploaded": 1,
      "downloaded": 0,
      "inSync": 23,
      "conflicts": 0,
      "skipped": 0,
      "errors": 0
    }
  },
  "config": {
    "url": "192.168.2.10",
    "port": 80,
    "baseUrl": "http://192.168.2.10:80",
    "email": "",
    "appPasswordConfigured": false,
    "root": "/media/fat",
    "stateDir": "./state",
    "watch": false,
    "watchInterval": 30,
    "forceUpload": false,
    "dryRun": false,
    "routePrefix": "",
    "sources": [
      {
        "id": "default-mister",
        "label": "default-mister",
        "kind": "mister-fpga",
        "profile": "mister",
        "savePaths": ["/media/fat/saves"],
        "romPaths": ["/media/fat/games"],
        "recursive": true,
        "systems": ["nes", "snes", "n64", "psx"],
        "createMissingSystemDirs": false,
        "managed": false,
        "origin": "default"
      }
    ]
  },
  "capabilities": {
    "serviceRun": true,
    "serviceInstall": true,
    "heartbeatEndpoint": "POST /helpers/heartbeat",
    "configSyncEndpoint": "POST /helpers/config/sync",
    "controlEvents": [
      "sync.requested",
      "scan.requested",
      "deep_scan.requested",
      "config.changed",
      "save.changed",
      "save_created",
      "save_parsed",
      "save_deleted",
      "conflict_created",
      "conflict_resolved"
    ],
    "schedulerFallback": true,
    "backendPolicyWins": true
  }
}
```

Important privacy rule:

- `APP_PASSWORD` is never sent as plaintext.
- Heartbeat sends only `appPasswordConfigured: true|false`.
- `configHash` is calculated from the redacted structured config.

## Logical Backend Sensors

The backend should store and expose these as first-class helper sensors.

### Identity Sensors

- `helper.name`: crate/binary name.
- `helper.version`: helper release version.
- `helper.deviceType`: backend device type, for example `mister`, `steamdeck`, `windows`.
- `helper.defaultKind`: source kind, for example `mister-fpga`.
- `helper.hostname`: device hostname.
- `helper.platform`: OS from Rust runtime.
- `helper.arch`: CPU architecture from Rust runtime.
- `helper.binaryPath`: path to the running binary.
- `helper.configPath`: active config path.
- `helper.stateDir`: active state path.

### Connection Sensors

- `sensors.online`: helper says it is running.
- `service.status`: one of `starting`, `syncing`, `idle`, `backoff`, `stopping`.
- `service.uptimeSeconds`: under `helper.uptimeSeconds`.
- `service.lastError`: latest daemon/sync error.
- `service.lastEvent`: last event that triggered a sync.
- `service.syncCycles`: number of sync attempts in this daemon lifetime.
- Backend-side `lastSeenAt`: timestamp when heartbeat was received.
- Backend-side `offlineAt`: timestamp when helper is considered offline.

### Config Sensors

- `sensors.configHash`: hash of redacted structured config.
- `sensors.configReadable`: whether helper could parse source sections.
- `sensors.configError`: parse/read error if any.
- `sensors.sourceCount`: active resolved source count.
- `sensors.savePathCount`: count of save roots.
- `sensors.romPathCount`: count of ROM roots.
- `sensors.configuredSystems`: union of configured source `SYSTEMS`.
- `sensors.supportedSystems`: helper default support matrix for this device type.

### Sync Sensors

- `sensors.syncLockPresent`: true if `STATE_DIR/sync.lock` exists.
- `sensors.lastSync.scanned`: files scanned.
- `sensors.lastSync.uploaded`: files uploaded.
- `sensors.lastSync.downloaded`: files downloaded/restored.
- `sensors.lastSync.inSync`: files already matching backend.
- `sensors.lastSync.conflicts`: conflicts detected/reported.
- `sensors.lastSync.skipped`: files skipped by scanner/policy.
- `sensors.lastSync.errors`: sync errors.
- `service.lastSyncStartedAt`: last sync start time.
- `service.lastSyncFinishedAt`: last sync finish time.
- `service.lastSyncOk`: boolean.

## Backend Online/Offline Rules

Recommended backend policy:

- Mark helper online immediately after a valid heartbeat.
- Store `lastSeenAt = now()`.
- Store `heartbeatInterval` from the payload.
- Mark helper stale if no heartbeat after `max(heartbeatInterval * 3, 90 seconds)`.
- Mark helper offline if no heartbeat after `max(heartbeatInterval * 6, 180 seconds)`.
- If a `stopping` heartbeat is received, mark offline gracefully but keep last status.
- If `service.status == backoff`, keep online but show degraded/yellow.

Suggested UI colors:

- Green: heartbeat fresh and `lastSyncOk != false`.
- Yellow: heartbeat fresh but `status == backoff`, `lastSyncOk == false`, or `syncLockPresent == true` for too long.
- Red/gray: offline or stale.

## Control Channel

Helpers already connect to:

```http
GET /events
Accept: text/event-stream
Authorization: Bearer <token>
```

The existing backend event stream can be reused.

### Supported Event Names

The helper triggers an immediate sync for:

- `sync.requested`
- `helper.sync.requested`
- `config.changed`
- `helper.config.changed`
- `save.changed`
- `save_created`
- `save_parsed`
- `save_deleted`
- `conflict_created`
- `conflict_resolved`

The helper triggers known-path scan + sync for:

- `scan.requested`
- `helper.scan.requested`

The helper triggers deep scan + sync for:

- `deep_scan.requested`
- `deep-scan.requested`
- `helper.deep_scan.requested`

### JSON Data Fallback

If backend cannot easily use custom SSE event names, it can send JSON data with a generic event:

```text
event: helper.command
data: {"action":"sync"}
```

Supported `action` values:

- `sync`
- `scan`
- `deep_scan`
- `deep-scan`
- `config.changed`
- `reload_config`

## Config Management Flow

The current safe flow is runtime policy, not raw file writeback.

1. Helper sends full structured config in `POST /helpers/config/sync`.
2. Backend stores editable helper/source model.
3. Backend returns policy in the config-sync response.
4. Helper applies policy in memory for that sync run.
5. In service mode, backend emits `config.changed`.
6. Helper receives the event and runs sync immediately.
7. That sync calls `POST /helpers/config/sync` again and receives the latest policy.

This means backend changes become active without waiting for the next 30-minute reconcile.

### Why No Silent `config.ini` Writeback Yet

We deliberately do not silently rewrite local `config.ini` from backend policy in `0.4.13`.

Reason:

- `config.ini` is also a local escape hatch.
- Users may hand-edit it while debugging devices over SSH.
- Backend policy must win at runtime, but local writeback should be explicit and auditable.
- `MANAGED=false` already means "do not silently write backend changes back to local config.ini".

Recommended next backend/helper contract for true writeback:

- Backend stores a config revision number.
- Backend sends desired config with `revision`.
- Helper validates the desired config.
- Helper writes a backup: `config.ini.backup.<timestamp>`.
- Helper writes the new config atomically.
- Helper heartbeat reports `configHash` and `configRevision`.
- Backend only considers the write applied after matching heartbeat.

This is a separate phase because it needs rollback behavior.

## Existing Config Sync Endpoint

The existing endpoint remains:

```http
POST /helpers/config/sync
```

Backend should continue to support response shapes already accepted by helpers:

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
        "id": "auto-emudeck",
        "enabled": true,
        "kind": "steamdeck",
        "profile": "retroarch",
        "systems": ["snes", "n64", "psx", "wii"],
        "savePaths": ["/home/deck/Emulation/saves"],
        "romPaths": ["/home/deck/Emulation/roms"],
        "recursive": true,
        "createMissingSystemDirs": false
      }
    ]
  }
}
```

Aliases accepted by helpers:

- `policy`
- `desiredConfig`
- `desired_config`
- `effectiveConfig`
- `effective_config`

Source identity matching:

- Prefer `id` / `sourceId`.
- Fallback to `label` / `name`.

Backend policy applies even to local `MANAGED=false` sources.

## Backend Data Model Recommendation

Suggested tables/collections:

### `helpers`

- `id`
- `deviceType`
- `defaultKind`
- `hostname`
- `name`
- `version`
- `platform`
- `arch`
- `configPath`
- `stateDir`
- `binaryPath`
- `firstSeenAt`
- `lastSeenAt`
- `lastHeartbeatAt`
- `status`
- `online`
- `stale`
- `lastError`
- `configHash`
- `configRevision`

### `helper_sources`

- `helperId`
- `sourceId`
- `label`
- `kind`
- `profile`
- `savePaths`
- `romPaths`
- `recursive`
- `systems`
- `createMissingSystemDirs`
- `managed`
- `origin`
- `enabled`
- `backendPolicy`

### `helper_sensors`

Can be a JSON column or normalized metrics:

- `sourceCount`
- `savePathCount`
- `romPathCount`
- `configuredSystems`
- `supportedSystems`
- `syncLockPresent`
- `lastSync`
- `heartbeatInterval`
- `reconcileInterval`
- `uptimeSeconds`

## Backend UI Recommendation

Device list:

- Name/hostname.
- Device type.
- Version.
- Online dot.
- Last seen.
- Last sync summary.
- Config hash/revision.
- Quick actions:
  - Sync now
  - Scan known folders
  - Deep scan
  - Edit sources
  - Enable/disable systems

Device detail:

- Sources table.
- Config policy editor.
- Per-source console toggles.
- Per-source emulator profile dropdown.
- Last errors and skipped files.
- Last sync counters.
- Installed service status.

## Push Actions From Backend

### Sync Now

SSE:

```text
event: sync.requested
data: {"helperId":"...","reason":"user_clicked_sync_now"}
```

### Known Folder Scan

SSE:

```text
event: scan.requested
data: {"helperId":"...","reason":"user_clicked_scan"}
```

### Deep Scan

SSE:

```text
event: deep_scan.requested
data: {"helperId":"...","reason":"user_clicked_deep_scan"}
```

### Config Changed

SSE:

```text
event: config.changed
data: {"helperId":"...","revision":12}
```

The helper will run sync immediately, which pulls the latest policy through `/helpers/config/sync`.

## Security Notes

- Heartbeat must require the same auth trust level as save sync.
- Token auth and app-password auth should both work.
- Do not trust `helper.hostname` as identity by itself.
- Bind helper identity to token/app-password where possible.
- Treat heartbeat paths as informational; backend policy still decides which systems/sources are allowed.
- Never ask helper to upload unsupported systems solely because UI toggles were changed.
- Keep backend capabilities matrix per `KIND + PROFILE`.

## Compatibility Rules

Older backend:

- Missing `/helpers/heartbeat` is fine.
- Helper logs only in verbose mode and continues syncing.

Older helper:

- Backend should not require heartbeat for sync.
- Continue accepting normal `sync` clients.

Mixed mode:

- `schedule install` remains valid.
- `service install` is now preferred for devices that can keep a daemon alive.

## Acceptance Checklist For Backend

Backend support is complete when:

- `POST /helpers/heartbeat` stores helper status and sensors.
- UI shows helper online/offline based on heartbeat freshness.
- UI can emit `sync.requested` over `GET /events`.
- UI can emit `scan.requested` over `GET /events`.
- UI can emit `config.changed` after source/system/profile edits.
- `/helpers/config/sync` returns updated policy after UI edits.
- Helper detail page shows source paths, systems, profile, last sync counters, and last error.
- Backend does not store or display raw app passwords from helper payloads.
- Offline detection works after helper process is stopped.
