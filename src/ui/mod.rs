//! The application: state, top bar, connection banner, and the eframe
//! glue. Panel contents live in the sibling modules ([`live`],
//! [`schedule_ui`]) as extra `impl CryoApp` blocks.

pub mod live;
pub mod schedule_ui;

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

    // safety
    pub max_setpoint: f64,
    pub jump_confirm_k: f64,
    /// a set command waiting for the user to confirm (jump > threshold)
    pub pending_set: Option<(u8, f64, Option<String>)>,

    // manual set controls
    pub manual_setpoint: String,
    pub manual_type: usize, // index into TYPES
}

pub const TYPES: [&str; 6] = ["PID", "RampP", "RampT", "Man", "Off", "Table"];

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
            max_setpoint: 300.0,
            jump_confirm_k: 10.0,
            pending_set: None,
            manual_setpoint: "298".into(),
            manual_type: 0,
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
        let (req_tx, req_rx) = std::sync::mpsc::channel();
        let (ev_tx, ev_rx) = std::sync::mpsc::channel();
        Worker::spawn(self.host.clone(), port, req_rx, ev_tx);
        self.link = Some(Link { req_tx, ev_rx });
        self.link_state = LinkState::Connecting;
        self.history.clear();
        self.t0 = std::time::Instant::now();
        self.console_push(format!("connecting to {}:{} ...", self.host, port));
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

    /// Send a setpoint change through the safety checks.
    /// `confirmed` skips the jump dialog (the dialog calls back with true).
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
        let jump = self
            .snap
            .setpoint_1
            .map(|cur| (cur - clamped).abs() > self.jump_confirm_k)
            .unwrap_or(true);
        let stepped = typ.as_deref().map(|t| !t.eq_ignore_ascii_case("RampP")).unwrap_or(true);
        if !confirmed && (jump || stepped) {
            self.pending_set = Some((loop_n, clamped, typ));
            return;
        }
        if let Some(t) = &typ {
            let _ = self.ask(DeviceCmd::SetLoopType { loop_n, typ: t.clone() });
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
    pub fn csv_row(&mut self, event: &str) {
        if !self.logging_enabled {
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
                }
                Ok(DeviceEvent::Disconnected(reason)) => {
                    self.link_state = LinkState::Connecting;
                    self.console_push(format!("connection lost ({reason}); retrying"));
                }
                Ok(DeviceEvent::Snapshot(s)) => {
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
    // eframe 0.36 hands the app a root Ui instead of a bare Context
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.drain_events();
        self.drain_progress();

        self.top_bar(ui);
        self.banner(ui);
        // egui 0.36 merged Side/TopBottom panels into one `Panel` type
        egui::Panel::left("live")
            .default_size(300.0)
            .min_size(220.0)
            .show(ui, |ui| self.live_panel(ui));
        egui::CentralPanel::default().show(ui, |ui| {
            self.charts(ui);
            ui.add_space(6.0);
            self.schedule_section(ui);
        });

        // keep the UI fresh while anything is running
        if self.link.is_some() || self.runner.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(300));
        }

        // modal confirmation for large setpoint jumps
        if let Some((loop_n, value, typ)) = self.pending_set.clone() {
            egui::Window::new("confirm setpoint change")
                .collapsible(false)
                .resizable(false)
                .show(&ctx, |ui| {
                    let current = self.snap.setpoint_1;
                    ui.label(format!(
                        "setpoint: {} -> {value} K{}",
                        current.map(|v| format!("{v:.1}")).unwrap_or_else(|| "?".into()),
                        typ.clone()
                            .map(|t| format!("  (type {t})"))
                            .unwrap_or_default(),
                    ));
                    if let Some(cur) = current {
                        ui.label(format!("jump: {:.1} K", (cur - value).abs()))
                            .on_hover_text("larger jumps are the risky ones");
                    }
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("confirm").clicked() {
                            self.pending_set = None;
                            self.do_set(loop_n, value, typ, true);
                        }
                        if ui.button("cancel").clicked() {
                            self.pending_set = None;
                        }
                    });
                });
        }
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
                    if let Some(d) = rfd::FileDialog::new().pick_folder() {
                        self.log_dir = d;
                    }
                }
                ui.label(
                    egui::RichText::new(self.log_dir.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default())
                        .color(MUTED),
                )
                .on_hover_text(self.log_dir.display().to_string());

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let stop = ui.add_sized(
                        [110.0, 28.0],
                        egui::Button::new(
                            egui::RichText::new("STOP ALL").color(egui::Color32::WHITE),
                        )
                        .fill(RED),
                    );
                    if stop.clicked() {
                        if let Some(r) = &self.runner {
                            r.abort.store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                        if self.link.is_some() {
                            let _ = self.ask(DeviceCmd::Stop);
                            self.console_push("STOP ALL sent (heaters off, schedule aborted)".into());
                        }
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
