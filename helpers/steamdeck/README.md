# sgm-steamdeck-helper

Steam Deck helper CLI for SGM self-hosted save sync.

## Commands

- `signup --email <email> --display-name <name>`
- `login --email <email> --app-password <password>`
- `login --email <email> --password <password>`
- `login --device` (or just `login` when no password is configured)
- `device-auth --poll-interval 5`
- `resend-verification --email <email>`
- `logout`
- `token`
- `sync`
- `watch`
- `source list`
- `source add ...`
- `source remove --name <name>`
- `state list`
- `state clean`
- `config show`

## Config

Default config location: `./config.ini` in the same directory as the binary.

Minimum required keys:

```ini
URL="192.168.1.1"
PORT="9096"
```

Full example: `config/config.ini.example`.

Default `ROOT` for SteamOS is set to:

- `/home/deck/.steam/steam/steamapps/compatdata`

For compatibility with existing 1Retro-style deployments you can also set:

- `ONE_RETRO_API_URL=http://host:port`
- `API_URL=http://host:port`
