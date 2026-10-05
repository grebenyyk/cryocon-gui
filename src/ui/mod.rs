//! The application: state, top bar, connection banner, and the eframe
//! glue. Panel contents live in the sibling modules ([`live`],
//! [`schedule_ui`]) as extra `impl CryoApp` blocks.

pub mod live;
pub mod rate_gate;
pub mod schedule_ui;

use rate_gate::RateGate;

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};

use crate::device::{DeviceCmd, DeviceEvent, Request, Snapshot, Worker};
use crate::logging::CsvLog;
use crate::schedule::{self, Progress};

/// Palette (light) — kept in one place so the panels stay consistent.
pub const BLUE: egui::Color32 = egui::Color32::from_rgb(0x2a, 0x78, 0xd6);
pub const ORANGE: egui::Color32 = egui::Color32::from_rgb(0xeb, 0x68, 0x34);
pub const MUTED: egui::Color32 = egui::Color32::from_rgb(0x89, 0x87, 0x81);
pub const INK2: egui::Color32 = egui::Color32::from_rgb(0x52, 0x51, 0x4e);
pub const RED: egui::Color32 = egui::Color32::from_rgb(0xd0, 0x3b, 0x3b);
pub const GREEN: egui::Color32 = egui::Color32::from_rgb(0x0c, 0xa3, 0x0c);

/// Plain Return pressed this frame — the dialog-confirm counterpart to
/// Esc-cancel. Unmodified only, so ⌘Return in the schedule editor keeps
/// meaning "run" even while a dialog is open.
pub(crate) fn return_pressed(ui: &mut egui::Ui) -> bool {
    ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.is_none())
}

/// One point of chart history.
pub struct Sample {
    pub t_min: f64,
    pub t_a: Option<f64>,
    pub setpoint: Option<f64>,
    pub power: Option<f64>,
}

#[derive(Default)]
pub enum LinkState {
    #[default]
    Off,
    /// worker is alive but the socket is down; it retries every 2 s
    Connecting,
    On { idn: String },
}

/// Handle to the running device worker.
pub struct Link {
    pub req_tx: Sender<Request>,
    pub ev_rx: Receiver<DeviceEvent>,
}

pub struct CryoApp {
    // connection
    pub host: String,
    pub port: String, // kept as text for the edit field
    pub link: Option<Link>,
    pub link_state: LinkState,

    // live data
    pub snap: Snapshot,
    pub history: Vec<Sample>,
    pub t0: std::time::Instant,

    // logging
    pub logging_enabled: bool,
    pub log_dir: PathBuf,
    pub csv: Option<CsvLog>,

    // schedule
    pub schedule_text: String,
    pub runner: Option<schedule::RunnerHandle>,
    pub prog_rx: Option<Receiver<Progress>>,
    pub console: Vec<String>,

    /// Rate-calibration gate: set while a rate-bearing run waits for the
    /// user's choice or while the calibration leg is in flight
    /// (see `rate_gate.rs`).
    pub rate_gate: Option<RateGate>,
    /// Last successful calibration: (scale, K where measured, when).
    pub last_calib: Option<(f64, f64, std::time::Instant)>,

    // safety
    pub max_setpoint: f64,
    pub jump_confirm_k: f64,
    /// A set command going through confirmation. Two stages, in order:
    /// first decide what happens to a running schedule, then (only if the
    /// jump is big) confirm the jump itself.
    pub pending_set: Option<PendingSet>,
    /// STOP ALL / disconnect finalizes the log; polls must not silently
    /// open a fresh file afterwards.
    pub log_finalized: bool,
    /// Loop control state as last commanded *by this app* (None = unknown,
    /// e.g. right after connecting). The instrument has no safe query for
    /// it, so we track our own commands; after `STOP` setpoints are stored
    /// but nothing heats until `CONTROL` re-engages the loops.
    pub control_on: Option<bool>,

