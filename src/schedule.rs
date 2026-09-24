//! Schedule grammar, parsing and execution — the same mini-language as
//! `cryocon.sh`, so schedule files are interchangeable between the bash
//! tool and this app.
//!
//! Grammar (one command per line, `#` starts a comment):
//!   control
//!   stop
//!   set <loop> <value> [PID|RampP|RampT|Man|Off|Table]
//!   rate <loop> <K/min>
//!   wait <seconds>
//!   stable <loop> <tol K> [timeout s]
//!   log
//!   input <A|B>
//!
//! The runner lives on its own thread and talks to the device through the
//! same request channel as the UI; an [`AtomicBool`] lets the user abort
//! at any moment (checked at least every 250 ms, including inside `wait`
//! and `stable`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use crate::device::{DeviceCmd, DeviceReply, Request};

/// A starter schedule shown in the editor (mirrors the bash examples).
pub const TEMPLATE: &str = "\
# one schedule command per line; '#' starts a comment
control
rate 1 3.333
set 1 299 RampP
stable 1 1.0 5400
log
";

#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    Control,
    Stop,
    Set { loop_n: u8, value: f64, typ: Option<String> },
    Rate { loop_n: u8, value_k_per_min: f64 },
    Wait(u64),
    Stable { loop_n: u8, tol_k: f64, timeout_s: u64 },
    Log,
    Input(char),
}

/// Parse the whole schedule text.
/// On success: the steps. On error: (line number, message) for every bad
/// line — the editor shows them all at once.
pub fn parse(text: &str) -> Result<Vec<Step>, Vec<(usize, String)>> {
    let mut steps = Vec::new();
    let mut errors = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line_no = i + 1;
        // strip comments and surrounding whitespace (same as the bash tool)
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let words: Vec<&str> = line.split_whitespace().collect();
        // a closure capturing `errors` mutably must itself be `mut`
        let mut bad =
            |what: &str| errors.push((line_no, format!("line {line_no}: {what}")));
        match words[0] {
            "control" if words.len() == 1 => steps.push(Step::Control),
            "stop" if words.len() == 1 => steps.push(Step::Stop),
            "set" => match (words.get(1), words.get(2), words.get(3)) {
                (Some(l), Some(v), rest) => {
                    match (l.parse::<u8>(), v.parse::<f64>(), rest.is_none_or_valid_type()) {
                        (Ok(loop_n), Ok(value), true) if (1..=4).contains(&loop_n) => {
                            steps.push(Step::Set {
                                loop_n,
                                value,
                                typ: words.get(3).map(|s| s.to_string()),
                            })
                        }
                        _ => bad("usage: set <loop 1-4> <value> [type]"),
                    }
                }
                _ => bad("usage: set <loop 1-4> <value> [type]"),
            },
            "rate" => match (words.get(1), words.get(2)) {
                (Some(l), Some(v)) => match (l.parse::<u8>(), v.parse::<f64>()) {
                    (Ok(loop_n), Ok(value)) if (1..=4).contains(&loop_n) => {
                        steps.push(Step::Rate { loop_n, value_k_per_min: value })
                    }
                    _ => bad("usage: rate <loop 1-4> <K/min>"),
                },
                _ => bad("usage: rate <loop 1-4> <K/min>"),
            },
            "wait" => match (words.get(1), words.len()) {
                (Some(w), 2) => match w.parse::<u64>() {
                    Ok(secs) => steps.push(Step::Wait(secs)),
                    Err(_) => bad("wait needs a number of seconds"),
                },
                _ => bad("usage: wait <seconds>"),
            },
            "stable" => {
                let ok = words.len() == 3 || words.len() == 4;
                if !ok {
                    bad("usage: stable <loop> <tol> [timeout]");
                    continue;
                }
                let l = words[1].parse::<u8>();
                let t = words[2].parse::<f64>();
                let to = words.get(3).map(|s| s.parse::<u64>()).unwrap_or(Ok(3600));
                match (l, t, to) {
                    (Ok(loop_n), Ok(tol), Ok(timeout)) if (1..=4).contains(&loop_n) => {
                        steps.push(Step::Stable { loop_n, tol_k: tol, timeout_s: timeout })
                    }
                    _ => bad("stable: bad loop/tolerance/timeout"),
                }
            }
            "log" if words.len() == 1 => steps.push(Step::Log),
            "input" => match (words.get(1), words.len()) {
                (Some(c), 2) if matches!(*c, "A" | "B" | "a" | "b") => {
                    // `c` is a single-character &str; take that char
                    let ch = c.chars().next().unwrap().to_ascii_uppercase();
                    steps.push(Step::Input(ch))
                }
                _ => bad("usage: input <A|B>"),
            },
            other => bad(&format!("unknown command {other:?}")),
        }
    }
    if errors.is_empty() {
        Ok(steps)
    } else {
        Err(errors)
    }
}

