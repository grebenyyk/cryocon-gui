# cryocon-gui

A macOS GUI for the **Cryo-con**/**PHYSIKE** 22C cryogenic temperature
controller, speaking its ASCII command protocol over TCP (port 5000).

Features (v1):
- connect / auto-reconnect, live readout (temperature, setpoint, heater power)
- live charts (T + setpoint; heater power)
- schedule editor & runner with the same grammar as `cryocon.sh`
  (`control / stop / set / rate / wait / stable / log`)
- CSV logging compatible with the bash toolchain (`match_temps.py`,
  `plot_ramp.py`)

v1 developed and tested against `mock_cryocon.py`, an offline simulator of the
instrument.

## Companion scripts (`scripts/`)

The GUI grew out of a bash toolchain; both share the same schedule grammar
and the same CSV log format, so they stay interchangeable:

| script | purpose |
|---|---|
| `cryocon.sh` | headless schedule runner — the GUI's sibling; ideal for overnight runs (`./cryocon.sh schedule.txt`) |
| `mock_cryocon.py` | offline simulator of the 22C (web + command port) — run it, then connect the GUI to `127.0.0.1:15000` |
| `plot_ramp.py` | plot temperature vs time from any cryocon CSV log (safe on a file still being written) |
| `match_temps.py` | assign interpolated temperatures to spectra by timestamp (joins a cryocon log with a spectra manifest CSV) |

## First launch from the DMG

The app is not signed with an Apple Developer ID, so Gatekeeper will
complain on first open. This can be fixed by removing the quarantine flag in Terminal:

  ```sh
  xattr -dr com.apple.quarantine /Applications/cryocon-gui.app
  ```

## Build yourself

```sh
cargo run --release          # native target
./build-universal.sh         # universal (arm64 + x86_64) binary in dist/
./package-dmg.sh             # .app bundle in a DMG (dist/*-universal.dmg)
```

## References & acknowledgements

Command syntax and instrument facts came from three kinds of sources:

- **Cryo-con official documentation** — Model 22C brochure and User's Guide
  (control modes, ramp = rate + target setpoint, command scripts), and the
  AB015 *Remote Programming Guide* (the LOOP-command language, common to all
  Cryo-con instruments).
- **[bicarlsen/cryocon-22c-controller](https://github.com/bicarlsen/cryocon-22c-controller)**
  — an existing Python/easy-scpi driver for the 22C; its source was read to
  confirm the exact command spellings and the `\r\n` line protocol used here.
- **The instrument itself** — all commands used by this app were verified
  against a real 22C (firmware 3.39F); notably `LOOP n:RATE` (not in the
  manual's short list) was found empirically.
