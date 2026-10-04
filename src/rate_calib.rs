//! Live rate calibration for RampP ramps.
//!
//! This unit's 22C firmware (fw 3.39F) advances its internal setpoint at
//! ~0.84x the commanded rate (measured 2026-09-30 at four commanded
//! rates, heater never saturated). Instead of trusting that constant,
//! the app can measure the scale right before a rate-bearing run:
//! a short out-and-back leg — heat a few K, fit the actual slope, glide
//! back — after which the planned rates are divided by the measured
//! scale so the *actual* slope matches what was asked for.
//!
//! The pure functions ([`plan_leg`], [`fit_slope`],
//! [`heating_rate_marks`]) are unit-tested; the leg itself runs on its
//! own thread ([`spawn`]), talking to the device through the same
//! request channel as everything else.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use crate::device::{DeviceCmd, DeviceReply, Request};
use crate::schedule::Step;

// --------------------------------------------------------------------------
// Planning (pure)
// --------------------------------------------------------------------------

/// Plan for the out-and-back calibration leg (temperatures in K).
#[derive(Clone, Debug)]
pub struct LegPlan {
    /// where the leg begins (= where it returns to)
    pub t_start: f64,
    /// the top of the out leg: t_start + delta
    pub leg_target: f64,
    /// the run's own planned rate (kept for the popup text)
    pub rate_plan: f64,
    /// commanded K/min for the out leg (the *probe* rate — see plan_leg)
    pub rate_cmd: f64,
    /// commanded K/min for the glide back down
    pub return_rate: f64,
    /// the app's max-setpoint clamp (the leg must stay under it)
    pub max_sp: f64,
}

/// Pick a leg for a planned heating ramp of `rate` K/min starting near
/// `t_now`. The leg does NOT probe at the plan's own rate: the measured
/// scale was rate-independent across 3-12 K/min (2026-09-30, four
/// commanded rates), so every leg probes inside that band whatever the
/// plan asks — that keeps the out leg inside the 2-3 min budget even
/// for violent plans (a 50 K/min probe would cross its own span before
/// a fit were possible). `None` when there is not enough headroom below
/// `max_setpoint`.
pub fn plan_leg(rate: f64, t_now: f64, max_setpoint: f64) -> Option<LegPlan> {
    if rate <= 0.0 {
        return None;
    }
    let probe = rate.clamp(4.0, 12.0);
    // 0.84 is our best prior for how far a commanded ramp actually
    // moves; two minutes at that estimate, clamped to a sane span
    let delta = (0.84 * probe * 2.0).clamp(5.0, 15.0);
    // the whole out leg must stay under the max-setpoint clamp
    let headroom = max_setpoint - 1.0 - t_now;
    let delta = delta.min(headroom);
    if delta < 3.0 {
        return None;
    }
    Some(LegPlan {
        t_start: t_now,
        leg_target: t_now + delta,
        rate_plan: rate,
        rate_cmd: probe,
        return_rate: (2.0 * probe).clamp(8.0, 25.0),
        max_sp: max_setpoint,
    })
}

/// Least-squares slope of y vs x (x in seconds), with R^2.
/// `None` when there are too few points or no x-span.
pub fn fit_slope(points: &[(f64, f64)]) -> Option<(f64, f64)> {
    if points.len() < 3 {
        return None;
    }
    let n = points.len() as f64;
    let sx: f64 = points.iter().map(|p| p.0).sum();
    let sy: f64 = points.iter().map(|p| p.1).sum();
    let sxx: f64 = points.iter().map(|p| p.0 * p.0).sum();
    let sxy: f64 = points.iter().map(|p| p.0 * p.1).sum();
    let denom = n * sxx - sx * sx;
    if denom.abs() < f64::EPSILON {
        return None;
    }
    let slope = (n * sxy - sx * sy) / denom;
    let intercept = (sy - slope * sx) / n;
    let my = sy / n;
    let ss_res: f64 = points
        .iter()
        .map(|p| {
            let d = p.1 - (intercept + slope * p.0);
            d * d
        })
        .sum();
    let ss_tot: f64 = points.iter().map(|p| (p.1 - my) * (p.1 - my)).sum();
    let r2 = if ss_tot > 0.0 { 1.0 - ss_res / ss_tot } else { 1.0 };
    Some((slope, r2))
}

