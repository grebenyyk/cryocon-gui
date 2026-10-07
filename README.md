# <img src="assets/icon-1024.png" width="42" alt=""> cryocon-gui

A macOS GUI for the **Cryo-con**/**PHYSIKE** 22C cryogenic temperature
controller, speaking its ASCII command protocol over TCP (port 5000).

![cryocon-gui main window: live readout, temperature and heater-power
charts with crosshair readout, schedule editor, console](docs/screenshot.png)

Features:
- connect / auto-reconnect, live readout (temperature, setpoint, heater power)
- live charts (T + setpoint; heater power) — all panels drag-resizable
- schedule editor & runner with the same grammar as `cryocon.sh`
  (`control / stop / set / rate / wait / stable / log`)
- quick-ramp panels: "go to T in N minutes" (rate derived from the live
  temperature) and "go to T at R K/min"
- **live rate calibration**: this unit's firmware ramps at ~0.84× the
  commanded rate (measured 2026-09-30 at four commanded rates, heater
  never saturated). Before any run that heats at a commanded rate, the
  app offers to measure the scale with a short out-and-back leg (probe
  at 4–12 K/min, span-based slope fit, heater-saturation aware) and
  divides the planned rates by it, so ramps actually move at the
  requested K/min. It refuses to calibrate while the temperature is
  still drifting or control is off; cooling legs are never corrected.
- CSV logging compatible with the bash toolchain (`match_temps.py`,
  `plot_ramp.py`); schedule runs, quick ramps and calibrations all land
  in the same log
- keyboard: `Return` applies the manual setpoint and fires the quick-ramp
  panels, `⌘Return` runs the schedule from the editor; in every dialog
  `Return` accepts the primary action (confirm jump, calibrate & run, …)
  and `Esc` cancels
- one-click end-of-day warm-up: engages control, heats to room temperature
  at full heater power and holds there for sample removal (STOP ALL closes
  the day)

Developed and tested against `mock_cryocon.py`, an offline simulator of
the instrument — including its firmware's ~0.84× rate behavior.

## Companion scripts (`scripts/`)

The GUI grew out of a bash toolchain; both share the same schedule grammar
and the same CSV log format, so they stay interchangeable:

| script | purpose |
|---|---|
| `cryocon.sh` | headless schedule runner — the GUI's sibling; ideal for overnight runs (`./cryocon.sh schedule.txt`) |
| `mock_cryocon.py` | offline simulator of the 22C (web + command port, RampP ramp engine incl. the ~0.84× firmware quirk) — run it, then connect the GUI to `127.0.0.1:15000` |
| `plot_ramp.py` | plot temperature vs time from any cryocon CSV log (safe on a file still being written) |
| `match_temps.py` | assign interpolated temperatures to spectra by timestamp (joins a cryocon log with a spectra manifest CSV) |
| `probe_cryocon.sh` | read-only first-contact checklist: pages, command port, control/OTD/line-freq/max-setpoint queries (`./probe_cryocon.sh [host] [tcp-port]`) |
| `make_icon.swift` | regenerate the app icon (snowflake + temperature trace, vector CoreGraphics) → `assets/AppIcon.icns` (`swift scripts/make_icon.swift`) |

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

- **Cryo-con official documentation** (copies in [`docs/`](docs/)):
  the [Model 22C User's Guide](docs/model-22c-users-guide.pdf) (the full
  manual: front-panel menus, control types, temperature ramping, remote
  command summary), the [Model 22C brochure](docs/model-22c-brochure.pdf),
  and the
  [AB015 *Remote Programming Guide*](docs/ab015-remote-programming-guide.pdf)
  (the SCPI/LOOP-command language, common to all Cryo-con instruments).
- **[bicarlsen/cryocon-22c-controller](https://github.com/bicarlsen/cryocon-22c-controller)**
  — an existing Python/easy-scpi driver for the 22C; its source was read to
  confirm the exact command spellings and the `\r\n` line protocol used here.
- **The instrument itself** — all commands used by this app were verified
  against a real 22C (firmware 3.39F); notably `LOOP n:RATE` (not in the
  manual's short list) was found empirically.
