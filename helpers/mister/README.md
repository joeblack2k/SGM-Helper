# sgm-mister-helper

MiSTer FPGA helper CLI for SGM self-hosted save sync.

## Commands

- `login --email <email> --app-password <password>`
- `logout`
- `token`
- `sync`
- `watch`
- `state list`
- `state clean`
- `config show`
- `device-auth` (phase 1 placeholder)

## Config

Default config location: `./config.ini` in the same directory as the binary.

Minimum required keys:

```ini
URL="192.168.1.1"
PORT="9096"
```

Full example: `config/config.ini.example`.
