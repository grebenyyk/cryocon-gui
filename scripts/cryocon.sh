#!/bin/bash
#
# cryocon.sh — schedule runner for the Cryo-con Model 22C temperature controller.
#
# Two transport modes:
#   tcp   (default) ASCII command interface on TCP port 5000
#         (the proper, documented way; shown on the controller's Net Cfg page)
#   http  imitates a user submitting the forms on the embedded web pages
#         (fallback; works even if only port 80 is reachable)
#
# Schedule file syntax (one command per line, '#' = comment):
#   control                  turn control loops ON
#   stop                     turn control loops OFF
#   set <loop> <value>       set setpoint of loop (1-4), Kelvin, e.g. "set 1 250"
#   set <loop> <value> <type>  ... optionally also set loop type: PID|RampP|RampT|Man|Off|Table
#   rate <loop> <K_per_min>  set the loop's ramp rate (tcp mode only; verified
#                            on firmware 3.39F as "LOOP n:RATE")
#   wait <seconds>           sleep
#   stable <loop> <tol> [timeout]  poll until the loop's sensor is within <tol> K
#                                  of its setpoint (timeout in s, default 3600)
#   log                      write one CSV line with current temperatures
#
# Quick single command, no schedule file:
#   ./cryocon.sh --once "set 1 250"
#   ./cryocon.sh --once "input A"      (read temperature)
#
# Test offline against the mock:
#   python3 mock_cryocon.py 8085 15000 &
#   ./cryocon.sh --host 127.0.0.1 --tcp-port 15000 --http-port 8085 schedule_example.txt
#
# Bash 3.2 compatible (stock macOS bash). No dependencies beyond curl;
# the tcp transport uses the built-in /dev/tcp, falling back to nc.

set -u

# ---------------------------------------------------------------- defaults --
HOST=192.168.1.5
MODE=tcp
TCP_PORT=5000
HTTP_PORT=80
POLL=2                  # s, poll interval for 'stable'
LOGFILE="cryocon_$(date '+%Y%m%d_%H%M%S').csv"
DRY_RUN=0
SCHEDULE_FILE=""

# loop -> source channel map (defaults taken from the captured output.htm;
# override at runtime if loops get rewired)
L1_SRC=A; L2_SRC=B; L3_SRC=A; L4_SRC=B

# ------------------------------------------------------------------ helpers --
now() { date '+%Y-%m-%d %H:%M:%S'; }

say()  { printf '[%s] %s\n' "$(now)" "$*"; }
err()  { printf '[%s] ERROR: %s\n' "$(now)" "$*" >&2; }

csv_init() {
    if [ ! -f "$LOGFILE" ]; then
        printf 'timestamp,event,setpoint_loop1_K,setpoint_loop2_K,input_A_K,input_B_K\n' \
            > "$LOGFILE"
    fi
}

csv_line() {  # csv_line <event> <sp1> <sp2> <tA> <tB>
    printf '%s,%s,%s,%s,%s,%s\n' "$(now)" "$1" "$2" "$3" "$4" "$5" >> "$LOGFILE"
}

read_all_temps() {  # sets TA / TB (or "?" if unreadable)
    TA=$(dev_temp A); TB=$(dev_temp B)
}

# ------------------------------------------------------------- tcp transport --
tcp_raw() {  # tcp_raw <payload-without-CRLF> -> prints response (may be empty)
    # NB: fd 3 must be opened in THIS shell, not a subshell, or it closes at once.
    local payload="$1" resp="" attempt
    for attempt in 1 2 3; do
        if { exec 3<>/dev/tcp/"$HOST"/"$TCP_PORT"; } 2>/dev/null; then
            printf '%s\r\n' "$payload" >&3 2>/dev/null
            # only queries (containing '?') get a reply; don't wait on set commands
            case "$payload" in
                *"?"*) IFS= read -t 5 -r resp <&3 || true ;;
            esac
            exec 3<&- 3>&- 2>/dev/null
            printf '%s' "$resp" | tr -d '\r'
            return 0
        fi
        # brief backoff before retrying the connection
        sleep 1
    done
    # fallback to nc (e.g. if /dev/tcp is unavailable)
    if command -v nc >/dev/null 2>&1; then
        local out
        out=$(printf '%s\r\n' "$payload" | nc -w 4 "$HOST" "$TCP_PORT" 2>/dev/null \
            | tr -d '\r')
        local rc=$?
        printf '%s' "$out"
        return $rc
    fi
    return 1
}