    // manual set controls
    pub manual_setpoint: String,
    pub manual_type: usize, // index into TYPES

    // quick ramp panels (left pane, under 'set setpoint')
    pub quick_target_time: String,
    pub quick_time_min: String,
    pub quick_target_rate: String,
    pub quick_rate: String,
}

pub const TYPES: [&str; 7] = ["PID", "RampP", "RampT", "Man", "Off", "Table", "SCALE"];

/// Staged confirmation for a manual set (see `CryoApp::pending_set`).
#[derive(Clone, Debug)]
pub enum PendingSet {
    /// a schedule is running: decide its fate first
    ScheduleChoice { loop_n: u8, value: f64, typ: Option<String> },
    /// big jump: confirm it (carrying the schedule decision, if any)
    JumpConfirm { loop_n: u8, value: f64, typ: Option<String>, abort_schedule: bool },
}

impl CryoApp {
    pub fn new() -> Self {
        Self {
            host: "192.168.1.5".into(),
            port: "5000".into(),
            link: None,
            link_state: LinkState::Off,
            snap: Snapshot::default(),
            history: Vec::new(),
            t0: std::time::Instant::now(),
            logging_enabled: true,
            log_dir: dirs_documents().join("cryocon_logs"),
            csv: None,
            schedule_text: schedule::TEMPLATE.into(),
            runner: None,
            prog_rx: None,
            console: vec!["welcome — connect to the instrument or the mock".into()],
            rate_gate: None,
            last_calib: None,
            max_setpoint: 300.0,
            jump_confirm_k: 10.0,
            pending_set: None,
            log_finalized: false,
            control_on: None,
            manual_setpoint: "298".into(),
            manual_type: 0,
            quick_target_time: "298".into(),
            quick_time_min: "30".into(),
            quick_target_rate: "298".into(),
            quick_rate: "6".into(),
        }
    }

    fn console_push(&mut self, line: String) {
        self.console.push(line);
        let cap = 400;
        if self.console.len() > cap {
            self.console.drain(..self.console.len() - cap);
        }
    }

    /// Fire one request at the worker and wait briefly for its reply.
    fn ask(&self, cmd: DeviceCmd) -> Option<crate::device::DeviceReply> {
        let link = self.link.as_ref()?;
        let (tx, rx) = std::sync::mpsc::channel();
        link.req_tx
            .send(Request { cmd, reply: tx })
            .ok()?;
        rx.recv_timeout(std::time::Duration::from_secs(3)).ok()
    }

    fn connect(&mut self) {
        let port: u16 = match self.port.parse() {
            Ok(p) => p,
            Err(_) => {
                self.console_push("ERROR: port is not a number".into());
                return;
            }
        };
        // Ask where this session's log should live — every session, so the
        // choice can never be a forgotten stale default.
        if self.logging_enabled {
            match rfd::FileDialog::new()
                .set_title("folder for this session's CSV log")
                .set_directory(&self.log_dir)
                .pick_folder()
            {
                Some(dir) => {
                    self.log_dir = dir;
                    self.csv = None; // a fresh file is created in that folder
                    self.console_push(format!(
                        "log folder for this session: {}",
                        self.log_dir.display()
                    ));
                }
                None => {
                    self.logging_enabled = false;
                    self.console_push(
                        "no folder chosen — CSV logging is OFF for this session \
                         (re-enable via the checkbox + folder… button)"
                            .into(),
                    );
                }
            }
        }
        let (req_tx, req_rx) = std::sync::mpsc::channel();
        let (ev_tx, ev_rx) = std::sync::mpsc::channel();
        Worker::spawn(self.host.clone(), port, req_rx, ev_tx);
        self.link = Some(Link { req_tx, ev_rx });
        self.link_state = LinkState::Connecting;
        self.history.clear();
        self.t0 = std::time::Instant::now();
        self.log_finalized = false; // new session, new log allowed
        self.control_on = None; // unknown until we command it
        self.console_push(format!("connecting to {}:{} ...", self.host, port));
    }

