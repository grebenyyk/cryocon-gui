#!/usr/bin/env python3
"""
Mock Cryo-con Model 22C controller for offline testing of cryocon.sh.

Emulates the two interfaces of the real instrument:
  1. The embedded web server (form pages + .cgi POST endpoints),
     based on the captured output.htm source (source_code.txt).
  2. The ASCII command interface on a raw TCP port (default 15000 here,
     5000 on the real box — 5000 is taken by AirPlay on macOS).

Usage:  python3 mock_cryocon.py [http_port [tcp_port]]
        (defaults: 8085 http, 15000 tcp)

The simulated temperature glides toward the setpoint with a ~3 s time
constant whenever control is ON, so the `stable` schedule command can be
tested quickly. State changes are printed to the console.
"""

import os
import re
import sys
import math
import time
import random
import socket
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs

HERE = os.path.dirname(os.path.abspath(__file__))

# ----------------------------------------------------------------------------
# Simulated instrument state
# ----------------------------------------------------------------------------

class State:
    def __init__(self):
        self.lock = threading.Lock()
        self.control = False
        self.channels = {"A": 295.0, "B": 295.0}   # Kelvin
        self.ambient = 295.0
        self.tau_on = 3.0     # s, heater-driven approach (below setpoint)
        self.tau_cool = 25.0  # s, passive LN2 cooling (above setpoint) —
                              #    slower than heating, like a real cryostat
        self.tau_off = 60.0   # s, drift back to ambient when stopped
        self.loops = {
            1: {"setpt": 298.0, "type": "PID",   "source": "CHA", "range": "HI",
                "p": 20.0, "i": 30.0, "d": 0.0, "pman": 5.0, "rate": 25.0},
            2: {"setpt": 250.0, "type": "Off",   "source": "CHB", "range": "LOW",
                "p": 1.0,  "i": 5.0,  "d": 0.0, "pman": 5.0, "rate": 25.0},
            3: {"setpt": 100.0, "type": "Off",   "source": "CHA", "range": "5V",
                "p": 1.0,  "i": 5.0,  "d": 0.0, "pman": 5.0, "rate": 25.0},
            4: {"setpt": 100.0, "type": "Off",   "source": "CHB", "range": "5V",
                "p": 1.0,  "i": 5.0,  "d": 0.0, "pman": 5.0, "rate": 25.0},
        }

    def _thermal_point(self, ch):
        """(target_K, tau_s) for a channel.

        The heater is single-sided: below the setpoint it drives the
        temperature up (tau_on); above the setpoint it idles and the
        cryostat cools passively toward the setpoint (tau_cool, LN2).
        With control off everything drifts to ambient.
        """
        with self.lock:
            t = self.channels[ch]
            if not self.control:
                return self.ambient, self.tau_off
            for n in (1, 2, 3, 4):
                lp = self.loops[n]
                if lp["type"] != "Off" and lp["source"] == "CH" + ch:
                    if t < lp["setpt"]:
                        return lp["setpt"], self.tau_on   # heating
                    return lp["setpt"], self.tau_cool     # passive cooling
            return self.ambient, self.tau_off

    def tick(self, dt):
        for ch in ("A", "B"):
            target, tau = self._thermal_point(ch)
            with self.lock:
                t = self.channels[ch]
                t += (target - t) * (dt / tau)
                t += random.uniform(-0.01, 0.01)
                self.channels[ch] = t

    def power_pct(self, loop):
        """Heater output, %. A heater cannot cool: above the setpoint it
        reads 0 and the LN2 does the work; near the setpoint it shows the
        hold power needed to balance the cold head (grows with T, like the
        real instrument's 60-70 % at room temperature)."""
        with self.lock:
            lp = self.loops[loop]
            if not self.control or lp["type"] == "Off":
                return 0
            src = lp["source"][-1]
            t = self.channels[src]
            err = lp["setpt"] - t
            # power needed to hold temperature against the cooling system
            hold = min(80.0, max(0.0, 0.4 * (t - 100.0)))
            if err > 0.05:                      # below setpoint: heat
                return min(100, int(hold + err * 8))
            if err < -0.05:                     # above setpoint: heater idle
                return 0
            return int(hold)                    # at setpoint: hold balance


STATE = State()

# ----------------------------------------------------------------------------
# Web server
# ----------------------------------------------------------------------------

def load_output_template():
    """Prefer the real captured page; fall back to a minimal stand-in."""
    for cand in (os.path.join(HERE, "source_code.txt"),
                 os.path.join(HERE, "output.htm")):
        if os.path.exists(cand):
            with open(cand, "r", errors="replace") as f:
                return f.read()
    return ("<html><body><p>mock: source_code.txt not found</p></body></html>")


OUTPUT_TEMPLATE = load_output_template()


def fmt_setpt(v):
    return "{:.3f}K".format(v)


