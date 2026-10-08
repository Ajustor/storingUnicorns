use std::sync::{Arc, Mutex};

use crate::engine::config::AppConfig;
use crate::updater::ExitAction;

use super::theme::{self, ThemeChoice};

// Fields are read by the app shell (plan 3b, Task 4).
#[allow(dead_code)]
pub struct App {
    pub config: AppConfig,
    pub theme: ThemeChoice,
    /// Read by `gui::run` after the window closes.
    pub exit_action: Arc<Mutex<Option<ExitAction>>>,
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        exit_action: Arc<Mutex<Option<ExitAction>>>,
    ) -> Self {
        let config = AppConfig::load().unwrap_or_default();
        let theme = ThemeChoice::from_config(config.theme.as_deref());
        cc.egui_ctx.set_fonts(theme::fonts());
        theme::apply(&cc.egui_ctx, theme);
        Self {
            config,
            theme,
            exit_action,
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("storingUnicorns");
        });
    }
}