    /// Engage or disengage loop control (the instrument's CONTROL / STOP).
    /// A stored setpoint only heats once control is ON.
    pub fn set_control(&mut self, on: bool) {
        let reply = self.ask(if on { DeviceCmd::Control } else { DeviceCmd::Stop });
        let ok = matches!(reply, Some(crate::device::DeviceReply::Ok));
        self.control_on = Some(on).filter(|_| ok);
        let event = if on { "control on" } else { "control off" };
        self.console_push(if ok {
            event.to_string()
        } else {
            format!("ERROR: {event} command failed")
        });
        if ok {
            self.csv_row(&event);
            // turning control on with a far-away stored setpoint means
            // "drive there NOW" — say so before the chart does
            if on {
                if let (Some(t), Some(sp)) = (self.snap.t_a, self.snap.setpoint_1) {
                    if (t - sp).abs() > 5.0 {
                        self.console_push(format!(
                            "note: driving toward the stored setpoint {sp:.1} K \
                             ({:+.1} K from {t:.1} K)",
                            sp - t
                        ));
                    }
                }
            }
        }
    }

    /// The one big red button: heaters off, schedule aborted, log closed.
    /// Reports exactly which of those actually happened.
    fn stop_all(&mut self) {
        let mut parts: Vec<String> = Vec::new();
        if let Some(r) = &self.runner {
            r.abort.store(true, std::sync::atomic::Ordering::Relaxed);
            parts.push("schedule aborted".into());
        }
        // an in-flight calibration leg is also a thing that heats
        if let Some(RateGate::Running { abort, .. }) = &self.rate_gate {
            abort.store(true, std::sync::atomic::Ordering::Relaxed);
            parts.push("calibration aborted".into());
        }
        if self.rate_gate.is_some() {
            self.rate_gate = None; // pending popups close with it
        }
        if self.link.is_some() {
            let _ = self.ask(DeviceCmd::Stop);
            parts.push("heaters stopped".into());
        }
        if let Some(csv) = self.csv.take() {
            // `take()` drops (and closes) the file when this binding dies
            self.console_push(format!("log closed: {}", csv.path.display()));
            self.log_finalized = true; // polls must not open a fresh file
            parts.push("log closed".into());
        }
        if parts.is_empty() {
            self.console_push("STOP ALL — nothing was running".into());
        } else {
            self.console_push(format!("STOP ALL — {}", parts.join(", ")));
        }
        self.control_on = if self.link.is_some() { Some(false) } else { None };
        if self.link.is_some() {
            self.console_push(
                "loops are now OFF: setpoints are stored but will not heat \
                 until 'control on' (left panel) or a schedule 'control' step"
                    .into(),
            );
        }
    }

    fn disconnect(&mut self) {
        if let Some(link) = self.link.take() {
            // best-effort shutdown; the worker exits on receiving this
            let (tx, _rx) = std::sync::mpsc::channel();
            let _ = link.req_tx.send(Request { cmd: DeviceCmd::Shutdown, reply: tx });
        }
        self.csv = None; // close the log file with the session
        self.link_state = LinkState::Off;
        self.console_push("disconnected".into());
    }

    /// Is the jump from the current setpoint big enough to need a confirm?
    fn needs_jump_confirm(&self, value: f64) -> bool {
        self.snap
            .setpoint_1
            .map(|cur| (cur - value).abs() > self.jump_confirm_k)
            .unwrap_or(true)
    }

