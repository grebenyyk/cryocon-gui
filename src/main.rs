//! Thin bootstrap: creates the native window and hands over to
//! [`cryocon_gui::ui::CryoApp`], which owns all state.

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 800.0])
            .with_title("cryocon-gui — Cryo-con 22C"),
        ..Default::default()
    };
    eframe::run_native(
        "cryocon-gui",
        options,
        Box::new(|_cc| Ok(Box::new(cryocon_gui::ui::CryoApp::new()))),
    )
}
