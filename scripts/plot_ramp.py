#!/usr/bin/env python3
"""
plot_ramp.py — plot measured temperature vs time from a cryocon.sh CSV log.

READ-ONLY: opens the CSV for reading only, so it is safe to run while a
schedule is still writing to the same file (the plot is a snapshot of
whatever has been logged so far).

Usage:
    python3 plot_ramp.py cryocon_20260921_153951.csv [nominal_K_per_min]

Output: <csv-basename>_plot.png next to the CSV, plus a short stats summary.
"""

import csv
import datetime as dt
import os
import sys

import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt
from matplotlib.ticker import MultipleLocator

# palette (dataviz reference instance, light mode)
SURFACE = "#fcfcfb"
SERIES_1 = "#2a78d6"      # measured temperature
INK_PRIMARY = "#0b0b0b"
INK_SECONDARY = "#52514e"
INK_MUTED = "#898781"
GRIDLINE = "#e1e0d9"
BASELINE = "#c3c2b7"      # reference line / axis chrome (labeled in text)

TS_FMT = "%Y-%m-%d %H:%M:%S"


def load(path):
    """Return (times, temps, t0, target) where t0 is the ramp-start 'set' event."""
    times, temps = [], []
    t0, target = None, None
    with open(path, newline="") as f:
        rdr = csv.reader(f)
        next(rdr, None)  # header
        for row in rdr:
            if len(row) < 6 or not row[0].strip():
                continue
            try:
                t = dt.datetime.strptime(row[0][:19], TS_FMT)
                ta = float(row[4]) if row[4].strip() and row[4].strip() != "?" else None
            except ValueError:
                continue
            ev = row[1].strip()
            if t0 is None and ev.startswith("set loop1"):
                t0, target = t, float(row[2])
            if ta is not None:
                times.append(t)
                temps.append(ta)
    if not times:
        sys.exit("no temperature rows found in %s" % path)
    if t0 is None:
        t0 = times[0]
    return times, temps, t0, target


def main():
    csv_path = sys.argv[1]
    rate = float(sys.argv[2]) if len(sys.argv) > 2 else 3.333  # K/min nominal
    times, temps, t0, target = load(csv_path)
    if target is None:
        target = max(temps)

    elapsed = [(t - t0).total_seconds() / 60.0 for t in times]
    T0 = temps[0]
    nominal = [min(T0 + rate * e, target) for e in elapsed]
    dev = [m - n for m, n in zip(temps, nominal)]

    plt.rcParams.update({
        "font.family": ["Helvetica Neue", "Helvetica", "Arial", "sans-serif"],
        "figure.facecolor": SURFACE,
        "axes.facecolor": SURFACE,
        "savefig.facecolor": SURFACE,
    })
    fig, ax = plt.subplots(figsize=(9.5, 5.2), dpi=200)

    # recessive y-grid only; single bottom baseline
    ax.grid(axis="y", color=GRIDLINE, linewidth=0.8)
    ax.set_axisbelow(True)
    for side in ("top", "right", "left"):
        ax.spines[side].set_visible(False)
    ax.spines["bottom"].set_color(BASELINE)
    ax.tick_params(colors=INK_MUTED, labelsize=10, length=3)

    ax.plot(elapsed, nominal, linestyle=(0, (5, 4)), linewidth=1.4,
            color=BASELINE, label="nominal %.2f K/min" % rate, zorder=2)
    ax.plot(elapsed, temps, linewidth=2.0, color=SERIES_1,
            label="measured, channel A", zorder=3,
            solid_joinstyle="round")

    # direct labels at the right ends
    ax.annotate("%.1f K" % temps[-1], xy=(elapsed[-1], temps[-1]),
                xytext=(6, 0), textcoords="offset points",
                color=INK_PRIMARY, fontsize=10, fontweight="bold", va="center")
    ax.annotate("nominal", xy=(elapsed[-1], nominal[-1]),
                xytext=(6, 0), textcoords="offset points",
                color=INK_SECONDARY, fontsize=9, va="center")

    # annotate the point of largest lag vs nominal — but only for a genuine
    # mid-run feature; when the max lag is at/near the (live) end of the data
    # it goes into the subtitle instead, where it cannot collide with lines.
    i_dev = max(range(len(dev)), key=lambda i: abs(dev[i]))
    if abs(dev[i_dev]) > 1.5 and (elapsed[-1] - elapsed[i_dev]) >= 2.0:
        ax.annotate("largest lag vs nominal: %+.1f K" % dev[i_dev],
                    xy=(elapsed[i_dev], temps[i_dev]),
                    xytext=(-10, -46), textcoords="offset points",
                    fontsize=9, color=INK_SECONDARY, ha="right",
                    arrowprops=dict(arrowstyle="-", color=INK_MUTED,
                                    shrinkA=2, shrinkB=4))

    ax.set_xlabel("elapsed time since ramp start (min)", fontsize=11,
                  color=INK_SECONDARY)
    ax.set_ylabel("temperature (K)", fontsize=11, color=INK_SECONDARY)
    ax.yaxis.set_major_locator(MultipleLocator(25))
    duration = elapsed[-1]
    pad = max(1.5, duration * 0.03)
    ax.set_xlim(-pad * 0.4, duration + pad * 8)  # room for end labels
    lo, hi = min(min(temps), T0), max(max(temps), target)
    ax.set_ylim(lo - 6, hi + 6)

    live = "ramp in progress" if temps[-1] < target - 1.0 else "ramp complete"
    lag_note = "" if (abs(dev[i_dev]) > 1.5
                      and (elapsed[-1] - elapsed[i_dev]) >= 2.0) \
        else " · lag vs nominal %+.0f K" % dev[-1]
    ax.set_title("Cryostat ramp  %d → %d K  (channel A)" % (round(T0), round(target)),
                 fontsize=13, color=INK_PRIMARY, loc="left", pad=14)
    ax.text(0, 1.015,
            "started %s · data through %s · %s%s"
            % (t0.strftime("%H:%M:%S"), times[-1].strftime("%H:%M:%S"), live,
               lag_note),
            transform=ax.transAxes, fontsize=9.5, color=INK_SECONDARY)

    leg = ax.legend(loc="upper left", frameon=False, fontsize=10,
                    labelcolor=INK_SECONDARY)
    fig.tight_layout()

    out = os.path.splitext(csv_path)[0] + "_plot.png"
    fig.savefig(out, bbox_inches="tight")
    print("wrote", out)

    # stats
    mean_rate = (temps[-1] - temps[0]) / duration if duration > 0 else 0
    print("rows:            %d" % len(temps))
    print("window:          %s .. %s (%.1f min)"
          % (times[0].strftime("%H:%M:%S"), times[-1].strftime("%H:%M:%S"), duration))
    print("temperature:     %.2f -> %.2f K  (target %d K)" % (temps[0], temps[-1], target))
    print("mean rate:       %.2f K/min (nominal %.2f)" % (mean_rate, rate))
    print("lag vs nominal:  now %+.1f K, max %+.1f K at t=%.1f min"
          % (dev[-1], dev[i_dev], elapsed[i_dev]))


if __name__ == "__main__":
    main()