    /// Send a setpoint change through the safety checks.
    /// Unconfirmed sets land in a staged dialog instead (see `PendingSet`):
    /// schedule decision first, jump confirmation second.
    pub fn do_set(&mut self, loop_n: u8, value: f64, typ: Option<String>, confirmed: bool) {
        let clamped = if value > self.max_setpoint {
            self.console_push(format!(
                "WARNING: {value} clamped to max setpoint {}",
                self.max_setpoint
            ));
            self.max_setpoint
        } else {
            value
        };
        if !confirmed {
            if self.runner.is_some() {
                // stage 1: what happens to the running schedule?
                self.pending_set = Some(PendingSet::ScheduleChoice {
                    loop_n,
                    value: clamped,
                    typ,
                });
                return;
            }
            if self.needs_jump_confirm(clamped) {
                // no schedule involved: straight to the jump confirm
                self.pending_set = Some(PendingSet::JumpConfirm {
                    loop_n,
                    value: clamped,
                    typ,
                    abort_schedule: false,
                });
                return;
            }
        }
        if let Some(t) = &typ {
            let _ = self.ask(DeviceCmd::SetLoopType { loop_n, typ: t.clone() });
        }
        if self.control_on != Some(true) {
            self.console_push(
                "note: loops are OFF — setpoint stored, but nothing heats \
                 until 'control on'"
                    .into(),
            );
        }
        match self.ask(DeviceCmd::SetSetpoint { loop_n, value: clamped }) {
            Some(crate::device::DeviceReply::Ok) => {
                let event = match &typ {
                    Some(t) => format!("set loop{loop_n} -> {clamped} ({t})"),
                    None => format!("set loop{loop_n} -> {clamped}"),
                };
                self.console_push(event.clone());
                self.csv_row(&event);
            }
            Some(crate::device::DeviceReply::Nak(e))
            | Some(crate::device::DeviceReply::Error(e)) => {
                self.console_push(format!("ERROR: setpoint rejected: {e}"));
            }
            None => self.console_push("ERROR: no device connection".into()),
            Some(_) => {} // Snapshot/Text never answer a set command
        }
    }

    /// Write one CSV row (if logging is on), using the latest snapshot.
    /// After STOP ALL / disconnect the session's log is final — a new one
    /// is only started by the next connect, never by a background poll.
    pub fn csv_row(&mut self, event: &str) {
        if !self.logging_enabled || self.log_finalized {
            return;
        }
        if self.csv.is_none() {
            match CsvLog::create(&self.log_dir) {
                Ok(log) => {
                    self.console_push(format!("logging to {}", log.path.display()));
                    self.csv = Some(log);
                }
                Err(e) => {
                    self.console_push(format!("ERROR: cannot open CSV log: {e}"));
                    return;
                }
            }
        }
        if let Some(log) = self.csv.as_mut() {
            log.row(event, &self.snap);
        }
    }

    fn drain_events(&mut self) {
        // NB: receive into an owned value *before* touching `self`, so the
        // borrow of the channel ends each iteration (a first Rust lesson:
        // the borrow checker forbids holding a borrow across a mutation).
        loop {
            let ev = match self.link.as_ref() {
                Some(link) => link.ev_rx.try_recv(),
                None => return,
            };
            match ev {
                Ok(DeviceEvent::Connected(idn)) => {
                    self.link_state = LinkState::On { idn: idn.clone() };
                    self.console_push(format!("connected: {idn}"));
                    // provenance row: a log should record where it came
                    // from. The mock imitates the real IDN down to the
                    // serial, so host:port is the decisive part. Commas
                    // would shift the CSV columns — hence the replace.
                    let event = format!(
                        "connected {}:{} {}",
                        self.host,
                        self.port,
                        idn.replace(',', " ")
                    );
                    self.csv_row(&event);
                }
                Ok(DeviceEvent::Disconnected(reason)) => {
                    self.link_state = LinkState::Connecting;
                    self.console_push(format!("connection lost ({reason}); retrying"));
                }
                Ok(DeviceEvent::Snapshot(s)) => {
                    // the instrument's own CONTROL? answer wins over our
                    // bookkeeping whenever the firmware provides one
                    if let Some(on) = s.control_on {
                        self.control_on = Some(on);
                    }
                    self.snap = s.clone();
                    let t_min = self.t0.elapsed().as_secs_f64() / 60.0;
                    self.history.push(Sample {
                        t_min,
                        t_a: s.t_a,
                        setpoint: s.setpoint_1,
                        power: s.power_1,
                    });
                    // keep ~6 h at 1 Hz
                    if self.history.len() > 21_600 {
                        self.history.drain(..self.history.len() - 21_600);
                    }
                    let event = format!(
                        "poll loop1 T={} sp={}",
                        s.t_a.map(|v| format!("{v:.3}")).unwrap_or_else(|| "?".into()),
                        s.setpoint_1
                            .map(|v| format!("{v:.3}"))
                            .unwrap_or_else(|| "?".into()),
                    );
                    let power = s
                        .power_1
                        .map(|p| format!(" P={p:.0}%"))
                        .unwrap_or_default();
                    self.csv_row(&(event + &power));
                }
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => return,
            }
        }
    }

