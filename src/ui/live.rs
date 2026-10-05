//! Left panel: live readout tiles, manual setpoint control, safety
//! settings; and the two central charts (temperature, heater power).

use super::{CryoApp, BLUE, GREEN, INK2, MUTED, ORANGE, RED, TYPES};
use super::rate_gate::PendingRun;
use egui_plot::{Legend, Line, Plot};

/// One line of help per loop type (hover the dropdown entries).
/// Wording per the Model 22C User's Guide (control types table).
const TYPE_HELP: [&str; 7] = [
    "PID — classic feedback: drive straight to the setpoint as fast as the \
     tuned PID allows (a step change).",
    "RampP — temperature ramp mode: glide to the setpoint at the loop's ramp \
     rate (K/min, the 'rate' schedule command). The gentle option for real \
     samples.",
    "RampT — temperature ramp mode that pulls its tuning parameters from the \
     stored PID tables as the ramp progresses. Use only if you maintain \
     PID tables.",
    "Man — manual heater power: the setpoint field is ignored and the loop \
     outputs a fixed percentage (the 'Pmanual' setting). For heater tests.",
    "Off — loop disabled, no heating at all.",
    "Table — controlled by PID-table lookup (table chosen via 'PID Table \
     index'; not editable from this app).",
    "SCALE — output voltage scales with input temperature. Loops 3 and 4 \
     (analog outputs) only; not valid for the heater loops.",
];

impl CryoApp {
    pub fn live_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("live");
        ui.add_space(4.0);

        big_tile(ui, "temperature A", self.snap.t_a, "K", BLUE);
        big_tile(ui, "setpoint 1", self.snap.setpoint_1, "K", INK2);
        big_tile(ui, "heater power", self.snap.power_1, "%", ORANGE);
        small_row(ui, "ramp rate", self.snap.rate_1, "K/min");
        ui.label(
            egui::RichText::new("input B: no sensor").color(MUTED),
        )
        .on_hover_text("channel B has no sensor on this instrument; never used for stability");

