//! « Mise à jour »: the whole update flow in one modal, opened from the
//! version in the status bar (or the banner of an update found at
//! startup): check, release notes, install, restart. It follows the
//! `Updater`'s state live; closing it never stops a download.

use egui::text::LayoutJob;
use egui::{Align, FontSelection, RichText};
use egui_phosphor::regular as icon;

use super::Dialog;
use crate::gui::app::App;
use crate::gui::modal::{self, MessageKind, ModalButton, ModalResponse, ModalSize, ModalSpec};
use crate::updater::UpdateState;

pub const TITLE: &str = "Mise à jour";

/// Buttons of the modal (closing it — Plus tard, Fermer, Continuer en
/// arrière-plan, ×, Escape — is the cancel).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateAction {
    Install,
    Skip,
    Restart,
    Retry,
}

/// What the modal shows for one state of the updater.
#[derive(Debug, PartialEq)]
pub struct View {
    /// A spinner before `text` (checking, downloading).
    pub busy: bool,
    pub text: String,
    /// A weak line under `text`.
    pub hint: Option<&'static str>,
    /// Release notes (Markdown) and download page of an available update.
    pub notes: Option<String>,
    pub page_url: Option<String>,
    /// Error line above the footer.
    pub error: Option<String>,
    /// Label of the closing button.
    pub cancel: &'static str,
    pub buttons: Vec<ModalButton<UpdateAction>>,
}

impl View {
    fn new(text: String, cancel: &'static str) -> Self {
        Self {
            busy: false,
            text,
            hint: None,
            notes: None,
            page_url: None,
            error: None,
            cancel,
            buttons: vec![],
        }
    }
}

/// The modal's content for `state`, `current` being the running version.
pub fn view(state: &UpdateState, current: &str) -> View {
    match state {
        UpdateState::Idle | UpdateState::Checking => View {
            busy: true,
            ..View::new("Recherche d'une mise à jour…".into(), "Fermer")
        },
        UpdateState::UpToDate => View::new(
            format!("Vous utilisez la dernière version ({current})."),
            "Fermer",
        ),
        UpdateState::Available(info) => View {
            notes: Some(info.notes.clone()),
            page_url: Some(info.page_url.clone()),
            buttons: vec![
                ModalButton::primary("Installer", UpdateAction::Install),
                ModalButton::secondary("Ignorer cette version", UpdateAction::Skip)
                    .hint("Ne plus proposer cette version au démarrage"),
            ],
            ..View::new(
                format!(
                    "La version {} est disponible (vous avez {current}).",
                    info.version
                ),
                "Plus tard",
            )
        },
        UpdateState::Downloading(info) => View {
            busy: true,
            hint: Some("Vous pouvez fermer cette fenêtre : le téléchargement continue."),
            ..View::new(
                format!("Téléchargement de la version {}…", info.version),
                "Continuer en arrière-plan",
            )
        },
        UpdateState::Ready(info) => View {
            hint: Some("« Plus tard » : la mise à jour s'appliquera à la fermeture."),
            buttons: vec![ModalButton::primary(
                "Redémarrer maintenant",
                UpdateAction::Restart,
            )],
            ..View::new(
                format!(
                    "La version {} est installée. Redémarrez pour l'utiliser.",
                    info.version
                ),
                "Plus tard",
            )
        },
        UpdateState::Failed(e) => View {
            error: Some(e.clone()),
            buttons: vec![ModalButton::primary("Réessayer", UpdateAction::Retry)],
            ..View::new("La mise à jour a échoué.".into(), "Fermer")
        },
    }
}

/// Whether opening the modal on `state` starts a check: nothing found
/// (yet) or the last check / download failed. A found, downloading or
/// ready update is shown as it is.
pub fn needs_check(state: &UpdateState) -> bool {
    matches!(
        state,
        UpdateState::Idle | UpdateState::UpToDate | UpdateState::Failed(_)
    )
}

/// Open the modal (checking first when [`needs_check`]).
pub fn open(app: &mut App) {
    if needs_check(&app.updater.state) {
        app.updater.check(true, None);
    }
    app.dialog = Some(Dialog::Update);
}

/// Draw the modal for `state`.
pub fn ui(ui: &mut egui::Ui, state: &UpdateState, current: &str) -> ModalResponse<UpdateAction> {
    let v = view(state, current);
    let mut spec = ModalSpec::new(TITLE, ModalSize::Medium).icon(icon::ARROW_CIRCLE_UP);
    spec.cancel = Some(v.cancel);
    modal::show(
        ui,
        spec,
        |ui| {
            ui.horizontal(|ui| {
                if v.busy {
                    ui.spinner();
                }
                ui.label(&v.text);
            });
            if let Some(hint) = v.hint {
                ui.label(RichText::new(hint).weak().small());
            }
            if let Some(notes) = &v.notes {
                ui.add_space(8.0);
                notes_ui(ui, notes);
            }
            if let Some(url) = &v.page_url {
                ui.add_space(8.0);
                ui.hyperlink_to(format!("{} Voir sur la page", icon::ARROW_SQUARE_OUT), url);
            }
        },
        v.error.as_deref().map(|e| (MessageKind::Error, e)),
        &v.buttons,
    )
}

