# GameCube + Wii Install Guide

## 1. Build or download the DOL

- Build from source:

```bash
cd helpers/gamecube
make PLATFORM=gamecube
```

- Output file: `helpers/gamecube/build/sgm-gamecube-helper.dol`

Wii build:

```bash
cd helpers/gamecube
make PLATFORM=wii
```

- Output file: `helpers/gamecube/build/sgm-wii-helper.dol`

Homebrew Launcher package:

```bash
cd helpers/gamecube
make package-hbc
```

- Package folder: `helpers/gamecube/build/hbc/sgm-gamecube-helper`
- Contains: `boot.dol`, `meta.xml`

## 2. Copy to SD

Place the DOL on a Swiss-accessible device, for example:

- `sd:/apps/sgm-gamecube-helper.dol`

For Wii Homebrew Launcher copy package files to:

- `sd:/apps/sgm-gamecube-helper/boot.dol`
- `sd:/apps/sgm-gamecube-helper/meta.xml`

## 3. Launch from Swiss

1. Boot Swiss.
2. Browse to your DOL file.
3. Start `sgm-gamecube-helper.dol`.

## 3b. Launch from Wii Homebrew Launcher

1. Insert SD card with `apps/sgm-gamecube-helper`.
2. Open Homebrew Channel.
3. Launch `SGM GameCube/Wii Helper`.

## 4. First run

1. The app scans for backend servers.
2. Select `Save Game Manager` server.
3. Enter the 6-char device password generated in the Save Game Manager web UI.
4. Choose `Save per game` or `Restore from backend`.

## 5. Notes

- v1 targets Memory Card Slot A.
- The app expects LAN HTTP access to existing SGM routes.
- If no server is found automatically, manual IP entry is available.