tcp_cmd() {  # tcp_cmd <command> [label]
    local cmd="$1" label="${2:-$1}" resp rc
    if [ "$DRY_RUN" -eq 1 ]; then say "DRY-RUN tcp: $cmd"; return 0; fi
    resp=$(tcp_raw "$cmd"); rc=$?
    if [ $rc -ne 0 ]; then
        err "tcp transport failed ($MODE -> $HOST:$TCP_PORT): $label"
        return 1
    fi
    if [ -n "$resp" ]; then say "tcp: $cmd -> $resp"; else say "tcp: $cmd"; fi
    return 0
}

tcp_query() {  # tcp_query <query> -> prints response
    if [ "$DRY_RUN" -eq 1 ]; then echo "?"; return 0; fi
    tcp_raw "$1" | tr -d '\r'
}

strip_k() { printf '%s' "$1" | tr -d 'Kk ' ; }

dev_temp() {  # dev_temp A|B -> Kelvin or "?"
    local ch; ch=$(printf '%s' "$1" | tr 'ab' 'AB')
    local v
    if [ "$MODE" = tcp ]; then
        v=$(tcp_query "INPUT? $ch")
        v=$(strip_k "$v" | tr -d ' ')
        # only accept a real number: "......." means no sensor on that channel
        if printf '%s' "$v" | grep -qE '^-?[0-9]+\.?[0-9]*([eE][-+]?[0-9]+)?$'; then
            echo "$v"
        else
            echo "?"
        fi
    else
        local page; page=$(http_get index.htm)
        if [ -z "$page" ]; then echo "?"; return; fi
        printf '%s' "$page" | sed -n "s/.*Channel $ch[: ]*\([0-9.]*\)K.*/\1/p" | head -1
    fi
}

dev_setpoint() {  # dev_setpoint <loop> -> current setpoint (tcp mode only)
    tcp_query "LOOP $1:SETP?" | tr -d '\r'
}

dev_set() {  # dev_set <loop> <value> [type]
    local loop="$1" val="$2" type="${3:-}" cur_type
    if [ "$MODE" = tcp ]; then
        if [ -n "$type" ]; then
            tcp_cmd "LOOP $loop:TYPE $type" || return 1
        fi
        tcp_cmd "LOOP $loop:SETP $val" || return 1
        # verify the write took (device answers NAK to bad commands)
        local rb okv
        rb=$(strip_k "$(dev_setpoint "$loop")" | tr -d ' ')
        if printf '%s' "$rb" | grep -qE '^-?[0-9]+\.?[0-9]*([eE][-+]?[0-9]+)?$'; then
            okv=$(awk -v a="$rb" -v b="$val" 'BEGIN { d = a - b; if (d < 0) d = -d; print (d < 0.001) ? 1 : 0 }')
            [ "$okv" = 1 ] || err "read-back mismatch on loop $loop: sent $val, device reports $rb"
        fi
    else
        http_set_loop "$loop" "$val" "$type"
    fi
    # remember setpoints for logging / 'stable'
    case "$loop" in
        1) SP1=$val ;; 2) SP2=$val ;; 3) SP3=$val ;; 4) SP4=$val ;;
    esac
    read_all_temps
    case "$loop" in
        1) csv_line "set loop$1 -> $val$([ -n "$type" ] && echo " ($type)")" "$SP1" "${SP2:-?}" "$TA" "$TB" ;;
        2) csv_line "set loop$1 -> $val$([ -n "$type" ] && echo " ($type)")" "${SP1:-?}" "$SP2" "$TA" "$TB" ;;
        *) csv_line "set loop$1 -> $val$([ -n "$type" ] && echo " ($type)")" "${SP1:-?}" "${SP2:-?}" "$TA" "$TB" ;;
    esac
}