    fn drain_progress(&mut self) {
        loop {
            let msg = match self.prog_rx.as_ref() {
                Some(rx) => rx.try_recv(),
                None => return,
            };
            match msg {
                Ok(Progress::Line(line)) => {
                    // step headers ("[2/5] set loop1 -> 299") also go to CSV
                    if line.starts_with('[') {
                        if let Some(event) = line.split("] ").nth(1) {
                            // keep our view of the control state in sync with
                            // what the schedule commanded
                            match event {
                                "control ON" => self.control_on = Some(true),
                                "control STOP" => self.control_on = Some(false),
                                _ => {}
                            }
                            self.csv_row(event);
                        }
                    }
                    self.console_push(line);
                }
                Ok(Progress::Finished(ok)) => {
                    self.console_push(if ok {
                        "schedule complete".into()
                    } else {
                        "schedule stopped".into()
                    });
                    self.runner = None;
                    self.prog_rx = None;
                }
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => return,
            }
        }
    }
}

fn dirs_documents() -> PathBuf {
    std::env::var("HOME")
        .map(|h| PathBuf::from(h).join("Documents"))
        .unwrap_or_else(|_| PathBuf::from("."))
}

impl eframe::App for CryoApp {
    // Called by eframe while the event loop is still alive, right before
    // the window/renderer teardown. On this macOS version that teardown
    // aborts inside AppKit (an NSTouchBar display-flush observer throws
    // after the window closes — see the crash reports), which macOS then
    // reports as "quit unexpectedly". Exiting here, before any of that
    // runs, is a clean quit: nothing is lost (CSV rows are flushed as
    // they are written) and the heaters' state is the instrument's own.
    fn on_exit(&mut self) {
        // stop anything this app is driving, then leave without handing
        // control back to the crashing teardown
        if let Some(r) = &self.runner {
            r.abort.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        if let Some(RateGate::Running { abort, .. }) = &self.rate_gate {
            abort.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.csv = None; // close + flush the session log
        std::process::exit(0);
    }

    // eframe 0.36 hands the app a root Ui instead of a bare Context
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.drain_events();
        self.drain_progress();

        self.top_bar(ui);
        self.banner(ui);
        // egui 0.36 merged Side/TopBottom panels into one `Panel` type.
        // Four regions, three drag handles:
        //   left   — live panel (its right edge is draggable, as before)
        //   top    — charts   (resizable: handle on its bottom edge)
        //   middle — schedule (fills whatever the neighbours leave it)
        //   bottom — console  (resizable: handle on its top edge)
        // so both boundaries around the schedule drag exactly like the
        // live panel's divider.
        egui::Panel::left("live")
            .default_size(300.0)
            .min_size(220.0)
            .show(ui, |ui| self.live_panel(ui));
        egui::Panel::top("charts")
            .resizable(true) // side panels default to resizable; top/bottom don't
            // defaults sized so the schedule has a strip of its own in the
            // 1200x800 default window: ~40 (top bar) + 365 + 170 ≈ 575,
            // leaving ~225 for the schedule
            .default_size(365.0)
            .min_size(320.0)
            .show(ui, |ui| self.charts(ui));
        egui::Panel::bottom("console")
            .resizable(true)
            .default_size(170.0)
            .min_size(90.0)
            .show(ui, |ui| self.console_panel(ui));
        // the schedule keeps the middle strip: the editor fills it and
        // scrolls internally; validation + buttons pin to its bottom edge
        // (laid out inside schedule_ui.rs)
        egui::CentralPanel::default().show(ui, |ui| {
            self.schedule_section(ui);
        });

        // keep the UI fresh while anything is running
        if self.link.is_some() || self.runner.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(300));
        }