        // ---- loop control: after STOP, setpoints don't heat until ON ----
        ui.horizontal(|ui| {
            let (color, status) = match self.control_on {
                Some(true) => (GREEN, "control ON"),
                Some(false) => (RED, "control OFF"),
                None => (MUTED, "control ?"),
            };
            ui.colored_label(color, "●").on_hover_text(
                "the instrument's CONTROL/STOP for all loops. After STOP ALL \
                 the setpoint is stored but nothing heats until control is on.",
            );
            ui.label(egui::RichText::new(status).color(INK2).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let on = ui.add(
                    egui::Button::new(
                        egui::RichText::new("control on").color(egui::Color32::WHITE),
                    )
                    .fill(GREEN),
                );
                if on.clicked() {
                    self.set_control(true);
                }
                if ui.button("control off").clicked() {
                    self.set_control(false);
                }
            });
        });

        ui.add_space(10.0);
        ui.separator();
        ui.add_space(6.0);

        // ---- manual setpoint ------------------------------------------
        ui.heading("set setpoint");
        // a dialog window up = it owns Return; fields must not submit
        let dialog_open = self.pending_set.is_some() || self.rate_gate.is_some();
        let mut enter_apply = false;
        egui::Grid::new("manual-set").num_columns(2).show(ui, |ui| {
            ui.label("value, K");
            enter_apply = submit(&mut self.manual_setpoint, ui, dialog_open);
            ui.end_row();
            ui.label("type");
            egui::ComboBox::from_id_salt("loop-type")
                .selected_text(TYPES[self.manual_type])
                .show_ui(ui, |ui| {
                    for (i, t) in TYPES.iter().enumerate() {
                        ui.selectable_value(&mut self.manual_type, i, *t)
                            .on_hover_text(TYPE_HELP[i]);
                    }
                });
            ui.end_row();
        });
        if ui.button("apply").clicked() || enter_apply {
            if let Ok(v) = self.manual_setpoint.trim().parse::<f64>() {
                self.do_set(1, v, Some(TYPES[self.manual_type].to_string()), false);
            } else {
                self.console_push("ERROR: setpoint is not a number".into());
            }
        }

        ui.add_space(10.0);
        ui.separator();
        ui.add_space(6.0);

        // ---- quick ramps ------------------------------------------------
        self.quick_ramp_panels(ui);

        ui.add_space(10.0);
        ui.separator();
        ui.add_space(6.0);

        // ---- safety settings ------------------------------------------
        ui.heading("safety");
        egui::Grid::new("safety").num_columns(2).show(ui, |ui| {
            ui.label("max setpoint, K");
            ui.add(egui::DragValue::new(&mut self.max_setpoint).range(1.0..=1500.0));
            ui.end_row();
            ui.label("confirm jumps >, K");
            ui.add(egui::DragValue::new(&mut self.jump_confirm_k).range(0.0..=1000.0));
            ui.end_row();
        });
        ui.label(
            egui::RichText::new("OTD on this instrument: 300 K, DISABLED").color(MUTED),
        )
        .on_hover_text(
            "the over-temperature disconnect is switched off — that is why \
             the default max setpoint equals its threshold",
        );

        // ---- warm up (end of day) --------------------------------------
        ui.add_space(10.0);
        ui.separator();
        ui.add_space(6.0);
        if ui
            .button("warm up to 298 K")
            .on_hover_text(
                "end of day: engages control (if off) and heats to room \
                 temperature at full heater power (~15-20 min from ~100 K), \
                 then holds there — so the samples are at room temperature \
                 when you open the cryostat. You can leave it unattended; \
                 the console announces the arrival. STOP ALL once the \
                 samples are out. Rate calibration is skipped."
            )
            .clicked()
        {
            self.start_warmup();
        }
    }

    /// The two quick-ramp panels under 'set setpoint': the same
    /// rate + set + RampP that the schedule grammar spells out, entered as
    /// two numbers instead of a script. 'in time' derives the rate from
    /// the current temperature; 'at rate' takes the rate as given.
    fn quick_ramp_panels(&mut self, ui: &mut egui::Ui) {
        let running = self.runner.is_some();
        // a dialog window up = it owns Return; fields must not submit
        let dialog_open = self.pending_set.is_some() || self.rate_gate.is_some();

        // ---- panel 1: reach the target within a given time -------------
        ui.heading("go to · in time");
        let mut go_time = false;
        egui::Grid::new("quick-time").num_columns(2).show(ui, |ui| {
            ui.label("target, K");
            go_time |= submit(&mut self.quick_target_time, ui, dialog_open);
            ui.end_row();
            ui.label("in, min");
            go_time |= submit(&mut self.quick_time_min, ui, dialog_open);
            ui.end_row();
        });
        // live preview of the rate that would be commanded
        let derived = self.derived_rate();
        ui.label(
            egui::RichText::new(match &derived {
                Ok(rate) => format!("rate ≈ {rate:.2} K/min"),
                Err(why) => why.clone(),
            })
            .small()
            .color(MUTED),
        )
        .on_hover_text(
            "rate = |target − current temperature A| / time — updated live \
             from the latest reading",
        );
        ui.add_enabled_ui(!running, |ui| {
            if go_button(ui).clicked() || go_time {
                self.go_in_time();
            }
        })
        .response
        .on_disabled_hover_text("a schedule is running — abort it first");

        ui.add_space(8.0);

        // ---- panel 2: reach the target at a given rate ------------------
        ui.heading("go to · at rate");
        let mut go_rate = false;
        egui::Grid::new("quick-rate").num_columns(2).show(ui, |ui| {
            ui.label("target, K");
            go_rate |= submit(&mut self.quick_target_rate, ui, dialog_open);
            ui.end_row();
            ui.label("rate, K/min");
            go_rate |= submit(&mut self.quick_rate, ui, dialog_open);
            ui.end_row();
        });
        ui.add_enabled_ui(!running, |ui| {
            if go_button(ui).clicked() || go_rate {
                self.go_at_rate();
            }
        })
        .response
        .on_disabled_hover_text("a schedule is running — abort it first");
    }

    /// 'go to · in time' — the button and the Return key both land here.
    fn go_in_time(&mut self) {
        match (self.quick_target_time.trim().parse::<f64>(), self.derived_rate()) {
            (Ok(target), Ok(rate)) => self.launch_quick_ramp(rate, target),
            (Ok(_), Err(why)) => self.console_push(format!("ERROR: {why}")),
            _ => self.console_push("ERROR: target is not a number".into()),
        }
    }

    /// 'go to · at rate' — the button and the Return key both land here.
    fn go_at_rate(&mut self) {
        match (
            self.quick_target_rate.trim().parse::<f64>(),
            self.quick_rate.trim().parse::<f64>(),
        ) {
            (Ok(target), Ok(rate)) if rate > 0.0 => self.launch_quick_ramp(rate, target),
            (Ok(_), Ok(rate)) => {
                self.console_push(format!("ERROR: rate must be > 0 (got {rate})"))
            }
            _ => self.console_push("ERROR: target/rate is not a number".into()),
        }
    }

    /// Panel-1 inputs parsed, with the rate derived from the live
    /// temperature (Err carries the reason, shown in the preview line).
    fn derived_rate(&self) -> Result<f64, String> {
        let target: f64 = self
            .quick_target_time
            .trim()
            .parse()
            .map_err(|_| "target is not a number".to_string())?;
        let minutes: f64 = self
            .quick_time_min
            .trim()
            .parse()
            .map_err(|_| "time is not a number".to_string())?;
        if minutes <= 0.0 {
            return Err("time must be > 0".into());
        }
        let t = self
            .snap
            .t_a
            .ok_or_else(|| "no live temperature yet (channel A)".to_string())?;
        let delta = (target - t).abs();
        if delta < 0.5 {
            return Err("already at the target".into());
        }
        Ok(delta / minutes)
    }

    /// Handle a quick-panel "go": clamp the target, then hand the ramp
    /// to the rate-calibration gate (which starts it directly for
    /// cooling legs — only heating at a commanded rate is gated).
    fn launch_quick_ramp(&mut self, rate: f64, target: f64) {
        // the same clamp as every other setpoint write
        let target = if target > self.max_setpoint {
            self.console_push(format!(
                "WARNING: {target} clamped to max setpoint {}",
                self.max_setpoint
            ));
            self.max_setpoint
        } else {
            target
        };
        self.gate_run(PendingRun::Quick { rate, target });
    }

    /// The two stacked charts (one quantity per axis — never two scales on
    /// one plot).
    pub fn charts(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("charts");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("clear").clicked() {
                    self.history.clear();
                    self.t0 = std::time::Instant::now();
                }
            });
        });

        let t: Vec<[f64; 2]> = self
            .history
            .iter()
            .filter_map(|s| s.t_a.map(|v| [s.t_min, v]))
            .collect();
        let sp: Vec<[f64; 2]> = self
            .history
            .iter()
            .filter_map(|s| s.setpoint.map(|v| [s.t_min, v]))
            .collect();
        let pw: Vec<[f64; 2]> = self
            .history
            .iter()
            .filter_map(|s| s.power.map(|v| [s.t_min, v]))
            .collect();

        // the plots share the charts panel's height (temperature gets the
        // larger share), so dragging the panel's bottom edge resizes both
        let avail = ui.available_height();
        Plot::new("temperature")
            .legend(Legend::default())
            .height((avail * 0.55).max(130.0))
            .x_axis_label("elapsed, min")
            .y_axis_label("K")
            .show(ui, |p| {
                if !t.is_empty() {
                    p.line(Line::new("temperature A", t).color(BLUE).width(2.0));
                }
                if !sp.is_empty() {
                    p.line(Line::new("setpoint", sp).color(MUTED).width(1.6));
                }
            });
        Plot::new("power")
            .legend(Legend::default())
            .height(ui.available_height().max(100.0))
            .x_axis_label("elapsed, min")
            .y_axis_label("%")
            .show(ui, |p| {
                if !pw.is_empty() {
                    p.line(Line::new("heater power", pw).color(ORANGE).width(1.8));
                }
            });
    }
}

