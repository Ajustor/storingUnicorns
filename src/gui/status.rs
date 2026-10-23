//! Status bar (bottom) and update banner (top).

use egui::RichText;
use egui_phosphor::regular as icon;

use crate::updater::{UpdateEvent, UpdateState, Updater};

use super::app::App;
use super::dialogs::{update, Dialog};
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

/// Whether the update modal is open.
fn update_modal_open(app: &App) -> bool {
    matches!(app.dialog, Some(Dialog::Update))
}

pub fn poll_updater(app: &mut App) {
    let modal = update_modal_open(app);
    while let Some(ev) = app.updater.poll() {
        match update_message(&ev, modal) {
            Some((StatusKind::Info, text)) => app.info(text),
            Some((StatusKind::Success, text)) => app.success(text),
            Some((StatusKind::Error, text)) => app.error(text),
            None => {}
        }
    }
}

/// The status message of an update event; none while the update modal is
/// open: it shows the outcome itself.
fn update_message(ev: &UpdateEvent, modal_open: bool) -> Option<(StatusKind, String)> {
    if modal_open {
        return None;
    }
    Some(match ev {
        UpdateEvent::UpToDate => (StatusKind::Info, "storingUnicorns est à jour".into()),
        UpdateEvent::Available(v) => (StatusKind::Info, format!("Version {v} disponible")),
        UpdateEvent::Ready => (
            StatusKind::Success,
            "Mise à jour installée : redémarrez pour l'appliquer".into(),
        ),
        UpdateEvent::Error(e) => (StatusKind::Error, format!("Mise à jour : {e}")),
    })
}

/// The banner shows an available or ready update not put off ("Plus
/// tard"), unless the update modal (which shows it) is open.
fn banner_shown(updater: &Updater, modal_open: bool) -> bool {
    match &updater.state {
        UpdateState::Available(_) | UpdateState::Ready(_) => !updater.dismissed && !modal_open,
        _ => false,
    }
}

