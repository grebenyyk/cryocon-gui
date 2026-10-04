//! Schedule editor, run controls, and the console pane.

use super::{CryoApp, RED};
use crate::schedule;

impl CryoApp {
    pub fn schedule_section(&mut self, ui: &mut egui::Ui) {
        ui.heading("schedule");
        ui.add_space(4.0);

        // NB: this section lives in its own scrolling panel (see mod.rs),
        // so it just takes its natural height.
        ui.horizontal(|ui| {
            // ---- editor (left) --------------------------------------
            ui.vertical(|ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut self.schedule_text)
                        .code_editor()
                        .desired_rows(8)
                        .desired_width(ui.available_width() * 0.7),
                );
                // validation: show every bad line, live
                match schedule::parse(&self.schedule_text) {
                    Ok(steps) => {
                        ui.label(
                            egui::RichText::new(format!("{} steps, valid", steps.len()))
                                .color(egui::Color32::from_rgb(0x0c, 0xa3, 0x0c)),
                        );
                    }
                    Err(errs) => {
                        for (_, msg) in errs.iter().take(6) {
                            ui.label(egui::RichText::new(msg.clone()).color(RED));
                        }
                        if errs.len() > 6 {
                            ui.label(egui::RichText::new(format!("… +{} more", errs.len() - 6)).color(RED));
                        }
                    }
                }
                ui.horizontal(|ui| {
                    if ui.button("load…").clicked() {
                        if let Some(p) = rfd::FileDialog::new()
                            .add_filter("schedule", &["txt"])
                            .pick_file()
                        {
                            match std::fs::read_to_string(&p) {
                                Ok(text) => self.schedule_text = text,
                                Err(e) => self.console_push(format!("ERROR: {e}")),
                            }
                        }
                    }
                    if ui.button("save…").clicked() {
                        if let Some(p) = rfd::FileDialog::new()
                            .set_file_name("schedule.txt")
                            .save_file()
                        {
                            let _ = std::fs::write(&p, &self.schedule_text);
                        }
                    }
                    if ui.button("template").clicked() {
                        self.schedule_text = schedule::TEMPLATE.into();
                    }
                });
            });

            ui.separator();

            // ---- run controls (right) ----------------------------------
            ui.vertical(|ui| {
                let running = self.runner.is_some();
                ui.add_enabled_ui(!running, |ui| {
                    // the primary action gets the primary styling
                    let run = ui.add_sized(
                        [96.0, 26.0],
                        egui::Button::new(
                            egui::RichText::new("run")
                                .strong()
                                .color(egui::Color32::WHITE),
                        )
                        .fill(super::BLUE),
                    );
                    if run.clicked() {
                        self.start_schedule(false);
                    }
                });
                ui.add_enabled_ui(running, |ui| {
                    // same size as 'run', so the button doesn't jump around
                    let b = ui.add_sized(
                        [96.0, 26.0],
                        egui::Button::new(egui::RichText::new("abort").color(RED)),
                    );
                    if b.clicked() {
                        if let Some(r) = &self.runner {
                            r.abort
                                .store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                });
            });
        }); // <- editor + controls row
    }

    /// Console: resizable bottom panel (its top edge is the drag handle).
    /// Heading styled the same as "charts" and "schedule".
    pub fn console_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("console");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("clear").clicked() {
                    self.console.clear();
                }
            });
        });
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                egui::Grid::new("console-lines").num_columns(1).show(ui, |ui| {
                    for line in &self.console {
                        ui.monospace(egui::RichText::new(line).small());
                        ui.end_row();
                    }
                });
            });
    }

    fn start_schedule(&mut self, dry_run: bool) {
        let steps = match schedule::parse(&self.schedule_text) {
            Ok(s) => s,
            Err(_) => {
                self.console_push("ERROR: fix the schedule first (see editor)"
                    .into());
                return;
            }
        };
        if steps.is_empty() {
            self.console_push("ERROR: schedule is empty".into());
            return;
        }
        if !dry_run && self.link.is_none() {
            self.console_push("ERROR: not connected".into());
            return;
        }
        let req_tx = match &self.link {
            Some(link) => link.req_tx.clone(),
            None => {
                // dry run without a link: give the runner a dead channel
                let (tx, _rx) = std::sync::mpsc::channel();
                tx
            }
        };
        let (prog_tx, prog_rx) = std::sync::mpsc::channel();
        self.runner = Some(schedule::spawn(
            steps,
            dry_run,
            req_tx,
            prog_tx,
            self.max_setpoint,
        ));
        self.prog_rx = Some(prog_rx);
        self.console_push(if dry_run {
            "dry run starting".into()
        } else {
            "schedule starting".into()
        });
    }
}