dev_control() {  # dev_control on|off
    if [ "$1" = "on" ]; then
        if [ "$MODE" = tcp ]; then tcp_cmd "CONTROL" || return 1
        else http_post "HtrControl.cgi" "Control=Control" || return 1; fi
    else
        if [ "$MODE" = tcp ]; then tcp_cmd "STOP" || return 1
        else http_post "HtrControl.cgi" "Stop=Stop" || return 1; fi
    fi
    read_all_temps
    csv_line "control $1" "${SP1:-?}" "${SP2:-?}" "$TA" "$TB"
}

# ------------------------------------------------------------ http transport --
HTTP_BASE() { printf 'http://%s:%s' "$HOST" "$HTTP_PORT"; }

http_get() {  # http_get <page> -> body
    curl -s -m 10 "$(HTTP_BASE)/$1"
}

http_post() {  # http_post <cgi> <urlencoded-data>
    local url data
    url="$(HTTP_BASE)/$1"
    data="$2"
    if [ "$DRY_RUN" -eq 1 ]; then say "DRY-RUN http POST $url  data=$data"; return 0; fi
    local out rc
    out=$(curl -s -m 10 -o /dev/null -w '%{http_code}' \
        -X POST -H 'Content-Type: application/x-www-form-urlencoded' \
        --data "$data" "$url"); rc=$?
    if [ $rc -ne 0 ]; then err "curl failed ($rc) posting to $url"; return 1; fi
    case "$out" in
        200|201|204|302|303) say "http POST $1 -> $out"; return 0 ;;
        *) err "http POST $1 -> unexpected status $out"; return 1 ;;
    esac
}

# extract value="..." of a named text input from a downloaded page
parse_value() {  # parse_value <file> <input-name>
    sed -n 's/.*name="'"$2"'"[^>]*value="\([^"]*\)".*/\1/p' "$1" | head -1 \
        | sed 's/[[:space:]]*$//'
}

# extract the selected option text of a named select block
parse_select() {  # parse_select <file> <select-name>
    sed -n '/<select name="'"$2"'"/,/<\/select>/p' "$1" \
        | awk '/selected/ && /<option/ {
                   line = $0
                   sub(/^[^>]*>/, "", line)   # strip the opening <option ...> tag
                   sub(/<.*$/, "", line)      # strip the closing </option> tag
                   gsub(/^[[:space:]]+|[[:space:]]+$/, "", line)
                   print line; exit
               }'
}

dev_rate() {  # dev_rate <loop> <K_per_min>  (command port only)
    local loop="$1" val="$2"
    if [ "$MODE" != tcp ]; then
        err "rate is only settable over the command port (tcp mode) or the front panel"
        return 1
    fi
    tcp_cmd "LOOP $loop:RATE $val" || return 1
    local rb
    rb=$(tcp_query "LOOP $loop:RATE?" | tr -d '\r ')
    [ -n "$rb" ] && say "ramp rate of loop $loop now: $rb K/min"
    read_all_temps
    csv_line "rate loop$loop -> $val (read-back $rb)" "${SP1:-?}" "${SP2:-?}" "$TA" "$TB"
}

