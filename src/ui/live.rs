//! Left panel: live readout tiles, manual setpoint control, safety
//! settings; and the two central charts (temperature, heater power).

use super::{CryoApp, BLUE, GREEN, INK2, MUTED, ORANGE, RED, TYPES};
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
        egui::Grid::new("manual-set").num_columns(2).show(ui, |ui| {
            ui.label("value, K");
            ui.text_edit_singleline(&mut self.manual_setpoint);
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
        if ui.button("apply").clicked() {
            if let Ok(v) = self.manual_setpoint.trim().parse::<f64>() {
                self.do_set(1, v, Some(TYPES[self.manual_type].to_string()), false);
            } else {
                self.console_push("ERROR: setpoint is not a number".into());
            }
        }

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

        // fixed, generous heights: the whole central column scrolls, so the
        // plots no longer squeeze into whatever space the schedule left
        Plot::new("temperature")
            .legend(Legend::default())
            .height(280.0)
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
            .height(220.0)
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

fn small_row(ui: &mut egui::Ui, name: &str, v: Option<f64>, unit: &str) {
    let text = v.map(|x| format!("{x:.2} {unit}")).unwrap_or_else(|| "—".into());
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(name).color(MUTED));
        ui.label(text);
    });
}
