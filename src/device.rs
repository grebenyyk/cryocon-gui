//! Device protocol layer for the Cryo-con Model 22C.
//!
//! The instrument speaks a small ASCII command language over a raw TCP
//! socket (default port 5000): you send a line terminated with CRLF, and
//! queries (commands containing '?') answer with one CRLF-terminated line.
//! Set commands usually answer nothing; unknown commands answer "NAK".
//!
//! All of this was verified against a real 22C (firmware 3.39F) and against
//! `mock_cryocon.py`, an offline simulator used for development.
//!
//! Architecture: one [`Worker`] thread *owns* the TCP connection. Everyone
//! else (UI, schedule runner) talks to it through channels:
//!
//!   UI / runner --Request--> [Worker thread] --DeviceEvent--> UI
//!
//! A [`Request`] carries its own one-shot reply channel, so callers can
//! block for an answer without any shared mutable state. The worker also
//! polls the instrument once per second when idle and publishes a
//! [`Snapshot`] — this is what the live view shows.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

/// A command for the instrument (plus the reply channel for its answer).
pub struct Request {
    pub cmd: DeviceCmd,
    pub reply: Sender<DeviceReply>,
}

#[derive(Clone, Debug)]
pub enum DeviceCmd {
    /// stop the worker thread entirely (app is disconnecting)
    Shutdown,
    /// read everything we display (inputs, setpoint, rate, heater power)
    Poll,
    /// arbitrary raw query line (used rarely; e.g. "*IDN?")
    RawQuery(String),
    SetSetpoint { loop_n: u8, value: f64 },
    SetLoopType { loop_n: u8, typ: String },
    SetRate { loop_n: u8, value_k_per_min: f64 },
    Control,
    Stop,
}

#[derive(Clone, Debug)]
pub enum DeviceReply {
    /// answer to a poll
    Snapshot(Snapshot),
    /// query answered with text (e.g. "*IDN?" -> "Cryo-con,22C,...")
    Text(String),
    /// set command accepted (or at least not rejected)
    Ok,
    /// instrument explicitly rejected the command ("NAK")
    Nak(String),
    /// transport-level failure (not connected, timeout, ...)
    Error(String),
}

/// One periodic reading of everything we care about.
/// `None` fields mean "no answer" — for channel B that is the normal
/// "no sensor connected" case, which the instrument reports as ".......".
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub t_a: Option<f64>,
    pub t_b: Option<f64>,
    pub setpoint_1: Option<f64>,
    pub rate_1: Option<f64>,
    pub power_1: Option<f64>,
    pub when: Instant,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            t_a: None,
            t_b: None,
            setpoint_1: None,
            rate_1: None,
            power_1: None,
            when: Instant::now(), // Instant has no Default; "now" is the sensible start
        }
    }
}

/// Events the worker pushes to the UI unprompted.
#[derive(Clone, Debug)]
pub enum DeviceEvent {
    Connected(String),   // carries *IDN? text
    Disconnected(String), // carries the reason; worker keeps retrying
    Snapshot(Snapshot),
}

// --------------------------------------------------------------------------
// protocol parsing helpers (unit-tested below)
// --------------------------------------------------------------------------

/// Parse a numeric instrument answer.
///
/// Real answers look like `"299.000000K"`, `" 25.000000"`, `"62.391110"`.
/// A sensor with nothing connected answers `"......."` — that is *not* a
/// number and must never become one (a missing sensor reading "stable"
/// was an actual bug class in the bash predecessor).
pub fn parse_number(s: &str) -> Option<f64> {
    // strip whitespace and a trailing K/k unit, then require a real number
    let cleaned: String = s
        .trim()
        .trim_end_matches(['K', 'k'])
        .trim()
        .chars()
        .collect();
    // reject dot-fillers and anything else non-numeric in one step
    if cleaned.is_empty()
        || !cleaned
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E'))
    {
        return None;
    }
    cleaned.parse::<f64>().ok()
}

/// The instrument's negative acknowledgement for unknown commands.
pub fn is_nak(s: &str) -> bool {
    s.trim().eq_ignore_ascii_case("nak")
}

// --------------------------------------------------------------------------
// the worker
// --------------------------------------------------------------------------

pub struct Worker;

