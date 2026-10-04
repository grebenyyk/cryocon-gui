//! The rate-calibration gate: pops up before any run that heats at a
//! commanded rate, offering to measure this unit's actual/commanded
//! scale with a short out-and-back leg and apply it to the planned
//! rates (see [`crate::rate_calib`]). Cooling-only runs never gate.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Instant;

use super::{CryoApp, BLUE, INK2, MUTED, RED};
use crate::rate_calib::{self, CalibMsg, LegPlan};
use crate::schedule::Step;

/// A run waiting behind the calibration gate.
#[derive(Clone, Debug)]
pub enum PendingRun {
    Schedule { steps: Vec<Step> },
    Quick { rate: f64, target: f64 },
}

/// How long a calibration stays "fresh" enough to reuse.
const CALIB_FRESH_MIN: u64 = 60;

pub enum RateGate {
    /// normal case: a leg is possible, awaiting the user's choice
    Proposal { run: PendingRun, leg: LegPlan },
    /// not enough heating headroom to calibrate — only skip/cancel
    NoHeadroom { run: PendingRun, why: String },
    /// leg in flight; status lines arrive on the channel
    Running {
        run: PendingRun,
        rx: Receiver<CalibMsg>,
        abort: std::sync::Arc<AtomicBool>,
        status: Vec<String>,
        started: Instant,
    },
    /// leg finished badly — offer to run uncorrected
    Failed { run: PendingRun, why: String },
}

impl CryoApp {
    /// Entry point for every real run (schedule or quick ramp). Decides
    /// whether a heating rate needs calibrating; otherwise starts at once.
    pub fn gate_run(&mut self, run: PendingRun) {
        if self.rate_gate.is_some() {
            self.console_push(
                "ERROR: a rate decision is already pending — answer the \
                 calibration window first"
                    .into(),
            );
            return;
        }
        // telling heating from cooling needs a live temperature
        let t_now = match self.snap.t_a {
            Some(t) => t,
            None => {
                self.console_push(
                    "note: no live temperature — running uncorrected \
                     (rate calibration skipped)"
                        .into(),
                );
                self.launch_run(run, 1.0);
                return;
            }
        };
        // does this run heat at a commanded rate at all?
        let first_rate = match &run {
            PendingRun::Quick { rate, target } => {
                if *target > t_now + 0.5 {
                    Some(*rate)
                } else {
                    None // cooling leg: never gated
                }
            }
            PendingRun::Schedule { steps } => {
                let (marks, stored_heat) = rate_calib::heating_rate_marks(steps, t_now);
                if stored_heat {
                    self.console_push(
                        "note: a heating 'set' has no preceding 'rate' step — \
                         it rides the instrument's stored rate and cannot be \
                         corrected"
                            .into(),
                    );
                }
                marks.first().and_then(|&i| rate_of(&steps[i]))
            }
        };
        match first_rate {
            Some(r) => match rate_calib::plan_leg(r, t_now, self.max_setpoint) {
                Some(leg) => self.rate_gate = Some(RateGate::Proposal { run, leg }),
                None => {
                    self.rate_gate = Some(RateGate::NoHeadroom {
                        run,
                        why: format!(
                            "only {:.1} K of heating headroom below the max \
                             setpoint {} K — the out leg needs at least 3 K",
                            self.max_setpoint - 1.0 - t_now,
                            self.max_setpoint
                        ),
                    });
                }
            },
            None => self.launch_run(run, 1.0), // nothing to calibrate
        }
    }

    /// Launch a run, dividing its heating rates by `scale`.
    fn launch_run(&mut self, run: PendingRun, scale: f64) {
        match run {
            PendingRun::Quick { rate, target } => {
                let cmd = rate / scale;
                if self.control_on != Some(true) {
                    self.console_push(
                        "note: loops are OFF — setpoint stored, but nothing \
                         heats until 'control on'"
                            .into(),
                    );
                }
                if (cmd - rate).abs() > 0.001 {
                    self.console_push(format!(
                        "calibration x{scale:.3}: rate {rate:.3} -> commanded \
                         {cmd:.3} K/min"
                    ));
                }
                self.console_push(format!(
                    "quick ramp: loop1 -> {target} K (RampP) at commanded \
                     {cmd:.3} K/min"
                ));
                let steps = vec![
                    Step::Rate { loop_n: 1, value_k_per_min: cmd },
                    Step::Set { loop_n: 1, value: target, typ: Some("RampP".into()) },
                ];
                self.start_steps(steps);
            }
            PendingRun::Schedule { mut steps } => {
                // recompute the marks at launch time: the temperature may
                // have moved during the calibration leg
                let t = self.snap.t_a.unwrap_or(0.0);
                let (marks, _) = rate_calib::heating_rate_marks(&steps, t);
                for note in rate_calib::correct_rates(&mut steps, &marks, scale) {
                    self.console_push(note.clone());
                    self.csv_row(&note);
                }
                if self.start_steps(steps) {
                    self.console_push("schedule starting".into());
                }
            }
        }
    }

