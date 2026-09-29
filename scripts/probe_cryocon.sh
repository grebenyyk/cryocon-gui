#!/bin/bash
#
# probe_cryocon.sh — first-contact checklist for the Cryo-con Model 22C.
# Run this ON a computer connected to the instrument's LAN (192.168.1.x).
# Read-only except for the clearly marked optional step 6; it changes
# nothing on the controller. Results are printed and saved to ./probe_out/.
#
# Usage: ./probe_cryocon.sh [host]        (default 192.168.1.5)

set -u
HOST="${1:-192.168.1.5}"
TCP_PORT="${2:-5000}"
OUT=./probe_out
mkdir -p "$OUT"

ok()   { printf '  [ OK ] %s\n' "$*"; }
bad()  { printf '  [FAIL] %s\n' "$*"; }
info() { printf '  [i]    %s\n' "$*"; }

tcp_raw() {
    # NB: fd 3 must be opened in THIS shell, not a subshell, or it closes at once.
    local payload="$1" resp=""
    if exec 3<>/dev/tcp/"$HOST"/"$TCP_PORT" 2>/dev/null; then
        printf '%s\r\n' "$payload" >&3 2>/dev/null
        case "$payload" in
            *"?"*) IFS= read -t 4 -r resp <&3 || true ;;
        esac
        exec 3<&- 3>&- 2>/dev/null
        printf '%s' "$resp" | tr -d '\r'
        return 0
    fi
    return 1
}

echo "== 1. ping =="
if ping -c 1 -t 2 "$HOST" >/dev/null 2>&1; then ok "answers ping"; else bad "no ping reply (may still work — some devices block ICMP)"; fi

echo "== 2. web interface (port 80) =="
for page in index.htm input.htm output.htm relays.htm Sys.htm net.htm; do
    code=$(curl -s -m 5 -o "$OUT/$page" -w '%{http_code}' "http://$HOST/$page")
    if [ "$code" = 200 ]; then ok "GET /$page -> 200 (saved to $OUT/$page)"; else bad "GET /$page -> $code"; fi
done

echo "== 3. command interface (TCP $TCP_PORT) =="
if command -v nc >/dev/null 2>&1; then
    if nc -z -w 3 "$HOST" "$TCP_PORT" 2>/dev/null; then ok "port $TCP_PORT open"; else bad "port $TCP_PORT closed/filtered"; fi
else
    info "nc not installed; testing via /dev/tcp instead"
fi
idn=$(tcp_raw "*IDN?")
if [ -n "$idn" ]; then ok "*IDN? -> $idn"; else bad "no response to *IDN? on port $TCP_PORT"; fi

echo "== 4. read-only queries over the command port =="
for q in "INPUT? A" "INPUT? B" "LOOP 1:SETP?" "LOOP 1:TYPE?" "LOOP 1:SOURCE?" "LOOP 1:RANGE?" "LOOP 2:SETP?" "LOOP 2:TYPE?" \
         "CONTROL?" "LOOP 1:RAMP?" "OVERTEMP:ENABLE?" "OVERTEMP:TEMPERATURE?" "OVERTEMP:SOURCE?" \
         "SYSTEM:LINEFREQ?" "LOOP 1:MAXSET?" "LOOP 1:MAXPWR?" "SYSTEM:FWREV?"; do
    r=$(tcp_raw "$q")
    if [ -n "$r" ]; then ok "$q -> $r"; else bad "$q -> (no reply)"; fi
done

echo "== 5. web setpoint round-trip check (read-only) =="
rm -f "$OUT/output_before.htm"   # never parse a stale earlier run's file
curl -s -m 5 "http://$HOST/output.htm" -o "$OUT/output_before.htm"
sp1=$(sed -n 's/.*name="Set1"[^>]*value="\([^"]*\)".*/\1/p' "$OUT/output_before.htm" | head -1 | sed 's/[[:space:]]*$//')
[ -n "$sp1" ] && ok "web reports loop 1 setpoint: $sp1" || bad "could not parse setpoint from output.htm"

echo
echo "== 6. OPTIONAL live test (currently skipped) =="
info "When you are ready, verify a setpoint write by running e.g.:"
info "   ./cryocon.sh --host $HOST --once 'set 1 298'"
info "   ./cryocon.sh --host $HOST --once 'input A'"
info "and watching the Outputs page in the browser."
echo
echo "Done. If port 5000 answered *IDN?, use: ./cryocon.sh --host $HOST <schedule>"
echo "If only the web pages work, use: ./cryocon.sh --host $HOST --mode http <schedule>"