def render_select(html, name, current):
    """Move the `selected` attribute in <select name="name"> to `current`."""
    pat = re.compile(r'(<select name="%s".*?</select>)' % re.escape(name),
                     re.S | re.I)

    def fix(m):
        block = m.group(1)
        block = re.sub(r'\s+selected', '', block)

        def opt(mm):
            tag, text = mm.groups()
            if text.strip() == current:
                tag += " selected"
            return "<option%s>%s</option>" % (tag, text)

        block = re.sub(r'<option([^>]*)>([^<]*)</option>', opt, block)
        return block

    return pat.sub(fix, html)


def render_output_page():
    with STATE.lock:
        html = OUTPUT_TEMPLATE
        html = html.replace("Status:&nbsp;OFF", "Status:&nbsp;ON" if STATE.control
                            else "Status:&nbsp;OFF")
        temps = [STATE.channels[STATE.loops[n]["source"][-1]] for n in (1, 2, 3, 4)]
        # Sequentially fill the four "Current Temperature" placeholders.
        parts = html.split(".......K")
        if len(parts) > 1:
            out = parts[0]
            for i, p in enumerate(parts[1:]):
                out += "{:.3f}K".format(temps[min(i, 3)]) + p
            html = out
        for n in (1, 2, 3, 4):
            html = re.sub(
                r'(name="Set%d" type="text" id="Set%d" value=")[^"]*(")' % (n, n),
                lambda m: m.group(1) + fmt_setpt(STATE.loops[n]["setpt"]) + m.group(2),
                html)
            html = render_select(html, "Type%d" % n, STATE.loops[n]["type"])
            html = render_select(html, "Source%d" % n, STATE.loops[n]["source"])
            html = render_select(html, "Range%d" % n, STATE.loops[n]["range"])
        return html


def render_index_page():
    with STATE.lock:
        a, b = STATE.channels["A"], STATE.channels["B"]
        return ("<!DOCTYPE html><html><head><title>Cryo-con Model 22C - Status"
                "</title></head><body><h1>Cryo-con Model 22C Cryogenic Temperature"
                " Controller</h1><p>Channel A: {:.3f}K</p><p>Channel B: {:.3f}K</p>"
                "<p>Control: {}</p></body></html>").format(
                    a, b, "ON" if STATE.control else "OFF")


def render_input_page():
    with STATE.lock:
        return ("<!DOCTYPE html><html><head><title>Cryo-con Model 22C - Inputs"
                "</title></head><body><p>Channel A: {:.3f}K Sensor: Si Diode</p>"
                "<p>Channel B: {:.3f}K Sensor: Si Diode</p></body></html>").format(
                    STATE.channels["A"], STATE.channels["B"])


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        print("[http] %s" % (fmt % args))

    # -- GET -------------------------------------------------------------
    def do_GET(self):
        path = self.path.split("?")[0].lower()
        if path in ("/", "/index.htm", "/status"):
            body = render_index_page().encode()
        elif path in ("/output.htm", "/outputs"):
            body = render_output_page().encode("iso-8859-1", "replace")
        elif path in ("/input.htm", "/inputs"):
            body = render_input_page().encode()
        elif path == "/source_code.txt":
            body = OUTPUT_TEMPLATE.encode("iso-8859-1", "replace")
        else:
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header("Content-Type", "text/html")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    # -- POST ------------------------------------------------------------
    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        body = self.rfile.read(length).decode("iso-8859-1", "replace")
        fields = {k: v[0] for k, v in parse_qs(body, keep_blank_values=True).items()}
        path = self.path.split("?")[0].lstrip("/").lower()

        if path == "outchannel.cgi":
            loop = fields.get("LoopID") or fields.get("LoopID2")
            try:
                n = int(loop)
            except (TypeError, ValueError):
                self.send_error(400, "no LoopID")
                return
            with STATE.lock:
                lp = STATE.loops.get(n)
                if lp is None:
                    self.send_error(400, "bad loop")
                    return
                def num(key):
                    v = fields.get(key, "")
                    return float(re.sub(r"[^0-9.eE+-]", "", v) or 0)
                if "Set%d" % n in fields:
                    lp["setpt"] = num("Set%d" % n)
                if "Type%d" % n in fields:
                    lp["type"] = fields["Type%d" % n].strip()
                if "Source%d" % n in fields:
                    lp["source"] = fields["Source%d" % n].strip()
                if "Range%d" % n in fields:
                    lp["range"] = fields["Range%d" % n].strip()
                for key, attr in (("P%d" % n, "p"), ("I%d" % n, "i"),
                                  ("D%d" % n, "d"), ("Pman%d" % n, "pman")):
                    if key in fields:
                        lp[attr] = num(key)
                print("[state] loop %d updated: %s" % (n, lp))
        elif path == "htrcontrol.cgi":
            with STATE.lock:
                if "Control" in fields:
                    STATE.control = True
                elif "Stop" in fields:
                    STATE.control = False
                print("[state] control -> %s" % STATE.control)
        elif path == "overtemp.cgi":
            print("[state] overtemp form accepted: %s" % fields)
        else:
            self.send_error(404)
            return

        self.send_response(302)
        self.send_header("Location", "/output.htm")
        self.send_header("Content-Length", "0")
        self.end_headers()


# ----------------------------------------------------------------------------
# TCP command interface
# ----------------------------------------------------------------------------

