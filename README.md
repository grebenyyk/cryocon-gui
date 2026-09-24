# cryocon-gui

A small macOS GUI for the **Cryo-con Model 22C** cryogenic temperature
controller, speaking its ASCII command protocol over TCP (port 5000).

Features (v1):
- connect / auto-reconnect, live readout (temperature, setpoint, heater power)
- live charts (T + setpoint; heater power)
- schedule editor & runner with the same grammar as `cryocon.sh`
  (`control / stop / set / rate / wait / stable / log`)
- CSV logging compatible with the bash toolchain (`match_temps.py`,
  `plot_ramp.py`)
- safety rails: max-setpoint clamp, jump confirmation, dry-run, STOP ALL

Developed and tested against `mock_cryocon.py` (an offline simulator of the
instrument); works against the real controller unchanged.

## Build

```sh
cargo run --release          # native target
./build-universal.sh         # universal (arm64 + x86_64) binary in dist/
```

## Safety notes

- The Over-Temperature Disconnect on the reference instrument is configured
  at 300 K and *disabled* — the app clamps setpoints to 300 K by default.
- Channel B on the reference instrument has no sensor; it is shown as
  "no sensor" and never used as a stability criterion.