/// Which `rate 1` steps drive a *heating* leg, given the run starts at
/// `t0`? Rates consumed by a cooling set are left alone — only heating
/// legs get corrected (the scale was measured on heating). The bool
/// reports a heating `set` that rides the instrument's stored rate (no
/// `rate` step in front of it) — such a leg cannot be corrected.
pub fn heating_rate_marks(steps: &[Step], t0: f64) -> (Vec<usize>, bool) {
    let mut t = t0;
    let mut pending: Option<usize> = None;
    let mut marks = Vec::new();
    let mut stored_rate_heat = false;
    for (i, step) in steps.iter().enumerate() {
        match step {
            Step::Rate { loop_n: 1, .. } => pending = Some(i),
            Step::Set { loop_n: 1, value, .. } => {
                if *value > t + 0.5 {
                    match pending {
                        Some(idx) => marks.push(idx),
                        None => stored_rate_heat = true,
                    }
                }
                t = *value; // approximate the plant position after this set
                pending = None; // the rate was consumed (or orphaned)
            }
            _ => {}
        }
    }
    (marks, stored_rate_heat)
}

/// Divide the marked rates by the measured scale, returning a note per
/// rate that actually changed (a scale of 1.0 changes nothing).
pub fn correct_rates(steps: &mut [Step], marks: &[usize], scale: f64) -> Vec<String> {
    let mut notes = Vec::new();
    for &i in marks {
        if let Step::Rate { loop_n: 1, value_k_per_min } = &mut steps[i] {
            let cmd = *value_k_per_min / scale;
            if (cmd - *value_k_per_min).abs() > 0.001 {
                notes.push(format!(
                    "rate {:.3} -> commanded {:.3} K/min (calibration x{scale:.3})",
                    *value_k_per_min, cmd
                ));
                *value_k_per_min = cmd;
            }
        }
    }
    notes
}

// --------------------------------------------------------------------------
// The leg (runs on its own thread)
// --------------------------------------------------------------------------

/// What the calibration thread reports back to the UI.
pub enum CalibMsg {
    Status(String),
    /// Ok(scale) = actual slope / commanded rate
    Done(Result<f64, String>),
}

// Time budget knobs — consts so the popup can quote them.
/// samples before this elapsed time are ignored: the plant is still
/// catching up with the glide when the ramp starts
pub const TRANSIENT_S: f64 = 20.0;
/// samples after the transient needed for a trustworthy fit (the leg
/// polls once a second, so this is also a minimum duration)
pub const MIN_FIT_PTS: usize = 8;
/// the temperature span the fit window must cover — the real quality
/// criterion (2 K at any probe rate dwarfs the 0.01 K sensor noise)
pub const MIN_FIT_DT_K: f64 = 2.0;
/// hard caps so a stuck leg cannot run forever
pub const OUT_TIMEOUT_S: f64 = 300.0;
pub const BACK_TIMEOUT_S: u64 = 300;
/// a leg anchored while the temperature is still moving fast measures
/// momentum, not the ramp — refuse above this drift (the slowest probe
/// moves at ~3.4 K/min actual, so 2 K/min of drift is already poison)
pub const MAX_DRIFT_KMIN: f64 = 2.0;

/// Spawn the calibration leg. Always sends exactly one
/// [`CalibMsg::Done`] at the end (unless the receiver is gone).
pub fn spawn(
    leg: LegPlan,
    requests: Sender<Request>,
    msg: Sender<CalibMsg>,
    abort: std::sync::Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("rate-calibration".into())
        .spawn(move || {
            let outcome = run_leg(&leg, &requests, &msg, &abort);
            let _ = msg.send(CalibMsg::Done(outcome));
        })
        .expect("failed to spawn calibration thread")
}

