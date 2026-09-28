//! Schedule editor, run controls, and the console pane.

use super::{CryoApp, MUTED, RED};
use crate::schedule;

impl CryoApp {
    pub fn schedule_section(&mut self, ui: &mut egui::Ui) {
        ui.heading("schedule");
        ui.add_space(4.0);

        // the whole schedule area scrolls, so nothing is ever cut off on
        // small windows or long schedules
        egui::ScrollArea::vertical()
            .id_salt("schedule-scroll")
            .auto_shrink([false, false])
            .max_height(ui.available_height() - 170.0) // leave the console visible
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());

                ui.horizontal(|ui| {
                    // ---- editor (left) ----------------------------------
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
                    if ui
                        .button(egui::RichText::new("run").strong())
                        .clicked()
                    {
                        self.start_schedule(false);
                    }
                    if ui.button("dry run").clicked() {
                        self.start_schedule(true);
                    }
                });
                ui.add_enabled_ui(running, |ui| {
                    let b = ui.button(egui::RichText::new("abort").color(RED));
                    if b.clicked() {
                        if let Some(r) = &self.runner {
                            r.abort
                                .store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                });
                ui.label(
                    egui::RichText::new(
                        "dry run touches nothing — it only prints the steps",
                    )
                    .small()
                    .color(MUTED),
                );
            });
            }); // <- editor + controls row

            }); // <- scroll area

        ui.add_space(6.0);
        ui.separator();
        ui.add_space(4.0);

        // ---- console ---------------------------------------------------
        ui.heading("console");
        egui::ScrollArea::vertical()
            .max_height(140.0)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                egui::Grid::new("console").num_columns(1).show(ui, |ui| {
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
            self.console_push("ERROR: not connected (dry run still works)"
                .into());
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
