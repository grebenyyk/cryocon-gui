//! cryocon-gui — a small GUI for the Cryo-con Model 22C controller.
//!
//! Module map:
//! - [`device`]   protocol + background worker thread (TCP :5000)
//! - [`schedule`] schedule grammar, parser, runner thread
//! - [`logging`]  CSV log writer (bash-toolchain compatible)
//! - [`ui`]       the eframe/egui application

pub mod device;
pub mod logging;
pub mod schedule;
pub mod ui;