        // staged confirmation: schedule decision FIRST, jump confirm second
        match self.pending_set.clone() {
            Some(PendingSet::ScheduleChoice { loop_n, value, typ }) => {
                egui::Window::new("a schedule is running")
                    .collapsible(false)
                    .resizable(false)
                    .show(&ctx, |ui| {
                        // Esc = cancel, same as the button; Return =
                        // the primary action, "apply — schedule continues"
                        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            self.pending_set = None;
                        }
                        let mut proceed = return_pressed(ui);
                        ui.label(format!(
                            "manual setpoint: {} K{}",
                            value,
                            // as_deref(): borrow the String, keep `typ` usable below
                            typ.as_deref()
                                .map(|t| format!("  (type {t})"))
                                .unwrap_or_default()
                        ));
                        ui.label(
                            egui::RichText::new(
                                "the schedule's next step would take the setpoint back",
                            )
                            .small()
                            .color(INK2),
                        );
                        ui.separator();
                        ui.horizontal(|ui| {
                            if ui.button("apply — schedule continues").clicked() {
                                proceed = true;
                            }
                            if ui
                                .button(egui::RichText::new("apply & abort schedule").color(RED))
                                .clicked()
                            {
                                self.pending_set = if self.needs_jump_confirm(value) {
                                    Some(PendingSet::JumpConfirm {
                                        loop_n,
                                        value,
                                        typ: typ.clone(),
                                        abort_schedule: true,
                                    })
                                } else {
                                    if let Some(r) = &self.runner {
                                        r.abort
                                            .store(true, std::sync::atomic::Ordering::Relaxed);
                                    }
                                    self.do_set(loop_n, value, typ.clone(), true);
                                    None
                                };
                            }
                            if ui.button("cancel").clicked() {
                                self.pending_set = None;
                            }
                        });
                        // shared branch for the button and Return
                        if proceed {
                            self.pending_set = if self.needs_jump_confirm(value) {
                                Some(PendingSet::JumpConfirm {
                                    loop_n,
                                    value,
                                    typ: typ.clone(),
                                    abort_schedule: false,
                                })
                            } else {
                                self.do_set(loop_n, value, typ.clone(), true);
                                None
                            };
                        }
                    });
            }
            Some(PendingSet::JumpConfirm { loop_n, value, typ, abort_schedule }) => {
                egui::Window::new("confirm setpoint jump")
                    .collapsible(false)
                    .resizable(false)
                    .show(&ctx, |ui| {
                        // Esc = cancel, same as the button; Return = confirm
                        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            self.pending_set = None;
                        }
                        let confirm = return_pressed(ui);
                        let current = self.snap.setpoint_1;
                        ui.label(format!(
                            "setpoint: {} -> {value} K{}",
                            current.map(|v| format!("{v:.1}")).unwrap_or_else(|| "?".into()),
                            typ.clone()
                                .map(|t| format!("  (type {t})"))
                                .unwrap_or_default(),
                        ));
                        if let Some(cur) = current {
                            ui.label(format!("jump: {:.1} K", (cur - value).abs()));
                        }
                        if abort_schedule {
                            ui.label(
                                egui::RichText::new("the schedule will be aborted too")
                                    .small()
                                    .color(INK2),
                            );
                        }
                        ui.separator();
                        ui.horizontal(|ui| {
                            if ui.button("confirm").clicked() || confirm {
                                if abort_schedule {
                                    if let Some(r) = &self.runner {
                                        r.abort
                                            .store(true, std::sync::atomic::Ordering::Relaxed);
                                    }
                                }
                                self.pending_set = None;
                                self.do_set(loop_n, value, typ, true);
                            }
                            if ui.button("cancel").clicked() {
                                self.pending_set = None;
                            }
                        });
                    });
            }
            None => {}
        }

        // rate-calibration popup (gates any run that heats at a rate)
        self.rate_gate_ui(&ctx);
    }
}

