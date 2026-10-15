//! Status bar (bottom) and update banner (top).

use egui::RichText;
use egui_phosphor::regular as icon;

use crate::updater::{UpdateEvent, UpdateState, Updater};

use super::app::App;
use super::theme::{self, ERROR, SUCCESS};
use super::worker::OpId;

/// The error shown when connection `name` is needed but not open.
pub fn not_connected(name: &str) -> String {
    format!("{name} n'est pas connectée")
}

/// First non-blank line of `text`, with " …" when lines were dropped:
/// multi-line errors must not grow the status bar or a panel.
pub fn one_line(text: &str) -> std::borrow::Cow<'_, str> {
    let trimmed = text.trim();
    match trimmed.split_once('\n') {
        None => trimmed.into(),
        Some((first, _)) => format!("{} …", first.trim_end()).into(),
    }
}

/// `text` on one truncated line in `color`, the full text on hover.
pub fn one_line_label(ui: &mut egui::Ui, text: &str, color: egui::Color32) -> egui::Response {
    ui.add(egui::Label::new(RichText::new(one_line(text)).color(color)).truncate())
        .on_hover_text(text)
}

/// Progress of the running transfers (import / export / truncate), each
/// keyed by its operation so that two of them don't overwrite each other.
/// The status bar shows the one updated last.
#[derive(Debug, Default)]
pub struct Progress {
    /// `(op, done, total, label)`, least recently updated first.
    ops: Vec<(OpId, usize, usize, String)>,
}

impl Progress {
    pub fn update(&mut self, op: OpId, done: usize, total: usize, label: impl Into<String>) {
        self.ops.retain(|o| o.0 != op);
        self.ops.push((op, done, total, label.into()));
    }

    /// Operation `op` ended.
    pub fn finish(&mut self, op: OpId) {
        self.ops.retain(|o| o.0 != op);
    }

    /// `(done, total, label)` of the operation updated last.
    pub fn current(&self) -> Option<(usize, usize, &str)> {
        self.ops
            .last()
            .map(|(_, done, total, label)| (*done, *total, label.as_str()))
    }
}

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

/// The banner shows an available (not dismissed), downloading or ready (not
/// put off) update.
fn banner_shown(updater: &Updater) -> bool {
    match &updater.state {
        UpdateState::Available(_) | UpdateState::Ready(_) => !updater.dismissed,
        UpdateState::Downloading(_) => true,
        _ => false,
    }
}

/// Thin banner above everything while an update is available / downloading / ready.
pub fn update_banner(app: &mut App, ctx: &egui::Context) {
    if !banner_shown(&app.updater) {
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
                // Goes through the normal close: cancelling its confirmation
                // drops the relaunch (`cancel_restart`).
                if ui.button("Redémarrer maintenant").clicked() {
                    app.updater.schedule_restart();
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
                if ui
                    .button("Plus tard")
                    .on_hover_text("La mise à jour s'appliquera à la fermeture")
                    .clicked()
                {
                    app.updater.later();
                }
            }
            _ => {}
        });
    });
}

/// Banner above everything while a connection waits for its browser
/// sign-in: the link to copy (the browser may not have opened) and
/// « Annuler ».
pub fn sign_in_banner(app: &mut App, ctx: &egui::Context) {
    let Some((connection, url)) = app.sign_ins.current() else {
        return;
    };
    let (connection, url) = (connection.to_string(), url.to_string());
    let mut cancel = false;
    egui::TopBottomPanel::top("sign_in").show(ctx, |ui| {
        cancel = super::sign_in::waiting_row(ui, &connection, &url);
    });
    if cancel {
        app.cancel_sign_in(&connection);
    }
}

pub fn status_bar(app: &mut App, ctx: &egui::Context) {
    egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
        ui.horizontal(|ui| {
            version(app, ui);
            ui.separator();
            connections(app, ui);
            ui.separator();
            if let Some(summary) = app.tabs.active().and_then(|t| t.summary.clone()) {
                ui.label(RichText::new(summary).weak());
                ui.separator();
            }
            // Right side first, so the message gets (and is truncated to)
            // the room left in between.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                right_side(app, ui);
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    message(app, ui);
                });
            });
        });
    });
}

