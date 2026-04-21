# Windows Helper Protocol Notes (Phase 1)

Implemented backend routes:

- `POST /auth/token/app-password`
- `GET /auth/me`
- `GET /save/latest`
- `POST /saves`
- `GET /saves/download`
- `GET /rom/lookup`
- `GET /conflicts/check`
- `POST /conflicts/report`
- `GET /events`

Route prefix is configurable via `ROUTE_PREFIX` (e.g. `"/v1"`), default is root routes.

Auth mode in phase 1: app-password only.