/// Do `action` (`None`: the modal was closed); whether the modal stays
/// open.
pub fn act(app: &mut App, ctx: &egui::Context, action: Option<UpdateAction>) -> bool {
    match action {
        Some(UpdateAction::Install) => {
            app.updater.install();
            true
        }
        Some(UpdateAction::Retry) => {
            app.updater.check(true, None);
            true
        }
        Some(UpdateAction::Skip) => {
            if let Some(version) = app.updater.skip() {
                app.info(format!("La version {version} ne sera plus proposée"));
                app.config.skipped_version = Some(version);
                app.save_config();
            }
            false
        }
        // Goes through the normal close: cancelling its confirmation drops
        // the relaunch (`cancel_restart`).
        Some(UpdateAction::Restart) => {
            app.updater.schedule_restart();
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            false
        }
        // Plus tard / Fermer / Continuer en arrière-plan: an available or
        // ready update keeps its status-bar button, not its banner.
        None => {
            app.updater.later();
            false
        }
    }
}

/// Look of a piece of release-notes text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpanStyle {
    Plain,
    Bold,
    Code,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub style: SpanStyle,
}

/// A block of release notes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    Heading(Vec<Span>),
    /// A bullet, `depth` 0 for the outermost list.
    Item {
        depth: usize,
        spans: Vec<Span>,
    },
    Paragraph(Vec<Span>),
}

/// The inline Markdown of the notes: `**bold**`, `` `code` `` and
/// `[text](url)` (shown as its text; the page has the links).
pub fn spans(text: &str) -> Vec<Span> {
    let mut out: Vec<Span> = vec![];
    let mut push = |text: &str, style| {
        if text.is_empty() {
            return;
        }
        match out.last_mut() {
            Some(last) if last.style == style => last.text.push_str(text),
            _ => out.push(Span {
                text: text.into(),
                style,
            }),
        }
    };
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        if let Some(inner) = rest.strip_prefix("**") {
            if let Some(end) = inner.find("**") {
                push(&inner[..end].replace('`', ""), SpanStyle::Bold);
                rest = &inner[end + 2..];
                continue;
            }
        }
        if let Some(inner) = rest.strip_prefix('`') {
            if let Some(end) = inner.find('`') {
                push(&inner[..end], SpanStyle::Code);
                rest = &inner[end + 1..];
                continue;
            }
        }
        if let Some(inner) = rest.strip_prefix('[') {
            if let Some(close) = inner.find("](") {
                if let Some(end) = inner[close + 2..].find(')') {
                    for s in spans(&inner[..close]) {
                        push(&s.text, s.style);
                    }
                    rest = &inner[close + 2 + end + 1..];
                    continue;
                }
            }
        }
        push(&rest[..c.len_utf8()], SpanStyle::Plain);
        rest = &rest[c.len_utf8()..];
    }
    out
}

/// The release notes (a CHANGELOG section) as blocks: `#` headings,
/// (nested) `-` / `*` bullets whose indented lines continue them, and
/// paragraphs.
pub fn blocks(md: &str) -> Vec<Block> {
    enum Kind {
        Heading,
        Item { indent: usize, depth: usize },
        Paragraph,
    }
    let mut raw: Vec<(Kind, String)> = vec![];
    // Indentation of each open list level.
    let mut levels: Vec<usize> = vec![];
    // The last block can be continued by the next line.
    let mut open = false;
    for line in md.lines() {
        let text = line.trim();
        let indent = line.len() - line.trim_start().len();
        if text.is_empty() {
            open = false;
            continue;
        }
        let bullet = text.strip_prefix("- ").or_else(|| text.strip_prefix("* "));
        if let Some(item) = bullet {
            while levels.last().is_some_and(|&l| l > indent) {
                levels.pop();
            }
            if levels.last() != Some(&indent) {
                levels.push(indent);
            }
            let depth = levels.len() - 1;
            raw.push((Kind::Item { indent, depth }, item.trim().into()));
            open = true;
            continue;
        }
        let hashes = text.chars().take_while(|&c| c == '#').count();
        if (1..=6).contains(&hashes) && text[hashes..].starts_with(' ') {
            levels.clear();
            raw.push((Kind::Heading, text[hashes..].trim().into()));
            open = false;
            continue;
        }
        let continues = match raw.last() {
            Some((Kind::Item { indent: i, .. }, _)) => open && indent > *i,
            Some((Kind::Paragraph, _)) => open,
            _ => false,
        };
        if continues {
            let last = &mut raw.last_mut().expect("continued").1;
            last.push(' ');
            last.push_str(text);
        } else {
            levels.clear();
            raw.push((Kind::Paragraph, text.into()));
            open = true;
        }
    }
    raw.into_iter()
        .map(|(kind, text)| match kind {
            Kind::Heading => Block::Heading(spans(&text)),
            Kind::Item { depth, .. } => Block::Item {
                depth,
                spans: spans(&text),
            },
            Kind::Paragraph => Block::Paragraph(spans(&text)),
        })
        .collect()
}