    /// Pump calibration messages, then render the popup. Called every
    /// frame from `App::ui`.
    pub fn rate_gate_ui(&mut self, ctx: &egui::Context) {
        self.pump_rate_gate();

        // NB: `RateGate` cannot derive Clone (it owns a Receiver), so the
        // windows below only *borrow* the state; whatever a button asks
        // for is queued here and applied after rendering, once the
        // borrow has ended.
        enum GateAction {
            Calibrate { run: PendingRun, leg: LegPlan },
            UseLast { run: PendingRun, scale: f64 },
            Uncorrected { run: PendingRun },
            Cancel,
        }
        let mut action: Option<GateAction> = None;

        // a fresh-enough earlier measurement can be reused (Copy data)
        let last = self
            .last_calib
            .filter(|(_, _, when)| when.elapsed().as_secs() / 60 <= CALIB_FRESH_MIN);

        match &self.rate_gate {
            None => {}
            Some(RateGate::Proposal { run, leg }) => {
                egui::Window::new("rate calibration")
                    .collapsible(false)
                    .resizable(false)
                    .show(ctx, |ui| {
                        ui.label(run_summary(run, leg.t_start));
                        ui.label(
                            egui::RichText::new(
                                "this unit's firmware ramps at ~0.84x the commanded \
                                 rate — measure it now and correct the planned rates?",
                            )
                            .small()
                            .color(INK2),
                        );
                        ui.separator();
                        ui.label(format!(
                            "out-and-back: {:.1} -> {:.1} K at {:.2} K/min \
                             commanded, then glide back ({:.0} K span)",
                            leg.t_start,
                            leg.leg_target,
                            leg.rate_cmd,
                            leg.leg_target - leg.t_start
                        ));
                        if (leg.rate_cmd - leg.rate_plan).abs() > 0.01 {
                            ui.label(
                                egui::RichText::new(format!(
                                    "probing at {:.2} K/min, not the plan's \
                                     {:.2} — the scale is rate-independent \
                                     (measured across 3-12 K/min on this unit)",
                                    leg.rate_cmd, leg.rate_plan
                                ))
                                .small()
                                .color(MUTED),
                            );
                        }
                        if let Some((scale, at_k, when)) = last {
                            ui.label(
                                egui::RichText::new(format!(
                                    "last calibration: x{scale:.3} at {at_k:.0} K, \
                                     {} min ago",
                                    when.elapsed().as_secs() / 60
                                ))
                                .small()
                                .color(MUTED),
                            );
                        }
                        ui.separator();
                        ui.horizontal(|ui| {
                            let cal = ui.add_sized(
                                [150.0, 26.0],
                                egui::Button::new(
                                    egui::RichText::new("calibrate & run")
                                        .strong()
                                        .color(egui::Color32::WHITE),
                                )
                                .fill(BLUE),
                            );
                            if cal.clicked() {
                                action = Some(GateAction::Calibrate {
                                    run: run.clone(),
                                    leg: leg.clone(),
                                });
                            }
                            if let Some((scale, _, _)) = last {
                                if ui.button(format!("use last (x{scale:.3})")).clicked() {
                                    action = Some(GateAction::UseLast {
                                        run: run.clone(),
                                        scale,
                                    });
                                }
                            }
                            if ui.button("run uncorrected").clicked() {
                                action = Some(GateAction::Uncorrected { run: run.clone() });
                            }
                            if ui.button("cancel").clicked() {
                                action = Some(GateAction::Cancel);
                            }
                        });
                    });
            }
            Some(RateGate::NoHeadroom { run, why }) | Some(RateGate::Failed { run, why }) => {
                let title = if matches!(self.rate_gate, Some(RateGate::Failed { .. })) {
                    "rate calibration failed"
                } else {
                    "rate calibration"
                };
                let summary = run_summary(run, self.snap.t_a.unwrap_or(0.0));
                let mut choice = None;
                egui::Window::new(title)
                    .collapsible(false)
                    .resizable(false)
                    .show(ctx, |ui| {
                        ui.label(summary);
                        ui.label(egui::RichText::new(why).color(RED));
                        ui.separator();
                        ui.horizontal(|ui| {
                            if ui.button("run uncorrected").clicked() {
                                choice = Some(true);
                            }
                            if ui.button("cancel").clicked() {
                                choice = Some(false);
                            }
                        });
                    });
                match choice {
                    Some(true) => {
                        action = Some(GateAction::Uncorrected { run: run.clone() })
                    }
                    Some(false) => action = Some(GateAction::Cancel),
                    None => {}
                }
            }
            Some(RateGate::Running { status, started, abort, .. }) => {
                let abort = abort.clone();
                egui::Window::new("rate calibration — running")
                    .collapsible(false)
                    .resizable(false)
                    .show(ctx, |ui| {
                        let el = started.elapsed().as_secs();
                        ui.label(format!(
                            "measuring the actual ramp rate ({}:{:02} elapsed)",
                            el / 60,
                            el % 60
                        ));
                        egui::ScrollArea::vertical()
                            .max_height(140.0)
                            .stick_to_bottom(true)
                            .show(ui, |ui| {
                                for line in status {
                                    ui.monospace(egui::RichText::new(line).small());
                                }
                            });
                        if ui
                            .button(egui::RichText::new("abort calibration").color(RED))
                            .clicked()
                        {
                            // an Arc — no `self` mutation needed
                            abort.store(true, Ordering::Relaxed);
                        }
                    });
            }
        }

        // apply the queued action now that nothing borrows `self`
        match action {
            Some(GateAction::Calibrate { run, leg }) => self.start_calibration(run, leg),
            Some(GateAction::UseLast { run, scale }) => {
                self.rate_gate = None;
                self.console_push(format!(
                    "using calibration x{scale:.3} from earlier this session"
                ));
                self.launch_run(run, scale);
            }
            Some(GateAction::Uncorrected { run }) => {
                self.rate_gate = None;
                self.launch_run(run, 1.0);
            }
            Some(GateAction::Cancel) => self.rate_gate = None,
            None => {}
        }
    }

