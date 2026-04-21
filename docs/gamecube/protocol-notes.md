# GameCube Helper Protocol Notes

No backend API changes are required for this helper.

Runtime targets:

- GameCube via Swiss
- Wii via Homebrew Launcher (for GameCube memory card workflows)

## Discovery

1. mDNS browse attempt on `_http._tcp.local`.
2. Candidate hosts are validated via `GET /healthz`.
3. Fallback `/24` scan probes common HTTP ports.
4. Manual IP fallback if discovery finds no valid server.

## Auth and Device Binding

The helper sends:

- `X-RSM-App-Password`
- `X-RSM-Device-Type: gamecube-swiss`
- `X-RSM-Fingerprint: <stable-device-fingerprint>`

Password validation flow uses existing helper-auth enforcement via:

- `GET /save/latest?romSha1=sgm-probe&slotName=card-a`

## Save per game

Upload route:

- `POST /saves` (multipart)

Multipart fields:

- `file`: GCI bytes
- `system`: `gamecube`
- `slotName`: `card-a`

Headers include helper auth markers listed above.

## Restore from backend

List backend saves:

- `GET /saves?limit=500&offset=0`

Load versions for selected game:

- `GET /save?saveId=<id>`

Download selected version:

- `GET /saves/download?id=<id>`

Restore always asks overwrite confirmation when a matching local save exists.