/// `spans` laid out as one wrapping text.
fn job(ui: &egui::Ui, spans: &[Span], strong: bool) -> LayoutJob {
    let mut job = LayoutJob::default();
    for s in spans {
        let text = RichText::new(&s.text);
        let text = match s.style {
            SpanStyle::Plain if !strong => text,
            SpanStyle::Plain | SpanStyle::Bold => text.strong(),
            SpanStyle::Code => text.code(),
        };
        text.append_to(&mut job, ui.style(), FontSelection::Default, Align::Center);
    }
    job
}

/// The release notes, readable: headings strong, bullets indented.
fn notes_ui(ui: &mut egui::Ui, md: &str) {
    let blocks = blocks(md);
    if blocks.is_empty() {
        ui.label(RichText::new("Pas de notes pour cette version.").weak());
        return;
    }
    for (i, block) in blocks.iter().enumerate() {
        match block {
            Block::Heading(spans) => {
                if i > 0 {
                    ui.add_space(6.0);
                }
                ui.label(job(ui, spans, true));
            }
            Block::Item { depth, spans } => {
                ui.horizontal_top(|ui| {
                    ui.add_space(8.0 + 16.0 * *depth as f32);
                    ui.label(if *depth == 0 { "•" } else { "◦" });
                    ui.add(egui::Label::new(job(ui, spans, false)).wrap());
                });
            }
            Block::Paragraph(spans) => {
                ui.add(egui::Label::new(job(ui, spans, false)).wrap());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::updater::ReleaseInfo;

    fn info(notes: &str) -> ReleaseInfo {
        let json = serde_json::json!({
            "version": "99.0.2",
            "notes": notes,
            "page_url": "https://p/",
            "assets": [{"name": "storingUnicorns-setup.msi", "url": "u", "sha256": "00"}],
        })
        .to_string();
        crate::updater::release_from_manifest_for_tests(&json)
            .unwrap()
            .unwrap()
    }

    fn labels(v: &View) -> Vec<&str> {
        v.buttons.iter().map(|b| b.label.as_str()).collect()
    }

    #[test]
    fn checking_and_up_to_date() {
        for state in [UpdateState::Idle, UpdateState::Checking] {
            let v = view(&state, "99.0.1");
            assert!(v.busy);
            assert_eq!(v.text, "Recherche d'une mise à jour…");
            assert_eq!(v.cancel, "Fermer");
            assert!(v.buttons.is_empty());
        }
        let v = view(&UpdateState::UpToDate, "99.0.1");
        assert!(!v.busy);
        assert_eq!(v.text, "Vous utilisez la dernière version (99.0.1).");
        assert_eq!(v.cancel, "Fermer");
        assert!(v.buttons.is_empty());
    }

    #[test]
    fn available_shows_notes_page_and_choices() {
        let v = view(&UpdateState::Available(info("### Corrections")), "99.0.1");
        assert_eq!(
            v.text,
            "La version 99.0.2 est disponible (vous avez 99.0.1)."
        );
        assert_eq!(v.notes.as_deref(), Some("### Corrections"));
        assert_eq!(v.page_url.as_deref(), Some("https://p/"));
        assert_eq!(labels(&v), ["Installer", "Ignorer cette version"]);
        assert_eq!(v.buttons[0].kind, modal::ButtonKind::Primary);
        assert_eq!(v.buttons[0].action, UpdateAction::Install);
        assert_eq!(v.buttons[1].action, UpdateAction::Skip);
        assert_eq!(v.cancel, "Plus tard");
    }

    #[test]
    fn downloading_ready_and_failed() {
        let v = view(&UpdateState::Downloading(info("")), "99.0.1");
        assert!(v.busy);
        assert_eq!(v.text, "Téléchargement de la version 99.0.2…");
        assert_eq!(v.cancel, "Continuer en arrière-plan");
        assert!(v.buttons.is_empty());
        assert!(v.notes.is_none());

        let v = view(&UpdateState::Ready(info("")), "99.0.1");
        assert_eq!(
            v.text,
            "La version 99.0.2 est installée. Redémarrez pour l'utiliser."
        );
        assert_eq!(labels(&v), ["Redémarrer maintenant"]);
        assert_eq!(v.buttons[0].kind, modal::ButtonKind::Primary);
        assert_eq!(v.buttons[0].action, UpdateAction::Restart);
        assert_eq!(v.cancel, "Plus tard");

        let v = view(&UpdateState::Failed("réseau".into()), "99.0.1");
        assert_eq!(v.error.as_deref(), Some("réseau"));
        assert_eq!(labels(&v), ["Réessayer"]);
        assert_eq!(v.buttons[0].action, UpdateAction::Retry);
        assert_eq!(v.cancel, "Fermer");
    }

    #[test]
    fn opening_checks_unless_an_update_is_under_way() {
        assert!(needs_check(&UpdateState::Idle));
        assert!(needs_check(&UpdateState::UpToDate));
        assert!(needs_check(&UpdateState::Failed("x".into())));
        assert!(!needs_check(&UpdateState::Checking));
        assert!(!needs_check(&UpdateState::Available(info(""))));
        assert!(!needs_check(&UpdateState::Downloading(info(""))));
        assert!(!needs_check(&UpdateState::Ready(info(""))));
    }

    fn plain(text: &str) -> Span {
        Span {
            text: text.into(),
            style: SpanStyle::Plain,
        }
    }

    #[test]
    fn inline_markdown_becomes_styled_spans() {
        assert_eq!(
            spans("**Scripts** : `F5` ou [la page](https://p/)."),
            vec![
                Span {
                    text: "Scripts".into(),
                    style: SpanStyle::Bold
                },
                plain(" : "),
                Span {
                    text: "F5".into(),
                    style: SpanStyle::Code
                },
                plain(" ou la page."),
            ]
        );
        // Code inside bold is just bold; unmatched markers stay as they are.
        assert_eq!(
            spans("**`GO`** a ** b ` c"),
            vec![
                Span {
                    text: "GO".into(),
                    style: SpanStyle::Bold
                },
                plain(" a ** b ` c"),
            ]
        );
        assert_eq!(spans(""), vec![]);
    }

    #[test]
    fn notes_become_headings_bullets_and_paragraphs() {
        let md = "### Corrections\n\
                  \n\
                  - **Scripts** : un script\n  \
                    s'exécute en lots.\n\
                  - Liste :\n  \
                    - imbriquée\n\
                  - fin\n\
                  \n\
                  Un paragraphe\n\
                  sur deux lignes.\n\
                  ## [0.1.0]";
        let b = blocks(md);
        assert_eq!(b.len(), 7, "{b:?}");
        assert_eq!(b[0], Block::Heading(vec![plain("Corrections")]));
        assert_eq!(
            b[1],
            Block::Item {
                depth: 0,
                spans: vec![
                    Span {
                        text: "Scripts".into(),
                        style: SpanStyle::Bold
                    },
                    plain(" : un script s'exécute en lots."),
                ]
            }
        );
        assert_eq!(
            b[3],
            Block::Item {
                depth: 1,
                spans: vec![plain("imbriquée")]
            }
        );
        assert_eq!(
            b[4],
            Block::Item {
                depth: 0,
                spans: vec![plain("fin")]
            }
        );
        assert_eq!(
            b[5],
            Block::Paragraph(vec![plain("Un paragraphe sur deux lignes.")])
        );
        assert_eq!(b[6], Block::Heading(vec![plain("[0.1.0]")]));
        assert!(blocks("  \n").is_empty());
    }

    #[test]
    fn modal_answers_with_its_buttons() {
        use crate::gui::ui_test::{click, Driver};
        let state = UpdateState::Available(info("- un\n- deux"));
        let mut d = Driver::new();
        // The first frame only sizes the modal.
        for _ in 0..2 {
            d.modal(vec![], |ui| ui_for(ui, &state));
        }
        let install = d.button("Installer").expect("Installer");
        let r = d.modal(click(install.center), |ui| ui_for(ui, &state));
        assert_eq!(r, ModalResponse::Button(UpdateAction::Install));
        let later = d.button("Plus tard").expect("Plus tard");
        let r = d.modal(click(later.center), |ui| ui_for(ui, &state));
        assert_eq!(r, ModalResponse::Cancel);

        let state = UpdateState::Downloading(info(""));
        d.modal(vec![], |ui| ui_for(ui, &state));
        assert!(d.button("Continuer en arrière-plan").is_some());
        assert!(d.button("Installer").is_none());
    }

    fn ui_for(ui: &mut egui::Ui, state: &UpdateState) -> ModalResponse<UpdateAction> {
        super::ui(ui, state, "99.0.1")
    }
}