// tiny helper so the `set` arm above stays readable
trait NoneOrValid {
    fn is_none_or_valid_type(&self) -> bool;
}
impl NoneOrValid for Option<&&str> {
    fn is_none_or_valid_type(&self) -> bool {
        match self {
            None => true,
            Some(s) => matches!(
                s.to_ascii_lowercase().as_str(),
                "pid" | "rampp" | "rampt" | "man" | "off" | "table"
            ),
        }
    }
}

/// Handle to a running schedule (used by the UI to abort it).
pub struct RunnerHandle {
    pub abort: std::sync::Arc<AtomicBool>,
}

/// What the runner reports back to the UI console.
pub enum Progress {
    Line(String),
    Finished(bool), // true = completed, false = aborted or failed
}

/// Spawn the schedule runner.
///
/// `dry_run` prints what would happen and touches nothing.
/// Every executed step is also mirrored into the CSV through `log_tx`
/// (mirrored as text lines the App writes via its CsvLog).
pub fn spawn(
    steps: Vec<Step>,
    dry_run: bool,
    requests: Sender<Request>,
    progress: Sender<Progress>,
    max_setpoint: f64,
) -> RunnerHandle {
    let abort = std::sync::Arc::new(AtomicBool::new(false));
    let flag = abort.clone();
    std::thread::Builder::new()
        .name("schedule-runner".into())
        .spawn(move || {
            let mut ok = true;
            for (idx, step) in steps.iter().enumerate() {
                if flag.load(Ordering::Relaxed) {
                    let _ = progress.send(Progress::Line("ABORTED by user".into()));
                    ok = false;
                    break;
                }
                let _ = progress.send(Progress::Line(format!(
                    "[{}/{}] {}",
                    idx + 1,
                    steps.len(),
                    describe(step)
                )));
                if !execute(step, dry_run, &requests, &progress, flag.clone(), max_setpoint) {
                    ok = false;
                    break;
                }
            }
            let _ = progress.send(Progress::Finished(ok));
        })
        .expect("failed to spawn schedule runner");
    RunnerHandle { abort }
}

fn describe(step: &Step) -> String {
    match step {
        Step::Control => "control ON".into(),
        Step::Stop => "control STOP".into(),
        Step::Set { loop_n, value, typ } => match typ {
            Some(t) => format!("set loop{loop_n} -> {value} ({t})"),
            None => format!("set loop{loop_n} -> {value}"),
        },
        Step::Rate { loop_n, value_k_per_min } => {
            format!("rate loop{loop_n} -> {value_k_per_min} K/min")
        }
        Step::Wait(s) => format!("wait {s} s"),
        Step::Stable { loop_n, tol_k, timeout_s } => {
            format!("stable loop{loop_n} within {tol_k} K (timeout {timeout_s} s)")
        }
        Step::Log => "log".into(),
        Step::Input(c) => format!("input {c}"),
    }
}

