# sgm-gamecube-helper

Swiss-startable GameCube helper with Wii Homebrew Launcher compatibility for Save Game Manager sync.

This helper is intentionally implemented as a standalone C + libogc module and does not join the Rust workspace.

## Build

Prerequisites:

- devkitPro
- devkitPPC
- libogc

Build DOL for GameCube (Swiss):

```bash
cd helpers/gamecube
make PLATFORM=gamecube
```

Output:

- `build/sgm-gamecube-helper.dol`

Build DOL for Wii (Homebrew Launcher):

```bash
cd helpers/gamecube
make PLATFORM=wii
```

Output:

- `build/sgm-wii-helper.dol`

Package for Homebrew Launcher (`boot.dol` + `meta.xml`):

```bash
make package-hbc
```

Package output:

- `build/hbc/sgm-gamecube-helper/boot.dol`
- `build/hbc/sgm-gamecube-helper/meta.xml`

Host simulation build:

```bash
make host
./build/sgm-gamecube-helper-host
```

Host tests:

```bash
make test-host
```

## Runtime Flow

1. `Looking for servers...`
2. Server list (`Save Game Manager`, IP + port)
3. `Enter device password` (`ABC123` or `ABC-123`)
4. Main menu:
   - `Save per game`
   - `Restore from backend`
5. Save path:
   - list local Memory Card games
   - upload selected game as GCI (`POST /saves`)
6. Restore path:
   - list backend GameCube games
   - select version
   - confirm overwrite if local match exists
   - restore selected version to Slot A

## Controls

- D-Pad or Wii Remote D-Pad: navigate
- `A`: select / confirm
- `B`: back / cancel (Wii Remote `2` also works as back)
- `X`: clear input or rescan depending on screen (Wii Remote `1` also works for rescan)
- `Start`: quit current screen (Wii Remote `HOME` also works)

## Password Storage

The device password is normalized to a 6-char compact value and stored encrypted on SD.

- Compact form: `ABC123`
- Display form: `ABC-123`
- Store path: `sd:/sgm-gamecube/device_password.dat`
