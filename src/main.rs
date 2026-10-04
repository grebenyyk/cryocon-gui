//! Thin bootstrap: creates the native window and hands over to
//! [`cryocon_gui::ui::CryoApp`], which owns all state.

fn main() -> eframe::Result<()> {
    // eframe re-installs the app icon at runtime (every frame) and would
    // put its default egui logo over the bundle's AppIcon a moment after
    // launch — hand it the same snowflake PNG the bundle uses instead.
    let icon =
        eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon-1024.png"))
            .expect("embedded app icon must decode");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 800.0])
            .with_title("cryocon-gui — Cryo-con 22C")
            .with_icon(std::sync::Arc::new(icon)),
        ..Default::default()
    };
    eframe::run_native(
        "cryocon-gui",
        options,
        Box::new(|_cc| Ok(Box::new(cryocon_gui::ui::CryoApp::new()))),
    )
}