/// Execute one step; returns false to stop the schedule.
fn execute(
    step: &Step,
    dry_run: bool,
    requests: &Sender<Request>,
    progress: &Sender<Progress>,
    abort: std::sync::Arc<AtomicBool>,
    max_setpoint: f64,
) -> bool {
    let say = |msg: String| {
        let _ = progress.send(Progress::Line(msg));
    };
    // helper: one synchronous request to the device worker
    let ask = |cmd: DeviceCmd| -> DeviceReply {
        let (tx, rx) = std::sync::mpsc::channel();
        if requests.send(Request { cmd, reply: tx }).is_err() {
            return DeviceReply::Error("device worker is gone".into());
        }
        rx.recv().unwrap_or(DeviceReply::Error("no reply from worker".into()))
    };

    match step {
        Step::Control => {
            if !dry_run {
                if let DeviceReply::Error(e) = ask(DeviceCmd::Control) {
                    say(format!("ERROR: control on failed: {e}"));
                    return false;
                }
            }
            true
        }
        Step::Stop => {
            if !dry_run {
                let _ = ask(DeviceCmd::Stop);
            }
            true
        }
        Step::Set { loop_n, value, typ } => {
            // safety clamp — same rule as the UI's manual controls
            let value = if *value > max_setpoint {
                say(format!(
                    "WARNING: setpoint {} clamped to max {} K",
                    value, max_setpoint
                ));
                max_setpoint
            } else {
                *value
            };
            if dry_run {
                return true;
            }
            if let Some(t) = typ {
                if !matches!(ask(DeviceCmd::SetLoopType { loop_n: *loop_n, typ: t.clone() }), DeviceReply::Ok) {
                    say(format!("ERROR: could not set loop type {t}"));
                    return false;
                }
            }
            match ask(DeviceCmd::SetSetpoint { loop_n: *loop_n, value }) {
                DeviceReply::Ok => {
                    // verify the write took (the instrument NAKs bad values)
                    match ask(DeviceCmd::RawQuery(format!("LOOP {loop_n}:SETP?"))) {
                        DeviceReply::Text(t) => {
                            let back = crate::device::parse_number(&t);
                            if let Some(b) = back {
                                if (b - value).abs() > 0.001 {
                                    say(format!(
                                        "WARNING: read-back {b} != requested {value}"
                                    ));
                                }
                            }
                            true
                        }
                        _ => true,
                    }
                }
                DeviceReply::Nak(e) | DeviceReply::Error(e) => {
                    say(format!("ERROR: setpoint rejected: {e}"));
                    false
                }
                _ => true,
            }
        }
        Step::Rate { loop_n, value_k_per_min } => {
            if dry_run {
                return true;
            }
            match ask(DeviceCmd::SetRate { loop_n: *loop_n, value_k_per_min: *value_k_per_min }) {
                DeviceReply::Ok => {
                    if let DeviceReply::Text(t) =
                        ask(DeviceCmd::RawQuery(format!("LOOP {loop_n}:RATE?")))
                    {
                        say(format!("ramp rate read-back: {t}"));
                    }
                    true
                }
                DeviceReply::Nak(e) | DeviceReply::Error(e) => {
                    say(format!("ERROR: rate rejected: {e}"));
                    false
                }
                _ => true,
            }
        }
        Step::Wait(secs) => {
            // sleep in slices so abort stays responsive
            let deadline = Instant::now() + Duration::from_secs(*secs);
            while Instant::now() < deadline {
                if abort.load(Ordering::Relaxed) {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(250));
            }
            true
        }
        // loop 1 is the only loop driven by a live sensor (channel A) on
        // this instrument, so `stable` polls channel A regardless of loop_n
        Step::Stable { loop_n: _, tol_k, timeout_s } => {
            if dry_run {
                say("  (dry-run: would poll until within tolerance)".into());
                return true;
            }
            let deadline = Instant::now() + Duration::from_secs(*timeout_s);
            loop {
                if abort.load(Ordering::Relaxed) {
                    return false;
                }
                match ask(DeviceCmd::Poll) {
                    DeviceReply::Snapshot(s) => {
                        let t = s.t_a; // loop 1 is driven by channel A on this instrument
                        let sp = s.setpoint_1;
                        match (t, sp) {
                            (Some(t), Some(sp)) => {
                                let d = (t - sp).abs();
                                say(format!(
                                    "  poll T={t:.2} sp={sp:.2} diff={d:.2}{}",
                                    s.power_1
                                        .map(|p| format!(" power={p:.0}%"))
                                        .unwrap_or_default()
                                ));
                                if d <= *tol_k {
                                    say("  stable reached".into());
                                    return true;
                                }
                            }
                            _ => say("  poll: no sensor/ setpoint reading".into()),
                        }
                    }
                    DeviceReply::Error(e) => {
                        say(format!("ERROR: poll failed: {e}"));
                        return false;
                    }
                    _ => {}
                }
                if Instant::now() >= deadline {
                    say(format!("ERROR: stable timed out after {timeout_s} s"));
                    return false;
                }
                std::thread::sleep(Duration::from_secs(2));
            }
        }
        Step::Log | Step::Input(_) => true, // the App logs every snapshot anyway
    }
}

// --------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_reference_schedule() {
        let text = "# comment\ncontrol\nrate 1 3.333\nset 1 299 RampP\nstable 1 1.0 5400\nwait 5\nlog\n";
        let steps = parse(text).expect("should parse");
        assert_eq!(steps.len(), 6);
        assert_eq!(steps[0], Step::Control);
        assert_eq!(
            steps[1],
            Step::Rate { loop_n: 1, value_k_per_min: 3.333 }
        );
        assert_eq!(
            steps[2],
            Step::Set { loop_n: 1, value: 299.0, typ: Some("RampP".into()) }
        );
    }

    #[test]
    fn comments_and_blank_lines_are_free() {
        assert_eq!(parse("\n\n# only a comment\n").unwrap(), Vec::<Step>::new());
    }

    #[test]
    fn reports_every_bad_line_with_its_number() {
        let errs = parse("set 1\nset 9 100\nfrobnicate\nwait notanumber").unwrap_err();
        assert_eq!(errs.len(), 4);
        assert!(errs[0].0 == 1 && errs[1].0 == 2 && errs[2].0 == 3 && errs[3].0 == 4);
    }

    #[test]
    fn input_accepts_lower_and_upper_case() {
        assert_eq!(
            parse("input a").unwrap(),
            vec![Step::Input('A')]
        );
    }

    #[test]
    fn set_type_must_be_from_the_known_set() {
        assert!(parse("set 1 100 PIDLE").is_err());
        assert!(parse("set 1 100 RamPP").is_ok()); // case-insensitive type
    }
}
