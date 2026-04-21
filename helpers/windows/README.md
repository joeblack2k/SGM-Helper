# sgm-windows-helper

Windows helper CLI for SGM self-hosted save sync.

## Commands

- `login --email <email> --app-password <password>`
- `device-auth --poll-interval 5`
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
