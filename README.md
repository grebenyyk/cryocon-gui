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