def handle_command(line):
    """Return response string (without CRLF), or None for no reply."""
    s = line.strip()
    if not s:
        return None
    if s.upper() == "*IDN?":
        return "CRYOCON,MODEL22C,204056,3.39F"
    if s.upper() == "*OPC?":
        return "1"

    # tokenize: ':' / ';' become spaces, '?' becomes its own token
    s2 = re.sub(r"[:;]", " ", s)
    s2 = re.sub(r"\?", " ? ", s2)
    tok = s2.split()
    up = [t.upper() for t in tok]
    q = "?" in up

    def val_after(key_up):
        """first non-'?' token following the keyword token"""
        try:
            i = up.index(key_up)
        except ValueError:
            return None
        for t in tok[i + 1:]:
            if t != "?":
                return t
        return None

    if up[0] == "INPUT":
        if q:
            ch = None
            for t in tok[1:]:
                if t != "?":
                    ch = t.upper()[-1]
                    break
            ch = ch or "A"
            if "UNITS" in up:
                return "K"
            if "NAME" in up:
                return "Channel %s" % ch
            if ch in STATE.channels:
                with STATE.lock:
                    return "{:.3f}K".format(STATE.channels[ch])
            return "0"
        return None

    if up[0] == "CONTROL":
        if q:
            return "ON" if STATE.control else "OFF"
        with STATE.lock:
            STATE.control = True
        print("[state] control -> ON")
        return None
    if up[0] == "STOP":
        with STATE.lock:
            STATE.control = False
        print("[state] control -> OFF")
        return None

    if up[0] == "SYSTEM" and len(up) > 1:
        if up[1].startswith("LOCK"):
            print("[state] system lock %s" % " ".join(t for t in up[2:] if t != "?"))
            return None
        if up[1].startswith("ERR"):
            return "0,NO ERROR"

    if up[0] == "LOOP":
        try:
            n = int(tok[1])
        except (IndexError, ValueError):
            return None
        lp = STATE.loops.get(n)
        if lp is None:
            return None
        rest = up[2:]
        key = next((k for k in rest if k not in ("?",)), "")
        arg = val_after(key) if key else None
        if key.startswith("SETP"):
            if q or arg is None:
                return fmt_setpt(lp["setpt"])
            try:
                lp["setpt"] = float(re.sub(r"[^0-9.eE+-]", "", arg) or 0)
                print("[state] loop %d setpoint -> %.3f" % (n, lp["setpt"]))
            except ValueError:
                pass
            return None
        if key.startswith("TYPE"):
            if q or arg is None:
                return lp["type"]
            lp["type"] = arg.upper()
            print("[state] loop %d type -> %s" % (n, lp["type"]))
            return None
        if key.startswith("SOURCE"):
            if q or arg is None:
                return lp["source"]
            lp["source"] = arg.upper()
            return None
        if key.startswith("RANGE"):
            if q or arg is None:
                return lp["range"]
            lp["range"] = arg.upper()
            return None
        if key.startswith("RATE"):
            if q or arg is None:
                return "{:.6f}".format(lp["rate"])
            try:
                lp["rate"] = float(re.sub(r"[^0-9.eE+-]", "", arg) or 0)
                print("[state] loop %d ramp rate -> %s K/min" % (n, lp["rate"]))
            except ValueError:
                pass
            return None
        if key.startswith("OUTPWR"):
            return "{:d}".format(STATE.power_pct(n))
        if key.startswith("MAXSET"):
            return "1000.000K"

    print("[tcp] unhandled: %r" % s)
    return None


def tcp_client(conn, addr):
    print("[tcp] connection from %s" % (addr,))
    conn.settimeout(30)
    buf = b""
    try:
        while True:
            data = conn.recv(1024)
            if not data:
                break
            buf += data
            while b"\n" in buf:
                line, buf = buf.split(b"\n", 1)
                resp = handle_command(line.decode("iso-8859-1", "replace"))
                if resp is not None:
                    conn.sendall((resp + "\r\n").encode())
    except (socket.timeout, OSError):
        pass
    finally:
        conn.close()
        print("[tcp] connection closed")


def tcp_server(port):
    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("127.0.0.1", port))
    srv.listen(5)
    print("tcp command interface on 127.0.0.1:%d" % port)
    while True:
        conn, addr = srv.accept()
        threading.Thread(target=tcp_client, args=(conn, addr), daemon=True).start()


# ----------------------------------------------------------------------------
# Main
# ----------------------------------------------------------------------------

def main():
    http_port = int(sys.argv[1]) if len(sys.argv) > 1 else 8085
    tcp_port = int(sys.argv[2]) if len(sys.argv) > 2 else 15000

    httpd = ThreadingHTTPServer(("127.0.0.1", http_port), Handler)
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    print("web interface on http://127.0.0.1:%d/output.htm" % http_port)

    threading.Thread(target=tcp_server, args=(tcp_port,), daemon=True).start()

    t0 = time.time()
    last = t0
    while True:
        time.sleep(0.25)
        now = time.time()
        STATE.tick(now - last)
        last = now


if __name__ == "__main__":
    main()