/// Thin banner above everything when an update was found (at startup) or
/// got installed in the background; « Voir la mise à jour » opens the
/// update modal.
pub fn update_banner(app: &mut App, ctx: &egui::Context) {
    if !banner_shown(&app.updater, update_modal_open(app)) {
        return;
    }
    let state = app.updater.state.clone();
    egui::TopBottomPanel::top("update").show(ctx, |ui| {
        ui.horizontal(|ui| match &state {
            UpdateState::Available(info) => {
                ui.label(RichText::new(icon::ARROW_CIRCLE_UP).color(theme::ACCENT));
                ui.label(format!("La version {} est disponible.", info.version));
                if ui.button("Voir la mise à jour").clicked() {
                    update::open(app);
                }
                if ui.button("Plus tard").clicked() {
                    app.updater.later();
                }
            }
            UpdateState::Ready(info) => {
                ui.label(RichText::new(icon::CHECK_CIRCLE).color(SUCCESS));
                ui.label(format!(
                    "La version {} est installée. Redémarrez pour l'utiliser.",
                    info.version
                ));
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
    let cancellable =
        super::sign_in::cancellable(&connection, &app.sessions.connecting, app.worker.testing());
    let mut cancel = false;
    egui::TopBottomPanel::top("sign_in").show(ctx, |ui| {
        cancel = super::sign_in::waiting_row(ui, &connection, &url, cancellable);
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

/// Hover of the version link for `current`.
fn version_hover(state: &UpdateState, current: &str) -> String {
    match state {
        UpdateState::Checking => {
            format!("storingUnicorns {current} — recherche d'une mise à jour…")
        }
        UpdateState::Failed(e) => {
            format!("storingUnicorns {current} — dernière vérification en échec : {e}")
        }
        UpdateState::Available(_) | UpdateState::Downloading(_) | UpdateState::Ready(_) => {
            format!("storingUnicorns {current} — voir la mise à jour")
        }
        _ => format!("storingUnicorns {current} — vérifier les mises à jour"),
    }
}

/// What the status bar shows next to the version while an update is under
/// way.
#[derive(Debug, PartialEq)]
enum UpdateChip {
    /// An accent button (available, ready).
    Button { label: String, hover: String },
    /// A spinner and a link (downloading).
    Busy { label: String, hover: String },
}

fn update_chip(state: &UpdateState) -> Option<UpdateChip> {
    match state {
        UpdateState::Available(info) => Some(UpdateChip::Button {
            label: format!("{} {} disponible", icon::ARROW_CIRCLE_UP, info.version),
            hover: format!(
                "La version {} est disponible : cliquer pour la voir",
                info.version
            ),
        }),
        UpdateState::Downloading(info) => Some(UpdateChip::Busy {
            label: "Téléchargement…".into(),
            hover: format!(
                "Téléchargement de la version {} : cliquer pour suivre",
                info.version
            ),
        }),
        UpdateState::Ready(info) => Some(UpdateChip::Button {
            label: format!("{} Redémarrer pour {}", icon::ARROW_CLOCKWISE, info.version),
            hover: format!(
                "La version {} est installée : cliquer pour redémarrer",
                info.version
            ),
        }),
        _ => None,
    }
}

/// The running version (a link) and, while an update is under way, its
/// chip; whether one of them was clicked.
fn version_row(ui: &mut egui::Ui, state: &UpdateState, current: &str) -> bool {
    let mut clicked = ui
        .link(RichText::new(format!("v{current}")).weak())
        .on_hover_text(version_hover(state, current))
        .clicked();
    match update_chip(state) {
        Some(UpdateChip::Button { label, hover }) => {
            let chip = egui::Button::new(RichText::new(label).color(egui::Color32::WHITE))
                .fill(theme::ACCENT)
                .small();
            clicked |= ui.add(chip).on_hover_text(hover).clicked();
        }
        Some(UpdateChip::Busy { label, hover }) => {
            ui.spinner();
            clicked |= ui.link(label).on_hover_text(hover).clicked();
        }
        None => {}
    }
    clicked
}

/// The running version, at the far left, and the update chip: a click
/// opens the update modal (checking first when nothing was found yet).
fn version(app: &mut App, ui: &mut egui::Ui) {
    if version_row(ui, &app.updater.state, crate::updater::current_version()) {
        update::open(app);
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

    fn release(version: &str) -> crate::updater::ReleaseInfo {
        let json = format!(
            r#"{{"version":"{version}","notes":"","page_url":"p","assets":[{{"name":"storingUnicorns-setup.msi","url":"u","sha256":"00"}}]}}"#
        );
        crate::updater::release_from_manifest_for_tests(&json)
            .unwrap()
            .unwrap()
    }

    fn updater(state: UpdateState) -> Updater {
        let mut u = Updater::new(|| {});
        u.state = state;
        u
    }

    #[test]
    fn banner_shows_found_updates_unless_put_off_or_the_modal_is_open() {
        for state in [
            UpdateState::Available(release("99.0.0")),
            UpdateState::Ready(release("99.0.0")),
        ] {
            let mut u = updater(state);
            assert!(banner_shown(&u, false));
            assert!(!banner_shown(&u, true), "the modal shows it");
            u.later();
            assert!(!banner_shown(&u, false));
        }
        for state in [
            UpdateState::Idle,
            UpdateState::Checking,
            UpdateState::UpToDate,
            UpdateState::Downloading(release("99.0.0")),
            UpdateState::Failed("boom".into()),
        ] {
            assert!(!banner_shown(&updater(state), false));
        }
    }

    #[test]
    fn chip_next_to_the_version() {
        assert_eq!(
            update_chip(&UpdateState::Available(release("0.18.2"))),
            Some(UpdateChip::Button {
                label: format!("{} 0.18.2 disponible", icon::ARROW_CIRCLE_UP),
                hover: "La version 0.18.2 est disponible : cliquer pour la voir".into(),
            })
        );
        assert_eq!(
            update_chip(&UpdateState::Ready(release("0.18.2"))),
            Some(UpdateChip::Button {
                label: format!("{} Redémarrer pour 0.18.2", icon::ARROW_CLOCKWISE),
                hover: "La version 0.18.2 est installée : cliquer pour redémarrer".into(),
            })
        );
        assert!(matches!(
            update_chip(&UpdateState::Downloading(release("0.18.2"))),
            Some(UpdateChip::Busy { label, .. }) if label == "Téléchargement…"
        ));
        for state in [
            UpdateState::Idle,
            UpdateState::Checking,
            UpdateState::UpToDate,
            UpdateState::Failed("boom".into()),
        ] {
            assert_eq!(update_chip(&state), None);
        }
    }

    #[test]
    fn version_hover_follows_the_state() {
        assert_eq!(
            version_hover(&UpdateState::Idle, "0.13.0"),
            "storingUnicorns 0.13.0 — vérifier les mises à jour"
        );
        assert_eq!(
            version_hover(&UpdateState::Failed("boom".into()), "0.13.0"),
            "storingUnicorns 0.13.0 — dernière vérification en échec : boom"
        );
        assert_eq!(
            version_hover(&UpdateState::Ready(release("99.0.0")), "0.13.0"),
            "storingUnicorns 0.13.0 — voir la mise à jour"
        );
    }

    #[test]
    fn no_status_message_while_the_modal_shows_the_outcome() {
        let ev = UpdateEvent::Error("boom".into());
        assert_eq!(
            update_message(&ev, false),
            Some((StatusKind::Error, "Mise à jour : boom".into()))
        );
        assert_eq!(update_message(&ev, true), None);
        assert_eq!(update_message(&UpdateEvent::UpToDate, true), None);
        assert_eq!(
            update_message(&UpdateEvent::Ready, false).map(|m| m.0),
            Some(StatusKind::Success)
        );
    }

    #[test]
    fn clicking_the_version_or_its_chip_opens_the_modal() {
        use crate::gui::ui_test::{click, Driver};
        let mut d = Driver::new();
        let idle = UpdateState::Idle;
        d.panel(vec![], |ui| version_row(ui, &idle, "0.18.1"));
        let link = d.widget("v0.18.1").expect("version link");
        assert!(d.panel(click(link), |ui| version_row(ui, &idle, "0.18.1")));
        assert!(!d.panel(vec![], |ui| version_row(ui, &idle, "0.18.1")));

        let ready = UpdateState::Ready(release("0.18.2"));
        d.panel(vec![], |ui| version_row(ui, &ready, "0.18.1"));
        let chip = d.button("Redémarrer pour 0.18.2").expect("chip");
        assert!(d.panel(click(chip.center), |ui| version_row(ui, &ready, "0.18.1")));
    }

    #[test]
    fn one_line_keeps_the_first_line_only() {
        assert_eq!(one_line("  ok  "), "ok");
        assert_eq!(one_line("error: x\n  detail\nmore"), "error: x …");
        assert_eq!(one_line("\nfirst\r\nsecond"), "first …");
    }
}
