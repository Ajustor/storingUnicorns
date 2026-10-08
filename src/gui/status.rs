//! Status bar (bottom) and update banner (top).

use egui::RichText;
use egui_phosphor::regular as icon;

use crate::updater::{UpdateEvent, UpdateState};

use super::app::App;
use super::theme::{self, ERROR, SUCCESS};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    Info,
    Success,
    Error,
}

pub struct Status {
    pub text: String,
    pub kind: StatusKind,
}

impl Default for Status {
    fn default() -> Self {
        Self {
            text: "Prêt".into(),
            kind: StatusKind::Info,
        }
    }
}

pub fn poll_updater(app: &mut App) {
    while let Some(ev) = app.updater.poll() {
        match ev {
            UpdateEvent::UpToDate => app.info("storingUnicorns est à jour"),
            UpdateEvent::Available(v) => app.info(format!("Version {v} disponible")),
            UpdateEvent::Ready => {
                app.success("Mise à jour installée : redémarrez pour l'appliquer")
            }
            UpdateEvent::Error(e) => app.error(format!("Mise à jour : {e}")),
        }
    }
}

/// Thin banner above everything while an update is available / downloading / ready.
pub fn update_banner(app: &mut App, ctx: &egui::Context) {
    let show = match &app.updater.state {
        UpdateState::Available(_) => !app.updater.dismissed,
        UpdateState::Downloading(_) | UpdateState::Ready(_) => true,
        _ => false,
    };
    if !show {
        return;
    }
    let state = app.updater.state.clone();
    egui::TopBottomPanel::top("update").show(ctx, |ui| {
        ui.horizontal(|ui| match &state {
            UpdateState::Available(info) => {
                ui.label(RichText::new(icon::ARROW_CIRCLE_UP).color(theme::ACCENT));
                ui.label(format!("La version {} est disponible.", info.version));
                if ui.button("Installer").clicked() {
                    app.updater.install();
                }
                ui.hyperlink_to("Nouveautés", &info.page_url);
                if ui.button("Plus tard").clicked() {
                    app.updater.dismissed = true;
                }
                if ui.button("Ignorer cette version").clicked() {
                    app.config.skipped_version = Some(info.version.to_string());
                    app.updater.dismissed = true;
                    app.save_config();
                }
            }
            UpdateState::Downloading(info) => {
                ui.spinner();
                ui.label(format!("Téléchargement de la version {}…", info.version));
            }
            UpdateState::Ready(info) => {
                ui.label(RichText::new(icon::CHECK_CIRCLE).color(SUCCESS));
                ui.label(format!("Version {} prête.", info.version));
                if ui.button("Redémarrer maintenant").clicked() {
                    app.updater.schedule_restart();
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            _ => {}
        });
    });
}

pub fn status_bar(app: &mut App, ctx: &egui::Context) {
    egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
        ui.horizontal(|ui| {
            connections(app, ui);
            ui.separator();
            if let Some(name) = app.sessions.connecting.iter().next() {
                ui.spinner();
                ui.label(format!("Connexion à {name}…"));
            } else if let Some((done, total, label)) = &app.progress {
                ui.add(
                    egui::ProgressBar::new(*done as f32 / (*total).max(1) as f32)
                        .desired_width(160.0),
                );
                ui.label(format!("{done}/{total} {label}"));
            } else {
                let color = match app.status.kind {
                    StatusKind::Info => ui.visuals().text_color(),
                    StatusKind::Success => SUCCESS,
                    StatusKind::Error => ERROR,
                };
                ui.label(RichText::new(&app.status.text).color(color))
                    .on_hover_text(&app.status.text);
            }
            if let Some(summary) = app.tabs.active().and_then(|t| t.summary.as_deref()) {
                ui.separator();
                ui.label(RichText::new(summary).weak());
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let version = format!("v{}", crate::updater::current_version());
                let hover = match &app.updater.state {
                    UpdateState::Failed(e) => format!("Dernière vérification en échec : {e}"),
                    _ => "Vérifier les mises à jour".to_string(),
                };
                if ui
                    .link(RichText::new(version).weak())
                    .on_hover_text(hover)
                    .clicked()
                {
                    app.updater.check(true, None);
                }
                if ui
                    .button(app.theme.icon())
                    .on_hover_text("Thème : système / sombre / clair")
                    .clicked()
                {
                    app.theme = app.theme.next();
                    theme::apply(ui.ctx(), app.theme);
                    app.config.theme = app.theme.to_config();
                    app.save_config();
                }
                if ui
                    .selectable_label(app.show_value_panel, icon::SIDEBAR_SIMPLE)
                    .on_hover_text("Panneau Valeur")
                    .clicked()
                {
                    app.show_value_panel = !app.show_value_panel;
                }
            });
        });
    });
}

/// "● ● 2 connexions": one dot per open session, in its colour.
fn connections(app: &App, ui: &mut egui::Ui) {
    let n = app.sessions.open.len();
    if n == 0 {
        ui.label(RichText::new(icon::CIRCLE).weak());
        ui.label(RichText::new("Aucune connexion").weak());
        return;
    }
    let spacing = ui.spacing().item_spacing.x;
    ui.spacing_mut().item_spacing.x = 2.0;
    for name in app.sessions.open.keys() {
        theme::dot(ui, app.color_of(name)).on_hover_text(name);
    }
    ui.spacing_mut().item_spacing.x = spacing;
    ui.label(if n == 1 {
        "1 connexion".to_string()
    } else {
        format!("{n} connexions")
    });
}
