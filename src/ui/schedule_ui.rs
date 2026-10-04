//! Schedule editor, run controls, and the console pane.

use super::{CryoApp, BLUE, GREEN, RED};
use super::rate_gate::PendingRun;
use crate::schedule;

impl CryoApp {
    pub fn schedule_section(&mut self, ui: &mut egui::Ui) {
        ui.heading("schedule");
        ui.add_space(4.0);

        // Validate up front: the bottom strip (status lines + the five
        // buttons) is pinned to the section's bottom edge, so its height
        // must be known before layout; the editor fills everything above.
        let parsed = schedule::parse(&self.schedule_text);
        let n_lines = match &parsed {
            Ok(_) => 1,
            Err(errs) => errs.len().min(6) + usize::from(errs.len() > 6),
        };
        let line_h = ui.text_style_height(&egui::TextStyle::Body);
        let gap = ui.spacing().item_spacing.y;
        //  + button row + panel margins + a little slack (overshoot is
        //  harmless — content top-aligns; undershoot would clip buttons)
        let bottom_h = n_lines as f32 * line_h + (n_lines - 1) as f32 * gap + 52.0;

        egui::Panel::bottom("schedule-bottom")
            .exact_size(bottom_h)
            .show(ui, |ui| {
                // live validation (same as it ever was)
                match &parsed {
                    Ok(steps) => {
                        ui.label(
                            egui::RichText::new(format!("{} steps, valid", steps.len()))
                                .color(GREEN),
                        );
                    }
                    Err(errs) => {
                        for (_, msg) in errs.iter().take(6) {
                            ui.label(egui::RichText::new(msg.clone()).color(RED));
                        }
                        if errs.len() > 6 {
                            ui.label(
                                egui::RichText::new(format!("… +{} more", errs.len() - 6))
                                    .color(RED),
                            );
                        }
                    }
                }
                // one row of five, same height: file actions, then the two
                // run controls (kept visually grouped by a separator)
                ui.horizontal(|ui| {
                    if ui.add(egui::Button::new("load…").min_size([0.0, 26.0].into())).clicked() {
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
                    if ui.add(egui::Button::new("save…").min_size([0.0, 26.0].into())).clicked() {
                        if let Some(p) = rfd::FileDialog::new()
                            .set_file_name("schedule.txt")
                            .save_file()
                        {
                            let _ = std::fs::write(&p, &self.schedule_text);
                        }
                    }
                    if ui.add(egui::Button::new("template").min_size([0.0, 26.0].into())).clicked() {
                        self.schedule_text = schedule::TEMPLATE.into();
                    }
                    ui.separator();
                    let running = self.runner.is_some();
                    ui.add_enabled_ui(!running, |ui| {
                        // the primary action keeps the primary styling
                        let run = ui.add_sized(
                            [96.0, 26.0],
                            egui::Button::new(
                                egui::RichText::new("run")
                                    .strong()
                                    .color(egui::Color32::WHITE),
                            )
                            .fill(BLUE),
                        );
                        if run.clicked() {
                            self.try_run_schedule();
                        }
                    });
                    ui.add_enabled_ui(running, |ui| {
                        // same size as 'run', so the row doesn't jump around
                        let b = ui.add_sized(
                            [96.0, 26.0],
                            egui::Button::new(egui::RichText::new("abort").color(RED)),
                        );
                        if b.clicked() {
                            if let Some(r) = &self.runner {
                                r.abort.store(true, std::sync::atomic::Ordering::Relaxed);
                            }
                        }
                    });
                });
            });

        // The editor mirrors the console's scroll exactly: an outer
        // ScrollArea carries the content (egui 0.36 text edits grow with
        // their text and count on the surrounding ScrollArea to scroll),
        // and min_size stretches the editor frame to the full pane so it
        // doesn't hug three lines and leave an awkward gap below. The
        // code-editor tint keeps input visually distinct from console
        // output.
        let pane_h = ui.available_height().max(80.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut self.schedule_text)
                        .code_editor()
                        .desired_width(f32::INFINITY)
                        .min_size(egui::vec2(0.0, pane_h - 1.0)),
                );
            });
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

    /// Real run: parse, then through the rate-calibration gate — which
    /// starts it immediately when nothing needs calibrating (cooling
    /// ramps, rate-free schedules).
    pub fn try_run_schedule(&mut self) {
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
        self.gate_run(PendingRun::Schedule { steps });
    }

    /// Shared runner start for real runs — the schedule editor and the
    /// quick-ramp panels both funnel through here, so abort, console and
    /// CSV logging behave identically. Returns false (with a console
    /// ERROR) when there is no connection.
    pub fn start_steps(&mut self, steps: Vec<schedule::Step>) -> bool {
        let req_tx = match &self.link {
            Some(link) => link.req_tx.clone(),
            None => {
                self.console_push("ERROR: not connected".into());
                return false;
            }
        };
        let (prog_tx, prog_rx) = std::sync::mpsc::channel();
        self.runner = Some(schedule::spawn(
            steps,
            false,
            req_tx,
            prog_tx,
            self.max_setpoint,
        ));
        self.prog_rx = Some(prog_rx);
        true
    }
}