fn big_tile(ui: &mut egui::Ui, name: &str, v: Option<f64>, unit: &str, color: egui::Color32) {
    let text = match v {
        Some(x) => format!("{x:.2} {unit}"),
        None => format!("— {unit}"),
    };
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(text).size(22.0).strong().color(color));
    });
    ui.label(egui::RichText::new(name).color(MUTED));
    ui.add_space(8.0);
}

/// A single-line edit that reports being submitted with Return (the idiom
/// from egui's own docs: a single-line edit surrenders focus on Return).
/// While a dialog window is up, the dialog owns Return — never submit in
/// parallel with it.
fn submit(text: &mut String, ui: &mut egui::Ui, dialog_open: bool) -> bool {
    let response = ui.text_edit_singleline(text);
    !dialog_open && response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
}

/// The blue action button shared by the two quick-ramp panels — same size
/// and styling as the schedule section's 'run'.
fn go_button(ui: &mut egui::Ui) -> egui::Response {
    ui.add_sized(
        [96.0, 26.0],
        egui::Button::new(
            egui::RichText::new("go").strong().color(egui::Color32::WHITE),
        )
        .fill(BLUE),
    )
}

fn small_row(ui: &mut egui::Ui, name: &str, v: Option<f64>, unit: &str) {
    let text = v.map(|x| format!("{x:.2} {unit}")).unwrap_or_else(|| "—".into());
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(name).color(MUTED));
        ui.label(text);
    });
}