    /// Kick off the calibration thread and switch the gate to Running.
    fn start_calibration(&mut self, run: PendingRun, leg: LegPlan) {
        // the leg must heat — with loops off the plant drifts toward
        // ambient and the fit would measure that drift (checked in the
        // thread too, but failing here is instant)
        if self.control_on != Some(true) {
            let why =
                "loops are OFF — engage control before calibrating (the leg must heat)"
                    .to_string();
            self.console_push(format!("rate calibration failed: {why}"));
            self.rate_gate = Some(RateGate::Failed { run, why });
            return;
        }
        let req_tx = match &self.link {
            Some(link) => link.req_tx.clone(),
            None => {
                self.console_push("ERROR: not connected".into());
                return;
            }
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let abort = std::sync::Arc::new(AtomicBool::new(false));
        rate_calib::spawn(leg, req_tx, tx, abort.clone());
        self.console_push("rate calibration starting (out-and-back leg)".into());
        self.rate_gate = Some(RateGate::Running {
            run,
            rx,
            abort,
            status: Vec::new(),
            started: Instant::now(),
        });
    }

    /// Receive calibration messages into UI state; a `Done` either
    /// launches the run (with the measured scale) or switches to Failed.
    fn pump_rate_gate(&mut self) {
        // NB: receive into owned values first — the borrow of the channel
        // must end before `self` is mutated (a first Rust lesson).
        let mut lines = Vec::new();
        let mut done: Option<Result<f64, String>> = None;
        if let Some(RateGate::Running { rx, .. }) = &self.rate_gate {
            loop {
                match rx.try_recv() {
                    Ok(CalibMsg::Status(line)) => lines.push(line),
                    Ok(CalibMsg::Done(res)) => {
                        done = Some(res);
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        done = Some(Err("calibration thread ended unexpectedly".into()));
                        break;
                    }
                }
            }
        }
        if !lines.is_empty() {
            // echo to the console too — the popup is ephemeral, and the
            // console is where post-mortems happen
            for line in &lines {
                self.console_push(line.clone());
            }
            if let Some(RateGate::Running { status, .. }) = &mut self.rate_gate {
                status.extend(lines);
                if status.len() > 40 {
                    status.drain(..status.len() - 40);
                }
            }
        }
        if let Some(res) = done {
            if let Some(RateGate::Running { run, .. }) = self.rate_gate.take() {
                match res {
                    Ok(scale) => {
                        self.last_calib = Some((
                            scale,
                            self.snap.t_a.unwrap_or(0.0),
                            Instant::now(),
                        ));
                        let msg = format!(
                            "rate calibration: x{scale:.3} (actual/commanded) — \
                             planned heating rates divided by it"
                        );
                        self.console_push(msg.clone());
                        self.csv_row(&msg);
                        self.launch_run(run, scale);
                    }
                    Err(why) => {
                        self.console_push(format!("rate calibration failed: {why}"));
                        self.rate_gate = Some(RateGate::Failed { run, why });
                    }
                }
            }
        }
    }
}

fn rate_of(step: &Step) -> Option<f64> {
    match step {
        Step::Rate { value_k_per_min, .. } => Some(*value_k_per_min),
        _ => None,
    }
}

fn run_summary(run: &PendingRun, t_now: f64) -> String {
    match run {
        PendingRun::Quick { rate, target } => {
            format!("quick ramp {t_now:.1} -> {target} K at {rate} K/min")
        }
        PendingRun::Schedule { steps } => {
            let (marks, _) = rate_calib::heating_rate_marks(steps, t_now);
            let first = marks.first().and_then(|&i| rate_of(&steps[i]));
            format!(
                "schedule with {} heating rate step(s){}",
                marks.len(),
                first
                    .map(|r| format!(", first at {r} K/min"))
                    .unwrap_or_default()
            )
        }
    }
}