// top bar + banner live here (small enough to stay in mod.rs)

impl CryoApp {
    fn top_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("top").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let linked = self.link.is_some();
                ui.label("host");
                ui.text_edit_singleline(&mut self.host).on_hover_text(
                    "instrument address; use 127.0.0.1 with the mock's port",
                );
                ui.label("port");
                ui.text_edit_singleline(&mut self.port);
                if ui
                    .small_button("mock")
                    .on_hover_text("offline simulator preset (run mock_cryocon.py first)")
                    .clicked()
                {
                    self.host = "127.0.0.1".into();
                    self.port = "15000".into();
                }
                if linked {
                    if ui.button("disconnect").clicked() {
                        self.disconnect();
                    }
                } else if ui.button("connect").clicked() {
                    self.connect();
                }
                // status lamp
                let (color, note) = match &self.link_state {
                    LinkState::Off => (MUTED, "off".to_string()),
                    LinkState::Connecting => (ORANGE, "connecting…".into()),
                    LinkState::On { idn } => (egui::Color32::from_rgb(0x0c, 0xa3, 0x0c), idn.clone()),
                };
                ui.colored_label(color, "●").on_hover_text(note);
                ui.separator();
                ui.checkbox(&mut self.logging_enabled, "CSV log");
                if ui.button("folder…").clicked() {
                    if let Some(d) = rfd::FileDialog::new()
                        .set_title("log folder for the NEXT session")
                        .set_directory(&self.log_dir)
                        .pick_folder()
                    {
                        self.log_dir = d;
                        match &self.csv {
                            Some(_open_log) => self.console_push(format!(
                                "next session logs to {} (current file keeps going)",
                                self.log_dir.display()
                            )),
                            None => self.console_push(format!(
                                "log folder set: {}",
                                self.log_dir.display()
                            )),
                        }
                    }
                }
                ui.label(
                    egui::RichText::new(self.log_dir.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default())
                        .color(MUTED),
                )
                .on_hover_text(self.log_dir.display().to_string());

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Quiet by default; loud (filled red) exactly when a
                    // schedule is running and stopping actually matters.
                    let hot = self.runner.is_some();
                    let text = egui::RichText::new("STOP ALL")
                        .color(if hot { egui::Color32::WHITE } else { RED });
                    let mut btn = egui::Button::new(text);
                    if hot {
                        btn = btn.fill(RED);
                    }
                    if ui.add_sized([110.0, 28.0], btn).clicked() {
                        self.stop_all();
                    }
                });
            });
            ui.add_space(4.0);
        });
    }

    fn banner(&mut self, ui: &mut egui::Ui) {
        let show = matches!(self.link_state, LinkState::Connecting) && self.link.is_some();
        if !show {
            return;
        }
        egui::Panel::top("banner").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.colored_label(ORANGE, "⚠");
                ui.label(
                    egui::RichText::new("connection lost — the worker retries every 2 s")
                        .color(INK2),
                );
                if ui.small_button("give up").clicked() {
                    self.disconnect();
                }
            });
        });
    }
}
