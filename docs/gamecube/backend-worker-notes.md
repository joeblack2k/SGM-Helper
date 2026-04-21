# Backend Worker Notes (No App Code Changes)

This helper works with current backend routes. Optional infra improvements can make discovery better without changing server app code.

## Optional mDNS advertisement

Advertise your backend as `_http._tcp.local` from your host/network stack.

Examples:

- `avahi-daemon` service file on Linux
- Bonjour advertisement on macOS/Windows

Recommended metadata:

- Service name: `Save Game Manager`
- Port: backend HTTP port (`80`, `8080`, or `9096` depending on deployment)

## Health endpoint

Ensure `GET /healthz` is reachable from your GameCube LAN segment.

## Network policy

Allow GameCube helper traffic to:

- `GET /healthz`
- `GET /save/latest`
- `GET /saves`
- `GET /save`
- `GET /saves/download`
- `POST /saves`

## Password provisioning

Generate app password from web UI and provide to user as 6-char token.