http_set_loop() {  # http_set_loop <loop> <value> [type]
    local loop="$1" val="$2" type="${3:-}" tmp cur_set cur_src cur_type cur_p cur_i cur_d cur_pman cur_range hid data try
    tmp=$(mktemp) || return 1
    cur_type=""
    for try in 1 2 3; do
        http_get output.htm > "$tmp"
        cur_set=$(parse_value "$tmp" "Set$loop")
        cur_src=$(parse_select "$tmp" "Source$loop")
        cur_type=$(parse_select "$tmp" "Type$loop")
        cur_p=$(parse_value "$tmp" "P$loop")
        cur_i=$(parse_value "$tmp" "I$loop")
        cur_d=$(parse_value "$tmp" "D$loop")
        cur_pman=$(parse_value "$tmp" "Pman$loop")
        cur_range=$(parse_select "$tmp" "Range$loop")
        [ -n "$cur_type" ] && break
        err "could not parse output.htm (loop $loop, attempt $try; $(wc -c < "$tmp" | tr -d ' ') bytes fetched) — retrying"
        sleep 2
    done
    rm -f "$tmp"
    [ -z "$cur_type" ] && { err "could not parse output.htm after 3 attempts"; return 1; }

    # unit-aware formatting: the field is maxlength=10, e.g. "250.000K"
    local new_set
    new_set=$(printf '%.3fK' "$val" | cut -c1-10)
    [ -n "$type" ] && cur_type="$type"

    # Loop 1's hidden field is (quirk of the firmware) named LoopID2
    case "$loop" in
        1) hid="LoopID2=1" ;;
        *) hid="LoopID=$loop" ;;
    esac

    data="$hid"
    data="$data&Set$loop=$(printf '%s' "$new_set" | sed 's/ /%20/g')"
    data="$data&Source$loop=$(printf '%s' "$cur_src" | sed 's/ /%20/g')"
    data="$data&Type$loop=$(printf '%s' "$cur_type" | sed 's/ /%20/g')"
    data="$data&P$loop=$(printf '%s' "$cur_p" | sed 's/ /%20/g')"
    data="$data&I$loop=$(printf '%s' "$cur_i" | sed 's/ /%20/g')"
    data="$data&D$loop=$(printf '%s' "$cur_d" | sed 's/ /%20/g')"
    data="$data&Pman$loop=$(printf '%s' "$cur_pman" | sed 's/ /%20/g')"
    data="$data&Range$loop=$(printf '%s' "$cur_range" | sed 's/ /%20/g')"
    data="$data&UpdateLoop$loop=Update"

    http_post "OUTchannel.cgi" "$data"
}

# --------------------------------------------------------------- scheduler --
wait_stable() {  # wait_stable <loop> <tol_K> [timeout_s]
    local loop="$1" tol="$2" timeout="${3:-3600}" src target t elapsed=0
    case "$loop" in
        1) src=$L1_SRC; target=${SP1:-$(dev_setpoint 1)} ;;
        2) src=$L2_SRC; target=${SP2:-$(dev_setpoint 2)} ;;
        3) src=$L3_SRC; target=${SP3:-$(dev_setpoint 3)} ;;
        4) src=$L4_SRC; target=${SP4:-$(dev_setpoint 4)} ;;
    esac
    target=$(strip_k "$target")
    if [ -z "$target" ] || [ "$target" = "?" ]; then
        err "wait_stable: no setpoint known for loop $loop; run 'set' first"
        return 1
    fi
    say "waiting for loop $loop (input $src) to reach ${target}K +/- ${tol}K (timeout ${timeout}s)"
    while [ "$elapsed" -lt "$timeout" ]; do
        t=$(dev_temp "$src")
        if [ "$t" != "?" ] && [ -n "$t" ]; then
            local within pwr=""
            within=$(awk -v t="$t" -v sp="$target" -v tol="$tol" \
                'BEGIN { d = t - sp; if (d < 0) d = -d; print (d <= tol) ? 1 : 0 }')
            if [ "$MODE" = tcp ]; then
                pwr=$(tcp_query "LOOP $loop:OUTPWR?" | tr -d '\r %')
            fi
            say "poll loop$loop  T=$t K  setpoint=$target K${pwr:+  power=$pwr%}"
            read_all_temps
            csv_line "poll loop$loop T=$t sp=$target P=${pwr:-na}" "${SP1:-?}" "${SP2:-?}" "$TA" "$TB"
            if [ "$within" = 1 ]; then
                say "stable: input $src at ${t}K (setpoint ${target}K)"
                return 0
            fi
        fi
        sleep "$POLL"
        elapsed=$((elapsed + POLL))
    done
    err "wait_stable timed out after ${timeout}s (loop $loop)"
    return 1
}