impl Worker {
    /// Spawn the worker thread. It immediately tries to connect to
    /// `host:port` and keeps retrying (2 s backoff) until it succeeds or
    /// receives [`DeviceCmd::Shutdown`].
    pub fn spawn(
        host: String,
        port: u16,
        requests: Receiver<Request>,
        events: Sender<DeviceEvent>,
    ) -> std::thread::JoinHandle<()> {
        std::thread::Builder::new()
            .name("device-worker".into())
            .spawn(move || {
                let addr = format!("{host}:{port}");
                let mut backoff = Instant::now();
                let mut conn: Option<Connection> = None;

                loop {
                    // ---- try to (re)connect while we have none ----------
                    if conn.is_none() {
                        // serve shutdown even while offline
                        match requests.recv_timeout(Duration::from_millis(200)) {
                            Ok(req) if matches!(req.cmd, DeviceCmd::Shutdown) => return,
                            Ok(req) => {
                                let _ = req.reply.send(DeviceReply::Error(
                                    "not connected".into(),
                                ));
                            }
                            Err(RecvTimeoutError::Timeout) => {}
                            Err(RecvTimeoutError::Disconnected) => return,
                        }
                        if Instant::now() >= backoff {
                            match Connection::connect(&addr) {
                                Ok(mut c) => {
                                    // greet with *IDN? so the UI can show
                                    // exactly which instrument answered
                                    let idn = c.query("*IDN?").unwrap_or_default();
                                    let _ =
                                        events.send(DeviceEvent::Connected(idn));
                                    conn = Some(c);
                                }
                                Err(_) => {
                                    backoff = Instant::now() + Duration::from_secs(2)
                                }
                            }
                        }
                        continue;
                    }
                    // ---- connected: serve requests + poll ----------------
                    let mut c = conn.take().unwrap();
                    let mut last_poll = Instant::now() - Duration::from_secs(10);
                    'connected: loop {
                        // handle pending requests first (up to 100 ms wait)
                        match requests.recv_timeout(Duration::from_millis(100)) {
                            Ok(req) if matches!(req.cmd, DeviceCmd::Shutdown) => return,
                            Ok(req) => {
                                let keep = c.serve(req, &events);
                                if !keep {
                                    let _ = events.send(DeviceEvent::Disconnected(
                                        "connection lost".into(),
                                    ));
                                    break 'connected;
                                }
                            }
                            Err(RecvTimeoutError::Timeout) => {}
                            Err(RecvTimeoutError::Disconnected) => return,
                        }
                        // poll once per second when idle
                        if last_poll.elapsed() >= Duration::from_secs(1) {
                            match c.poll(&events) {
                                Ok(()) => last_poll = Instant::now(),
                                Err(e) => {
                                    let _ = events.send(DeviceEvent::Disconnected(e));
                                    break 'connected;
                                }
                            }
                        }
                    }
                    conn = None;
                    backoff = Instant::now() + Duration::from_secs(2);
                }
            })
            .expect("failed to spawn device worker thread")
    }
}

// --------------------------------------------------------------------------

/// An open connection plus its read/write halves.
struct Connection {
    writer: TcpStream,
    reader: BufReader<TcpStream>,
}

impl Connection {
    fn connect(addr: &str) -> Result<Self, String> {
        let stream = TcpStream::connect(addr).map_err(|e| e.to_string())?;
        stream
            .set_nodelay(true)
            .map_err(|e| e.to_string())?;
        let writer = stream.try_clone().map_err(|e| e.to_string())?;
        Ok(Self {
            writer: stream,
            reader: BufReader::new(writer.try_clone().map_err(|e| e.to_string())?),
        })
    }

    /// Send one command line. Safe on its own; IO errors bubble up.
    fn send(&mut self, line: &str) -> Result<(), String> {
        self.writer
            .write_all(format!("{line}\r\n").as_bytes())
            .map_err(|e| e.to_string())
    }

