#!/usr/bin/env python3
"""
match_temps.py — assign temperatures to spectra by timestamp.

Combines two logs from the same experiment:
  1. a temperature log: either the Mac-side cryocon.sh CSV
     (timestamp,event,setpoint_loop1_K,...,input_A_K,input_B_K) or the
     Windows-side temp_logger.ps1 CSV (timestamp,input_A_K,input_B_K);
  2. a spectra manifest CSV (timestamp,name) — one row per spectrum,
     where 'timestamp' is when that spectrum was acquired (start or
     midpoint; just be consistent) on the spectrometer computer.

Temperature is linearly interpolated at each spectrum timestamp.

Clock offset between the two computers can be corrected with
--offset-seconds (added to the spectra timestamps; negative if the
spectrometer clock runs ahead of the temperature-logger clock).

Both files are assumed to be in the SAME timezone (check this once!).
Timestamp format: "YYYY-MM-DD HH:MM:SS" or ISO with 'T'.

Usage:
  python3 match_temps.py --temps cryocon_20260920.csv --spectra spectra.csv
  python3 match_temps.py --temps temp_log.csv --spectra spectra.csv \
      --offset-seconds -3 --out merged.csv
"""

import argparse
import csv
import datetime as dt
import sys

TS_FORMATS = ("%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M:%S")


def parse_ts(s):
    s = s.strip()
    for f in TS_FORMATS:
        try:
            return dt.datetime.strptime(s[:19], f)
        except ValueError:
            continue
    raise SystemExit("unrecognized timestamp: %r" % s)


def load_temps(path):
    """Return (times, tA, tB, sp1_or_None) sorted by time."""
    times, tA, tB, sp1 = [], [], [], []
    with open(path, newline="") as f:
        rdr = csv.reader(f)
        header = next(rdr, None)
        if header is None:
            raise SystemExit("temperature log is empty: %s" % path)
        h = [c.strip().lower() for c in header]
        # auto-detect the two supported layouts
        if h[:1] == ["timestamp"] and "input_a_k" in h:
            i_ts, i_a, i_b = 0, h.index("input_a_k"), h.index("input_b_k")
            i_sp = h.index("setpoint_loop1_k") if "setpoint_loop1_k" in h else None
        elif "time" in h or "timestamp" in h:
            i_ts = h.index("timestamp") if "timestamp" in h else h.index("time")
            # generic: assume ...,temperature columns; fall back to heuristic
            i_a = i_b = None
            for idx, c in enumerate(h):
                if "temp" in c and i_a is None:
                    i_a = idx
                elif "temp" in c and i_b is None:
                    i_b = idx
            if i_a is None:
                raise SystemExit("could not find temperature columns in %s" % path)
            i_sp = None
        else:
            # cryocon.sh CSV has no 'timestamp' header word match? it does.
            raise SystemExit("unrecognized temperature log header: %s" % header)

        n_bad = 0
        for row in rdr:
            if not row or not row[i_ts].strip():
                continue
            try:
                t = parse_ts(row[i_ts])
                a = float(row[i_a]) if i_a is not None and row[i_a].strip() else None
                b = float(row[i_b]) if i_b is not None and i_b < len(row) and row[i_b].strip() else None
                sp = float(row[i_sp]) if i_sp is not None and row[i_sp].strip() else None
            except (ValueError, IndexError):
                n_bad += 1
                continue
            times.append(t); tA.append(a); tB.append(b); sp1.append(sp)
    if n_bad:
        print("skipped %d unparseable temperature rows" % n_bad, file=sys.stderr)
    if len(times) < 2:
        raise SystemExit("need at least 2 temperature rows to interpolate")
    order = sorted(range(len(times)), key=lambda i: times[i])
    return ([times[i] for i in order], [tA[i] for i in order],
            [tB[i] for i in order], [sp1[i] for i in order])


def interp(times, values, t):
    """Linear interpolation; None-aware; clamps at the ends."""
    known = [(ti, v) for ti, v in zip(times, values) if v is not None]
    if not known:
        return None
    if t <= known[0][0]:
        return known[0][1]
    if t >= known[-1][0]:
        return known[-1][1]
    for (t0, v0), (t1, v1) in zip(known, known[1:]):
        if t0 <= t <= t1:
            if t1 == t0:
                return v0
            f = (t - t0).total_seconds() / (t1 - t0).total_seconds()
            return v0 + f * (v1 - v0)
    return None


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--temps", required=True, help="temperature CSV (cryocon.sh or temp_logger.ps1 format)")
    ap.add_argument("--spectra", required=True, help="spectra manifest CSV with columns: timestamp,name")
    ap.add_argument("--offset-seconds", type=float, default=0.0,
                    help="add to spectra timestamps (negative if spectrometer clock is ahead)")
    ap.add_argument("--out", default="merged_spectra_temps.csv")
    args = ap.parse_args()

    times, tA, tB, sp1 = load_temps(args.temps)
    print("temperature log: %d rows, %s .. %s" %
          (len(times), times[0], times[-1]))

    offset = dt.timedelta(seconds=args.offset_seconds)
    rows_out = []
    with open(args.spectra, newline="") as f:
        rdr = csv.DictReader(f)
        cols = [c.strip().lower() for c in (rdr.fieldnames or [])]
        ts_col = "timestamp" if "timestamp" in cols else ("time" if "time" in cols else None)
        name_col = "name" if "name" in cols else ("file" if "file" in cols else None)
        if ts_col is None:
            raise SystemExit("spectra CSV needs a 'timestamp' (or 'time') column")
        for row in rdr:
            t = parse_ts(row.get(ts_col, "")) + offset
            rows_out.append({
                "name": row.get(name_col, "") if name_col else "",
                "spectrum_timestamp": row.get(ts_col, ""),
                "T_A_K": interp(times, tA, t),
                "T_B_K": interp(times, tB, t),
                "setpoint_loop1_K": interp(times, sp1, t) if any(v is not None for v in sp1) else "",
            })

    with open(args.out, "w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=list(rows_out[0].keys()) if rows_out else
                           ["name", "spectrum_timestamp", "T_A_K", "T_B_K", "setpoint_loop1_K"])
        w.writeheader()
        w.writerows(rows_out)
    print("wrote %d rows -> %s" % (len(rows_out), args.out))


if __name__ == "__main__":
    main()