// one synchronous request to the device worker (same pattern as the
// schedule runner's `stable` step)
fn ask(requests: &Sender<Request>, cmd: DeviceCmd) -> DeviceReply {
    let (tx, rx) = std::sync::mpsc::channel();
    if requests.send(Request { cmd, reply: tx }).is_err() {
        return DeviceReply::Error("device worker is gone".into());
    }
    rx.recv().unwrap_or(DeviceReply::Error("no reply from worker".into()))
}

fn send_set(requests: &Sender<Request>, cmd: DeviceCmd) -> Result<(), String> {
    match ask(requests, cmd) {
        DeviceReply::Nak(e) | DeviceReply::Error(e) => Err(e),
        _ => Ok(()),
    }
}

fn run_leg(
    leg: &LegPlan,
    requests: &Sender<Request>,
    msg: &Sender<CalibMsg>,
    abort: &std::sync::Arc<AtomicBool>,
) -> Result<f64, String> {
    let say = |s: String| {
        let _ = msg.send(CalibMsg::Status(s));
    };

    // fresh reading: the leg starts wherever the plant actually is now.
    // The leg MUST heat — with loops off the plant just drifts toward
    // ambient and the fit would measure that drift as a "ramp".
    let snap = match ask(requests, DeviceCmd::Poll) {
        DeviceReply::Snapshot(s) => s,
        _ => return Err("cannot read the instrument".into()),
    };
    if snap.control_on == Some(false) {
        return Err(
            "loops are OFF — engage control before calibrating (the leg \
             must heat)"
                .into(),
        );
    }
    let t0 = snap
        .t_a
        .ok_or_else(|| "no live temperature (channel A)".to_string())?;

    // settling check: a leg anchored mid-plunge (or mid-glide) measures
    // momentum, not the ramp. Watch a few seconds and refuse while the
    // temperature is still moving fast — with the number, so the user
    // knows what "wait" means.
    {
        say("checking that the temperature is settled...".into());
        let watch = Instant::now();
        let mut pts: Vec<(f64, f64)> = Vec::new();
        for _ in 0..5 {
            std::thread::sleep(Duration::from_secs(1));
            match ask(requests, DeviceCmd::Poll) {
                DeviceReply::Snapshot(s) => {
                    if s.control_on == Some(false) {
                        return Err("control went OFF — calibration abandoned".into());
                    }
                    if let Some(t) = s.t_a {
                        pts.push((watch.elapsed().as_secs_f64(), t));
                    }
                }
                _ => return Err("cannot read the instrument".into()),
            }
        }
        if let Some((slope, _)) = fit_slope(&pts) {
            let drift = slope * 60.0;
            if drift.abs() > MAX_DRIFT_KMIN {
                return Err(format!(
                    "temperature is moving at {drift:+.1} K/min — let it \
                     settle near a setpoint (or STOP) before calibrating"
                ));
            }
        }
    }
    // keep the planned span, re-anchored (and re-clamped) to the fresh
    // start temperature
    let delta = leg.leg_target - leg.t_start;
    let target = (t0 + delta).min(leg.max_sp - 0.5);
    if target - t0 < 3.0 {
        return Err("not enough headroom below the max setpoint".into());
    }

    // ---- out leg ----------------------------------------------------
    say(format!(
        "out leg: {t0:.1} -> {target:.1} K at commanded {:.3} K/min",
        leg.rate_cmd
    ));
    send_set(requests, DeviceCmd::SetRate { loop_n: 1, value_k_per_min: leg.rate_cmd })?;
    send_set(requests, DeviceCmd::SetLoopType { loop_n: 1, typ: "RAMPP".into() })?;
    send_set(requests, DeviceCmd::SetSetpoint { loop_n: 1, value: target })?;

    let started = Instant::now();
    let mut pts: Vec<(f64, f64)> = Vec::new(); // (elapsed s, temperature K)
    let mut saturated = false;
    let mut last_note = Instant::now();
    // set exactly once at the loop's break, returned after the back leg
    let outcome: Result<f64, String>;
    loop {
        if abort.load(Ordering::Relaxed) {
            let _ = ask(requests, DeviceCmd::Stop);
            return Err("aborted by user (heaters stopped)".into());
        }
        std::thread::sleep(Duration::from_secs(1));
        let (t, p, ctl) = match ask(requests, DeviceCmd::Poll) {
            DeviceReply::Snapshot(s) => match (s.t_a, s.power_1, s.control_on) {
                (Some(t), Some(p), ctl) => (t, p, ctl),
                _ => return Err("lost the temperature/power reading mid-leg".into()),
            },
            other => return Err(format!("poll failed: {other:?}")),
        };
        if ctl == Some(false) {
            // someone hit STOP mid-leg: there is nothing to glide back
            // with, so just give up honestly
            return Err("control went OFF mid-leg — calibration abandoned".into());
        }
        let el = started.elapsed().as_secs_f64();
        pts.push((el, t));
        if last_note.elapsed().as_secs() >= 10 {
            say(format!("out leg {el:.0} s: {t:.2} K, heater {p:.0}%"));
            last_note = Instant::now();
        }
        if p >= 99.0 {
            // the heater pegged: the slope is capability-limited, not
            // firmware-limited — calibrating makes no sense here
            saturated = true;
        }
        // fit candidates: samples after the initial transient
        let fitpts: Vec<(f64, f64)> = pts
            .iter()
            .filter(|(x, _)| *x >= TRANSIENT_S)
            .copied()
            .collect();
        // the span covered is the quality criterion — a fast probe
        // covers it in seconds, a slow one takes its time
        let span_ok = fitpts.len() >= 3
            && fitpts.last().unwrap().1 - fitpts.first().unwrap().1 >= MIN_FIT_DT_K;
        if saturated && el > TRANSIENT_S + 5.0 {
            outcome = Err(format!(
                "heater saturated at {p:.0}% — the commanded rate exceeds \
                 what this temperature band can do; no calibration possible"
            ));
            break;
        }
        if fitpts.len() >= MIN_FIT_PTS && span_ok {
            outcome = match fit_slope(&fitpts) {
                Some((slope_per_s, r2)) if r2 > 0.9 && slope_per_s > 0.005 => {
                    let scale = slope_per_s * 60.0 / leg.rate_cmd;
                    if (0.3..=1.3).contains(&scale) {
                        say(format!(
                            "fit: {:.3} K/min actual vs {:.3} commanded (R² {r2:.3})",
                            slope_per_s * 60.0,
                            leg.rate_cmd
                        ));
                        Ok(scale)
                    } else {
                        Err(format!(
                            "measured scale x{scale:.3} is not plausible — refusing it"
                        ))
                    }
                }
                Some((_, r2)) => Err(format!("slope fit unreliable (R² {r2:.3})")),
                None => Err("not enough points for a slope fit".into()),
            };
            break;
        }
        if t >= target - 0.3 {
            outcome = Err(
                "reached the leg top before enough slope data — lower the \
                 rate or raise the max setpoint"
                    .into(),
            );
            break;
        }
        if el > OUT_TIMEOUT_S {
            outcome = Err("out leg timed out".into());
            break;
        }
    }

    // ---- back leg: glide home regardless of the out leg's outcome ----
    say(format!("back leg: gliding {target:.1} -> {t0:.1} K"));
    let _ = send_set(
        requests,
        DeviceCmd::SetRate { loop_n: 1, value_k_per_min: leg.return_rate },
    );
    let _ = send_set(requests, DeviceCmd::SetSetpoint { loop_n: 1, value: t0 });
    let back = Instant::now();
    loop {
        if abort.load(Ordering::Relaxed) {
            let _ = ask(requests, DeviceCmd::Stop);
            return Err("aborted during the back leg (heaters stopped)".into());
        }
        std::thread::sleep(Duration::from_secs(2));
        if let DeviceReply::Snapshot(s) = ask(requests, DeviceCmd::Poll) {
            if let Some(t) = s.t_a {
                if (t - t0).abs() <= 0.8 {
                    say(format!("back at {t0:.1} K"));
                    break;
                }
                if last_note.elapsed().as_secs() >= 15 {
                    say(format!("back leg {:.0} s: {t:.2} K", back.elapsed().as_secs()));
                    last_note = Instant::now();
                }
            }
        }
        // a missed poll during a passive glide is not fatal
        if back.elapsed().as_secs() > BACK_TIMEOUT_S {
            say("back leg timed out — still gliding home; check the chart".into());
            break;
        }
    }
    outcome
}