    /// Read one answer line. `wait_ms` is how long we'll block for an
    /// answer: generous for queries, short for set commands (which usually
    /// answer nothing at all, so a timeout simply means "accepted").
    fn read_line(&mut self, wait_ms: u64) -> Result<Option<String>, String> {
        self.writer
            .set_read_timeout(Some(Duration::from_millis(wait_ms)))
            .map_err(|e| e.to_string())?;
        let mut buf = String::new();
        match self.reader.read_line(&mut buf) {
            Ok(0) => Ok(None), // device closed the line cleanly / nothing came
            Ok(_) => Ok(Some(buf.trim_end_matches(['\r', '\n']).to_string())),
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                Ok(None)
            }
            Err(e) => Err(e.to_string()),
        }
    }

    /// Perform a query (expects one answer line).
    fn query(&mut self, q: &str) -> Result<String, String> {
        self.send(q)?;
        match self.read_line(2000)? {
            Some(line) => Ok(line),
            None => Err(format!("no answer to {q:?}")),
        }
    }

    /// Perform a set command (no answer expected; NAK is an error).
    fn set(&mut self, cmd: &str) -> Result<(), String> {
        self.send(cmd)?;
        match self.read_line(400)? {
            None => Ok(()), // silence = accepted, as on the real box
            Some(line) if is_nak(&line) => Err(format!("instrument rejected {cmd:?}")),
            Some(_) => Ok(()), // some firmwares echo *something*; accept
        }
    }

    /// Poll everything the live view needs; emits a Snapshot event.
    /// Returns Err only on transport failure (caller drops the link).
    fn poll(&mut self, events: &Sender<DeviceEvent>) -> Result<(), String> {
        let snap = self
            .poll_snapshot()
            .unwrap_or_default();
        let _ = events.send(DeviceEvent::Snapshot(snap));
        Ok(())
    }

    fn poll_snapshot(&mut self) -> Option<Snapshot> {
        let t_a = parse_number(&self.query("INPUT? A").ok()?);
        let t_b = parse_number(&self.query("INPUT? B").ok()?);
        let setpoint_1 = parse_number(&self.query("LOOP 1:SETP?").ok()?);
        let rate_1 = parse_number(&self.query("LOOP 1:RATE?").ok()?);
        let power_1 = parse_number(&self.query("LOOP 1:OUTPWR?").ok()?);
        Some(Snapshot {
            t_a,
            t_b,
            setpoint_1,
            rate_1,
            power_1,
            when: Instant::now(),
        })
    }

    /// Answer one request from the UI / schedule runner.
    /// Returns false when the connection died and should be dropped.
    fn serve(&mut self, req: Request, events: &Sender<DeviceEvent>) -> bool {
        let reply = match req.cmd {
            DeviceCmd::Shutdown => return false, // caller handles
            DeviceCmd::Poll => match self.poll_snapshot() {
                Some(s) => DeviceReply::Snapshot(s),
                None => {
                    let _ = events.send(DeviceEvent::Disconnected(
                        "connection lost during poll".into(),
                    ));
                    return false;
                }
            },
            DeviceCmd::RawQuery(q) => match self.query(&q) {
                Ok(text) => DeviceReply::Text(text),
                Err(e) => {
                    let _ = events.send(DeviceEvent::Disconnected(e));
                    return false;
                }
            },
            DeviceCmd::SetSetpoint { loop_n, value } => {
                self.set(&format!("LOOP {loop_n}:SETP {value}")).into_reply()
            }
            DeviceCmd::SetLoopType { loop_n, typ } => {
                self.set(&format!("LOOP {loop_n}:TYPE {typ}")).into_reply()
            }
            DeviceCmd::SetRate { loop_n, value_k_per_min } => {
                self.set(&format!("LOOP {loop_n}:RATE {value_k_per_min}")).into_reply()
            }
            DeviceCmd::Control => self.set("CONTROL").into_reply(),
            DeviceCmd::Stop => self.set("STOP").into_reply(),
        };
        let _ = req.reply.send(reply);
        true
    }
}

/// Small helper: turn a set()-style Result into a DeviceReply.
trait IntoReply {
    fn into_reply(self) -> DeviceReply;
}
impl IntoReply for Result<(), String> {
    fn into_reply(self) -> DeviceReply {
        match self {
            Ok(()) => DeviceReply::Ok,
            // an instrument NAK comes back with the command quoted in the
            // error string; separate the two cases for the UI's benefit
            Err(e) if e.starts_with("instrument rejected") => DeviceReply::Nak(e),
            Err(e) => DeviceReply::Error(e),
        }
    }
}

// --------------------------------------------------------------------------
// unit tests
// --------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_with_units_and_spaces() {
        assert_eq!(parse_number("299.000000K\r\n"), Some(299.0));
        assert_eq!(parse_number(" 25.000000 "), Some(25.0));
        assert_eq!(parse_number("62.391110"), Some(62.391110));
        assert_eq!(parse_number("PID  "), None);
    }

    #[test]
    fn missing_sensor_is_never_a_number() {
        // "......." means no sensor — it must not parse (not even as 0)
        assert_eq!(parse_number("......."), None);
        assert_eq!(parse_number(""), None);
    }

    #[test]
    fn nak_detection() {
        assert!(is_nak("NAK"));
        assert!(is_nak(" nak "));
        assert!(!is_nak("299.0"));
    }
}