/// Connection in progress, transfer progress, or the last message.
fn message(app: &App, ui: &mut egui::Ui) {
    if let Some(name) = app.sessions.connecting.iter().next() {
        ui.spinner();
        ui.label(format!("Connexion à {name}…"));
    } else if let Some((done, total, label)) = app.progress.current() {
        ui.add(egui::ProgressBar::new(done as f32 / total.max(1) as f32).desired_width(160.0));
        ui.label(format!("{done}/{total} {label}"));
    } else {
        let color = match app.status.kind {
            StatusKind::Info => ui.visuals().text_color(),
            StatusKind::Success => SUCCESS,
            StatusKind::Error => ERROR,
        };
        one_line_label(ui, &app.status.text, color);
    }
}

/// Text and hover of the version link for `current`: "v0.13.0 → 0.14.0"
/// once an update is ready.
fn version_label(state: &UpdateState, current: &str) -> (String, String) {
    match state {
        UpdateState::Ready(info) => (
            format!("v{current} → {}", info.version),
            "Mise à jour prête : cliquer pour redémarrer".into(),
        ),
        UpdateState::Failed(e) => (
            format!("v{current}"),
            format!("storingUnicorns {current} — dernière vérification en échec : {e}"),
        ),
        _ => (
            format!("v{current}"),
            format!("storingUnicorns {current} — vérifier les mises à jour"),
        ),
    }
}

/// The running version, at the far left; a click checks for updates, or
/// shows the banner of a ready update again.
fn version(app: &mut App, ui: &mut egui::Ui) {
    let ready = matches!(app.updater.state, UpdateState::Ready(_));
    let (text, hover) = version_label(&app.updater.state, crate::updater::current_version());
    let text = if ready {
        RichText::new(text).color(theme::ACCENT)
    } else {
        RichText::new(text).weak()
    };
    if ui.link(text).on_hover_text(hover).clicked() {
        if ready {
            app.updater.show_ready();
        } else {
            app.updater.check(true, None);
        }
    }
}

/// Theme toggle, value panel toggle.
fn right_side(app: &mut App, ui: &mut egui::Ui) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_of_concurrent_operations_does_not_clobber() {
        let mut p = Progress::default();
        assert_eq!(p.current(), None);
        p.update(1, 0, 10, "tables");
        p.update(2, 0, 500, "lignes");
        assert_eq!(p.current(), Some((0, 500, "lignes")), "most recent");
        p.update(1, 3, 10, "tables");
        assert_eq!(p.current(), Some((3, 10, "tables")));
        p.finish(1);
        assert_eq!(p.current(), Some((0, 500, "lignes")), "the other one stays");
        p.finish(2);
        p.finish(7);
        assert_eq!(p.current(), None);
    }

    fn ready_updater() -> Updater {
        let json = r#"{"version":"99.0.0","notes":"","page_url":"p","assets":[{"name":"storingUnicorns-setup.msi","url":"u","sha256":"00"}]}"#;
        let info = crate::updater::release_from_manifest_for_tests(json)
            .unwrap()
            .unwrap();
        let mut u = Updater::new(|| {});
        u.state = UpdateState::Ready(info);
        u
    }

    #[test]
    fn later_hides_the_ready_banner_and_the_version_shows_it_again() {
        let mut u = ready_updater();
        assert!(banner_shown(&u));
        u.later();
        assert!(!banner_shown(&u));
        let (text, hover) = version_label(&u.state, "0.13.0");
        assert_eq!(text, "v0.13.0 → 99.0.0");
        assert_eq!(hover, "Mise à jour prête : cliquer pour redémarrer");
        u.show_ready();
        assert!(banner_shown(&u));
    }

    #[test]
    fn version_label_without_ready_update() {
        let u = Updater::new(|| {});
        assert!(!banner_shown(&u));
        let (text, hover) = version_label(&u.state, "0.13.0");
        assert_eq!(text, "v0.13.0");
        assert_eq!(hover, "storingUnicorns 0.13.0 — vérifier les mises à jour");
        let (_, hover) = version_label(&UpdateState::Failed("boom".into()), "0.13.0");
        assert_eq!(
            hover,
            "storingUnicorns 0.13.0 — dernière vérification en échec : boom"
        );
    }

    #[test]
    fn one_line_keeps_the_first_line_only() {
        assert_eq!(one_line("  ok  "), "ok");
        assert_eq!(one_line("error: x\n  detail\nmore"), "error: x …");
        assert_eq!(one_line("\nfirst\r\nsecond"), "first …");
    }
}
