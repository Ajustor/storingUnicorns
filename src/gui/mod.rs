//! Graphical interface (egui).
mod app;
mod dialogs;
mod editor;
mod explorer;
mod history_popup;
mod grid;
mod sessions;
mod status;
mod table_search;
mod tabs;
mod theme;
mod value_panel;
mod worker;

use std::sync::{Arc, Mutex};

pub fn run() -> anyhow::Result<()> {
    crate::updater::cleanup_stale_staging();
    let exit_action = Arc::new(Mutex::new(None));
    let viewport = egui::ViewportBuilder::default()
        .with_title("storingUnicorns")
        .with_app_id("storingUnicorns")
        .with_inner_size([1280.0, 800.0])
        .with_min_inner_size([720.0, 480.0]);
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    let action = exit_action.clone();
    eframe::run_native(
        "storingUnicorns",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, action)))),
    )
    .map_err(|e| anyhow::anyhow!("GUI error: {e}"))?;

    let action = exit_action
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
    if let Some(action) = action {
        if let Err(e) = crate::updater::run_exit_action(&action, &[]) {
            tracing::error!("Failed to apply the update on exit ({action:?}): {e}");
        }
    }
    Ok(())
}