run_command() {  # run_command <line...>
    local cmd="$1"; shift || true
    case "$cmd" in
        control)
            say "control ON"; dev_control on ;;
        stop)
            say "control STOP"; dev_control off ;;
        set)
            [ $# -ge 2 ] || { err "usage: set <loop> <value> [type]"; return 1; }
            say "set loop $1 -> $2${3:+ (type $3)}"
            dev_set "$1" "$2" "${3:-}" ;;
        rate)
            [ $# -ge 2 ] || { err "usage: rate <loop> <K_per_min>"; return 1; }
            say "set ramp rate of loop $1 -> $2 K/min"
            dev_rate "$1" "$2" ;;
        wait)
            [ $# -ge 1 ] || { err "usage: wait <seconds>"; return 1; }
            if [ "$DRY_RUN" -eq 1 ]; then say "DRY-RUN wait $1 s"; return 0; fi
            say "waiting $1 s"; sleep "$1" ;;
        stable)
            [ $# -ge 2 ] || { err "usage: stable <loop> <tol> [timeout]"; return 1; }
            if [ "$DRY_RUN" -eq 1 ]; then say "DRY-RUN stable $*"; return 0; fi
            wait_stable "$1" "$2" "${3:-3600}" ;;
        input)
            [ $# -ge 1 ] || { err "usage: input <A|B>"; return 1; }
            local t; t=$(dev_temp "$1")
            say "input $(printf '%s' "$1" | tr 'ab' 'AB') = ${t} K" ;;
        log)
            read_all_temps
            csv_line "log" "${SP1:-?}" "${SP2:-?}" "$TA" "$TB"
            say "T(A)=$TA K  T(B)=$TB K" ;;
        *)
            err "unknown schedule command: $cmd $*"; return 1 ;;
    esac
}

syntax() {
    sed -n '2,35p' "$0" | grep -E '^#( |$)' | sed 's/^# \{0,1\}//'
}

# ------------------------------------------------------------------- main ---
ONCE=""
while [ $# -gt 0 ]; do
    case "$1" in
        --host)       HOST="$2"; shift 2 ;;
        --mode)       MODE="$2"; shift 2 ;;
        --tcp-port)   TCP_PORT="$2"; shift 2 ;;
        --http-port)  HTTP_PORT="$2"; shift 2 ;;
        --poll)       POLL="$2"; shift 2 ;;
        --log)        LOGFILE="$2"; shift 2 ;;
        --dry-run)    DRY_RUN=1; shift ;;
        --once)       ONCE="$2"; shift 2 ;;
        --syntax|-h)  syntax; exit 0 ;;
        -*)           err "unknown option: $1"; exit 2 ;;
        *)            SCHEDULE_FILE="$1"; shift ;;
    esac
done

case "$MODE" in
    tcp)  ;;
    http) ;;
    *) err "--mode must be tcp or http"; exit 2 ;;
esac

csv_init
say "cryocon.sh  host=$HOST  mode=$MODE  tcp=$TCP_PORT  http=$HTTP_PORT  log=$LOGFILE"

# initial state snapshot (tcp mode can read it back; http mode starts unknown)
SP1=""; SP2=""; SP3=""; SP4=""
if [ "$MODE" = tcp ] && [ "$DRY_RUN" -eq 0 ]; then
    idn=$(tcp_query "*IDN?")
    [ -n "$idn" ] && say "connected: $idn"
    SP1=$(strip_k "$(dev_setpoint 1)")
    SP2=$(strip_k "$(dev_setpoint 2)")
fi

if [ -n "$ONCE" ]; then
    # shellcheck disable=SC2086
    run_command $ONCE
    exit $?
fi

if [ -z "$SCHEDULE_FILE" ]; then
    err "no schedule file given (or use --once)"; syntax; exit 2
fi
if [ ! -f "$SCHEDULE_FILE" ]; then
    err "schedule file not found: $SCHEDULE_FILE"; exit 2
fi

ln=0
while IFS= read -r line || [ -n "$line" ]; do
    ln=$((ln + 1))
    # strip comments and whitespace-only lines
    line=$(printf '%s' "$line" | sed 's/#.*//' | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')
    [ -z "$line" ] && continue
    say "schedule[$ln]: $line"
    # shellcheck disable=SC2086
    if ! run_command $line; then
        err "schedule aborted at line $ln: '$line'"
        exit 1
    fi
done < "$SCHEDULE_FILE"

read_all_temps
say "schedule complete. T(A)=$TA K  T(B)=$TB K  log: $LOGFILE"