// --------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leg_span_follows_rate_with_clamps() {
        // 6 K/min probes at 6: 0.84*6*2 = 10.08 K of span
        let leg = plan_leg(6.0, 200.0, 300.0).unwrap();
        assert!((leg.leg_target - 200.0 - 10.08).abs() < 0.01);
        assert_eq!(leg.return_rate, 12.0);
        // slow plans probe at the 4 K/min floor: 0.84*4*2 = 6.72 K
        let leg = plan_leg(1.0, 200.0, 300.0).unwrap();
        assert!((leg.leg_target - 206.72).abs() < 0.01);
        assert_eq!(leg.rate_cmd, 4.0);
        assert_eq!(leg.rate_plan, 1.0);
        // violent plans probe at the 12 K/min ceiling — never AT the
        // plan rate, or the leg would outrun its own span
        let leg = plan_leg(57.0, 200.0, 300.0).unwrap();
        assert_eq!(leg.rate_cmd, 12.0);
        assert_eq!(leg.rate_plan, 57.0);
        assert!((leg.leg_target - 215.0).abs() < 0.01); // span capped at 15 K
        assert_eq!(leg.return_rate, 24.0);
    }

    #[test]
    fn leg_needs_headroom_below_the_max_setpoint() {
        assert!(plan_leg(6.0, 250.0, 300.0).is_some());
        assert!(plan_leg(6.0, 297.5, 300.0).is_none());
        assert!(plan_leg(-1.0, 250.0, 300.0).is_none());
    }

    #[test]
    fn slope_fit_recovers_a_known_line() {
        let pts: Vec<(f64, f64)> = (0..30).map(|i| (i as f64, 100.0 + 0.1 * i as f64)).collect();
        let (slope, r2) = fit_slope(&pts).unwrap();
        assert!((slope - 0.1).abs() < 1e-9);
        assert!(r2 > 0.999);
        assert!(fit_slope(&[(0.0, 1.0), (1.0, 2.0)]).is_none()); // too few
        assert!(fit_slope(&[(0.0, 1.0); 5]).is_none()); // no x-span
    }

    #[test]
    fn heating_marks_only_heating_legs() {
        use crate::schedule::parse;
        // heat 200 -> 250 (rate applies), then cool 250 -> 150 (no mark)
        let steps = parse("rate 1 6\nset 1 250 RampP\nrate 1 3\nset 1 150 RampP\n")
            .unwrap();
        let (marks, stored) = heating_rate_marks(&steps, 200.0);
        assert_eq!(marks, vec![0]);
        assert!(!stored);
        // a heating set with no preceding rate rides the stored rate
        let steps = parse("set 1 250 RampP\n").unwrap();
        let (marks, stored) = heating_rate_marks(&steps, 200.0);
        assert!(marks.is_empty());
        assert!(stored);
    }

    #[test]
    fn correcting_divides_marked_rates_only() {
        use crate::schedule::parse;
        let mut steps = parse("rate 1 6\nset 1 250 RampP\nrate 1 3\nset 1 150 RampP\n")
            .unwrap();
        let (marks, _) = heating_rate_marks(&steps, 200.0);
        let notes = correct_rates(&mut steps, &marks, 0.84);
        assert_eq!(notes.len(), 1);
        match &steps[0] {
            Step::Rate { value_k_per_min, .. } => {
                assert!((value_k_per_min - 6.0 / 0.84).abs() < 1e-9)
            }
            _ => panic!("not a rate step"),
        }
        match &steps[2] {
            Step::Rate { value_k_per_min, .. } => assert!((value_k_per_min - 3.0).abs() < 1e-9),
            _ => panic!("not a rate step"),
        }
        // a scale of 1.0 is a no-op (no notes, no changes)
        let mut steps = parse("rate 1 6\nset 1 250 RampP\n").unwrap();
        let (marks, _) = heating_rate_marks(&steps, 200.0);
        assert!(correct_rates(&mut steps, &marks, 1.0).is_empty());
    }
}
