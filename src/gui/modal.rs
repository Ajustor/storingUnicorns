//! The one standard modal: every dialog draws its content through [`show`]
//! (or [`confirm`]) inside the shared `egui::Modal` of `dialogs`, so that
//! all of them have the same header, width per role, scrolling body,
//! message line, footer buttons and keys.

use egui::{Align, Key, Layout, Modifiers, RichText};
use egui_phosphor::regular as icon;

use super::popup;
use super::theme::{ACCENT, ERROR, SUCCESS};

/// Label of the cancel button every modal shows (but information ones).
pub const CANCEL: &str = "Annuler";
/// Width of a [`ModalSize::Small`] modal (confirmations).
pub const SMALL_WIDTH: f32 = 440.0;
/// Width of a [`ModalSize::Medium`] modal (forms).
pub const MEDIUM_WIDTH: f32 = 640.0;
/// Room kept between a modal and the window's edges.
const WINDOW_MARGIN: f32 = 32.0;

/// Width of a modal, by role.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModalSize {
    /// Confirmations: 440 points.
    Small,
    /// Forms (connection, structure, export / import): 640 points.
    Medium,
    /// Lists (history, table search): [`popup::modal_width`].
    Large,
}

impl ModalSize {
    /// Width of the modal's content in a window `screen_width` wide: the
    /// role's width, never wider than the window (less a margin).
    pub fn width(self, screen_width: f32) -> f32 {
        let wanted = match self {
            ModalSize::Small => SMALL_WIDTH,
            ModalSize::Medium => MEDIUM_WIDTH,
            ModalSize::Large => popup::modal_width(screen_width),
        };
        wanted.min(screen_width - WINDOW_MARGIN).max(0.0)
    }
}

/// Highest the scrolling body of a modal grows: 75 % of the window, and
/// never more than what the header, message line and footer (`chrome`)
/// leave, so that they stay in view in a small window.
pub fn body_max_height(screen_height: f32, chrome: f32) -> f32 {
    (screen_height * 0.75)
        .min(screen_height - WINDOW_MARGIN - chrome)
        .max(0.0)
}

/// Look of a footer button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonKind {
    /// The main action: filled with the accent colour, triggered by Enter.
    Primary,
    /// Another, harmless action.
    Secondary,
    /// A destructive action: red, never triggered by Enter.
    Danger,
}

/// A footer button answering `action` when clicked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModalButton<T> {
    pub label: String,
    pub kind: ButtonKind,
    pub action: T,
    pub enabled: bool,
    /// Tooltip (also shown when the button is disabled, to say why).
    pub hint: Option<String>,
}

impl<T> ModalButton<T> {
    pub fn new(label: impl Into<String>, kind: ButtonKind, action: T) -> Self {
        Self {
            label: label.into(),
            kind,
            action,
            enabled: true,
            hint: None,
        }
    }

    pub fn primary(label: impl Into<String>, action: T) -> Self {
        Self::new(label, ButtonKind::Primary, action)
    }

    pub fn secondary(label: impl Into<String>, action: T) -> Self {
        Self::new(label, ButtonKind::Secondary, action)
    }

    pub fn danger(label: impl Into<String>, action: T) -> Self {
        Self::new(label, ButtonKind::Danger, action)
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

/// Header and size of a modal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModalSpec<'a> {
    pub title: &'a str,
    /// A phosphor icon shown before the title.
    pub icon: Option<&'a str>,
    pub size: ModalSize,
    /// Label of the cancel button, leftmost of the footer ([`CANCEL`] by
    /// default); `None` for an information modal (Escape and × still
    /// cancel).
    pub cancel: Option<&'a str>,
}

impl<'a> ModalSpec<'a> {
    pub fn new(title: &'a str, size: ModalSize) -> Self {
        Self {
            title,
            icon: None,
            size,
            cancel: Some(CANCEL),
        }
    }

    pub fn icon(mut self, icon: &'a str) -> Self {
        self.icon = Some(icon);
        self
    }
}

/// What the user did with a modal this frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModalResponse<T> {
    /// Nothing yet: keep the modal open.
    None,
    /// This button was clicked (or Enter chose the primary one).
    Button(T),
    /// Cancel, ×, Escape (or a click outside, mapped by `dialogs`).
    Cancel,
}

impl<T> ModalResponse<T> {
    /// The same response with the button's action mapped by `f`.
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> ModalResponse<U> {
        match self {
            ModalResponse::None => ModalResponse::None,
            ModalResponse::Button(t) => ModalResponse::Button(f(t)),
            ModalResponse::Cancel => ModalResponse::Cancel,
        }
    }
}

/// Kind of the message line above the footer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageKind {
    Error,
    Info,
    Success,
}

/// Draw one standard modal inside the shared `egui::Modal`: header (icon,
/// title, × close button), separator, scrollable body (at most
/// [`body_max_height`]), optional message line, separator, footer with the
/// cancel button and `buttons` right-aligned (see [`footer_slots`]).
///
/// Enter triggers the first enabled Primary button unless a widget keeps
/// the keyboard focus after the body (a multiline text edit, a focused
/// button) or a popup (combo box, completion) is open; Escape (unless a
/// popup is open) and × give [`ModalResponse::Cancel`]. The width is fixed
/// by `spec.size` and bounded by the window.
pub fn show<T: Clone>(
    ui: &mut egui::Ui,
    spec: ModalSpec,
    body: impl FnOnce(&mut egui::Ui),
    message: Option<(MessageKind, &str)>,
    buttons: &[ModalButton<T>],
) -> ModalResponse<T> {
    let screen = ui.ctx().screen_rect();
    ui.set_width(spec.size.width(screen.width()));
    let popup_before = ui.memory(|m| m.any_popup_open());
    let fresh = first_frame(ui, spec.title);
    let mut answer = ModalResponse::None;

    ui.horizontal(|ui| {
        let title = match spec.icon {
            Some(icon) => format!("{icon} {}", spec.title),
            None => spec.title.to_owned(),
        };
        ui.heading(title);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let close = egui::Button::new(icon::X).frame(false);
            if ui.add(close).on_hover_text("Fermer (Échap)").clicked() {
                answer = ModalResponse::Cancel;
            }
        });
    });
    ui.separator();
    // Room left for the body: the window less the header drawn above, the
    // footer (separator and a row of buttons), the message line and the
    // frame's margins.
    let chrome = {
        let s = ui.spacing();
        let line = s.interact_size.y + s.item_spacing.y;
        let header = ui.min_rect().height() + s.item_spacing.y;
        let separator = 6.0 + s.item_spacing.y;
        let message = message.map_or(0.0, |(_, text)| {
            message_height(ui, text) + 4.0 + s.item_spacing.y
        });
        header + separator + line + message + s.menu_margin.sum().y
    };
    egui::ScrollArea::vertical()
        .id_salt("modal-body")
        .max_height(body_max_height(screen.height(), chrome))
        .min_scrolled_height(body_max_height(screen.height(), chrome))
        .auto_shrink([false, true])
        .show(ui, body);
    if let Some((kind, text)) = message {
        ui.add_space(4.0);
        let text = RichText::new(text);
        let text = match kind {
            MessageKind::Error => text.color(ERROR),
            MessageKind::Success => text.color(SUCCESS),
            MessageKind::Info => text.weak(),
        };
        // At most a few lines: a longer message scrolls.
        egui::ScrollArea::vertical()
            .id_salt("modal-message")
            .max_height(message_max_height(ui))
            // egui's default (64) would be taller than three lines.
            .min_scrolled_height(message_max_height(ui))
            .auto_shrink([false, true])
            .show(ui, |ui| ui.label(text));
    }
    ui.separator();
    // One row exactly: a bare right-to-left layout would take (and centre
    // the buttons in) all the height left in the window.
    let row = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
    ui.allocate_ui_with_layout(row, Layout::right_to_left(Align::Center), |ui| {
        // Right to left: the last slot is drawn first.
        for slot in footer_slots(spec.cancel.is_some(), buttons)
            .into_iter()
            .rev()
        {
            let clicked = match slot {
                Slot::Cancel => ui.button(spec.cancel.unwrap_or(CANCEL)).clicked(),
                Slot::Button(i) => button(ui, &buttons[i]).clicked(),
            };
            if clicked {
                answer = match slot {
                    Slot::Cancel => ModalResponse::Cancel,
                    Slot::Button(i) => ModalResponse::Button(buttons[i].action.clone()),
                };
            }
        }
    });

    if !matches!(answer, ModalResponse::None) {
        return answer;
    }
    let (escape, enter) = ui.input(|i| {
        (
            i.key_pressed(Key::Escape),
            i.key_pressed(Key::Enter) && i.modifiers.is_none(),
        )
    });
    release_focus_behind(ui);
    let (focused, popup_after) = ui.memory(|m| (m.focused().is_some(), m.any_popup_open()));
    let popup_open = popup_before || popup_after;
    match key_outcome(
        Keys {
            escape,
            enter,
            popup_open,
            focused,
            fresh,
        },
        enter_target(buttons),
    ) {
        KeyOutcome::None => ModalResponse::None,
        KeyOutcome::Cancel => {
            ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
            ModalResponse::Cancel
        }
        KeyOutcome::Button(i) => {
            ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
            ModalResponse::Button(buttons[i].action.clone())
        }
    }
}

/// Whether this is the first frame the modal titled `title` is drawn:
/// it was not drawn on the previous pass.
fn first_frame(ui: &egui::Ui, title: &str) -> bool {
    let id = egui::Id::new(ui.layer_id()).with("modal-pass").with(title);
    let pass = ui.ctx().cumulative_pass_nr();
    let last: Option<u64> = ui.data_mut(|d| {
        let last = d.get_temp(id);
        d.insert_temp(id, pass);
        last
    });
    !matches!(last, Some(p) if p + 1 >= pass)
}

/// A widget behind the modal (a console, a grid cell) keeps the keyboard
/// focus in egui: it would get the keys meant for the modal, and Enter
/// would not reach the footer. Take it away (not while a popup, which has
/// its own layer, is open).
fn release_focus_behind(ui: &egui::Ui) {
    let ctx = ui.ctx();
    let Some(id) = ctx.memory(|m| m.focused()) else {
        return;
    };
    let layer = ctx.read_response(id).map(|r| r.layer_id);
    if layer != Some(ui.layer_id()) {
        ctx.memory_mut(|m| m.surrender_focus(id));
    }
}

/// Highest the message line grows (three lines) before it scrolls.
fn message_max_height(ui: &egui::Ui) -> f32 {
    3.0 * ui.text_style_height(&egui::TextStyle::Body)
}

/// Height of the message `text` wrapped to the modal's width, at most
/// [`message_max_height`].
fn message_height(ui: &egui::Ui, text: &str) -> f32 {
    let galley = egui::WidgetText::from(text).into_galley(
        ui,
        Some(egui::TextWrapMode::Wrap),
        ui.available_width(),
        egui::TextStyle::Body,
    );
    galley.size().y.min(message_max_height(ui))
}

/// Standard confirmation (small): `text`, then `items` as a bullet list,
/// then Cancel and the `confirm` button (Primary or Danger).
pub fn confirm<T: Clone>(
    ui: &mut egui::Ui,
    title: &str,
    text: &str,
    items: &[String],
    confirm: ModalButton<T>,
) -> ModalResponse<T> {
    show(
        ui,
        ModalSpec::new(title, ModalSize::Small),
        |ui| {
            ui.label(text);
            if !items.is_empty() {
                ui.add_space(4.0);
                for item in items {
                    ui.label(format!("• {item}"));
                }
            }
        },
        None,
        std::slice::from_ref(&confirm),
    )
}

fn button<T>(ui: &mut egui::Ui, b: &ModalButton<T>) -> egui::Response {
    let widget = match b.kind {
        ButtonKind::Secondary => egui::Button::new(&b.label),
        ButtonKind::Primary => {
            egui::Button::new(RichText::new(&b.label).color(egui::Color32::WHITE)).fill(ACCENT)
        }
        ButtonKind::Danger => {
            egui::Button::new(RichText::new(&b.label).color(egui::Color32::WHITE)).fill(ERROR)
        }
    };
    let response = ui.add_enabled(b.enabled, widget);
    match &b.hint {
        Some(hint) => response.on_hover_text(hint).on_disabled_hover_text(hint),
        None => response,
    }
}

/// A place in the footer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slot {
    Cancel,
    /// Index in the buttons given to [`show`].
    Button(usize),
}

/// Footer, left to right: the cancel button (when there is one), then the
/// buttons by kind — Secondary, Danger, Primary — keeping the given order
/// within a kind, so that the main action is always rightmost.
fn footer_slots<T>(cancel: bool, buttons: &[ModalButton<T>]) -> Vec<Slot> {
    let rank = |k: ButtonKind| match k {
        ButtonKind::Secondary => 0,
        ButtonKind::Danger => 1,
        ButtonKind::Primary => 2,
    };
    let mut order: Vec<usize> = (0..buttons.len()).collect();
    order.sort_by_key(|&i| rank(buttons[i].kind));
    cancel
        .then_some(Slot::Cancel)
        .into_iter()
        .chain(order.into_iter().map(Slot::Button))
        .collect()
}

/// The button Enter triggers: the first enabled Primary one.
fn enter_target<T>(buttons: &[ModalButton<T>]) -> Option<usize> {
    buttons
        .iter()
        .position(|b| b.kind == ButtonKind::Primary && b.enabled)
}

/// Keyboard state of a frame, once the body is drawn.
#[derive(Clone, Copy, Debug, Default)]
struct Keys {
    escape: bool,
    /// Enter without modifiers.
    enter: bool,
    /// A popup (combo box, completion) is open: the keys are its own.
    popup_open: bool,
    /// A widget still has the keyboard focus (multiline text edit, focused
    /// button): Enter is its own. A single-line text edit gives the focus
    /// up on Enter, so Enter there submits the modal.
    focused: bool,
    /// First frame of the modal: an Enter now is the one that opened it
    /// (e.g. in a field behind it), not an answer.
    fresh: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeyOutcome {
    None,
    Cancel,
    Button(usize),
}

fn key_outcome(keys: Keys, primary: Option<usize>) -> KeyOutcome {
    if keys.popup_open {
        return KeyOutcome::None;
    }
    if keys.escape {
        return KeyOutcome::Cancel;
    }
    match primary {
        Some(i) if keys.enter && !keys.focused && !keys.fresh => KeyOutcome::Button(i),
        _ => KeyOutcome::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_follows_the_role_within_the_window() {
        assert_eq!(ModalSize::Small.width(1920.0), 440.0);
        assert_eq!(ModalSize::Medium.width(1920.0), 640.0);
        assert_eq!(ModalSize::Large.width(1920.0), 900.0);
        assert_eq!(ModalSize::Large.width(1000.0), 800.0);
        // Small windows: never wider than the window, less a margin.
        assert_eq!(ModalSize::Medium.width(600.0), 568.0);
        assert_eq!(ModalSize::Small.width(400.0), 368.0);
        assert_eq!(ModalSize::Large.width(300.0), 268.0);
        assert_eq!(ModalSize::Small.width(10.0), 0.0);
    }

    #[test]
    fn body_takes_at_most_three_quarters_of_the_window() {
        assert_eq!(body_max_height(800.0, 100.0), 600.0);
        // Small windows: the header and the footer (`chrome`) keep their room.
        assert_eq!(body_max_height(480.0, 100.0), 480.0 - 32.0 - 100.0);
        assert_eq!(body_max_height(100.0, 100.0), 0.0);
    }

    #[test]
    fn egui_a_tall_body_scrolls_and_the_footer_stays_in_a_small_window() {
        let long = "Erreur : la connexion a échoué. ".repeat(40);
        for (w, h, message) in [
            (720.0, 480.0, "Erreur"),
            (1280.0, 800.0, "Erreur"),
            // A long message is capped (it scrolls): the footer stays in view.
            (720.0, 480.0, long.as_str()),
        ] {
            let ctx = egui::Context::default();
            let mut footer = None;
            for _ in 0..3 {
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(w, h),
                    )),
                    ..Default::default()
                };
                let _ = ctx.run(input, |ctx| {
                    let modal = egui::Modal::new(egui::Id::new("t")).show(ctx, |ui| {
                        let _ = show(
                            ui,
                            ModalSpec::new("Titre", ModalSize::Large),
                            |ui| {
                                for i in 0..200 {
                                    ui.label(format!("ligne {i}"));
                                }
                            },
                            Some((MessageKind::Error, message)),
                            &buttons(),
                        );
                    });
                    footer = Some(modal.response.rect);
                });
            }
            let rect = footer.unwrap();
            assert!(
                rect.top() >= WINDOW_MARGIN / 2.0 - 1.0
                    && rect.bottom() <= h - WINDOW_MARGIN / 2.0 + 1.0,
                "{w}x{h} ({} chars): the modal {rect:?} fits in the window",
                message.len()
            );
            assert!(rect.height() > h * 0.6, "{w}x{h}: the body uses the room");
        }
    }

    fn frame(ctx: &egui::Context, events: Vec<egui::Event>, f: impl FnMut(&egui::Context)) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 800.0),
            )),
            events,
            focused: true,
            ..Default::default()
        };
        let _ = ctx.run(input, f);
    }

    #[test]
    fn egui_the_enter_that_opens_a_modal_does_not_press_its_button() {
        let ctx = egui::Context::default();
        let field = egui::Id::new("where");
        let mut text = "id > 100".to_string();
        let mut open = false;
        let mut answers = vec![];
        for events in [vec![], vec![key(Key::Enter)], vec![key(Key::Enter)]] {
            frame(&ctx, events, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let r = ui.add(egui::TextEdit::singleline(&mut text).id(field));
                    if !open && !r.lost_focus() {
                        r.request_focus();
                    }
                    // Like a data tab: Enter in the field asks to confirm.
                    if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        open = true;
                    }
                });
                if open {
                    let modal = egui::Modal::new(egui::Id::new("dialog")).show(ctx, |ui| {
                        show(
                            ui,
                            ModalSpec::new("T", ModalSize::Small),
                            |_| {},
                            None,
                            &buttons(),
                        )
                    });
                    answers.push(modal.inner);
                }
            });
        }
        assert_eq!(
            answers,
            vec![ModalResponse::None, ModalResponse::Button(2)],
            "first frame ignored, then Enter submits"
        );
    }

    #[test]
    fn egui_a_modal_takes_the_focus_from_the_widgets_behind_it() {
        let ctx = egui::Context::default();
        let console = egui::Id::new("console");
        let mut text = String::from("select 1");
        let mut answers = vec![];
        for (n, events) in [vec![], vec![], vec![key(Key::Enter)]]
            .into_iter()
            .enumerate()
        {
            frame(&ctx, events, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let r = ui.add(egui::TextEdit::multiline(&mut text).id(console));
                    if n == 0 {
                        r.request_focus();
                    }
                });
                if n > 0 {
                    let modal = egui::Modal::new(egui::Id::new("dialog")).show(ctx, |ui| {
                        show(
                            ui,
                            ModalSpec::new("T", ModalSize::Small),
                            |_| {},
                            None,
                            &buttons(),
                        )
                    });
                    answers.push(modal.inner);
                }
            });
            if n == 1 {
                assert_eq!(
                    ctx.memory(|m| m.focused()),
                    None,
                    "the console lost the focus"
                );
            }
        }
        assert_eq!(text, "select 1", "Enter did not leak to the console");
        assert_eq!(answers, vec![ModalResponse::None, ModalResponse::Button(2)]);
    }

    #[test]
    fn button_constructors() {
        let b = ModalButton::danger("Supprimer", 1).hint("Définitif");
        assert_eq!(b.kind, ButtonKind::Danger);
        assert_eq!(b.label, "Supprimer");
        assert!(b.enabled);
        assert_eq!(b.hint.as_deref(), Some("Définitif"));
        assert!(
            !ModalButton::primary("Enregistrer", 2)
                .enabled(false)
                .enabled
        );
        assert_eq!(
            ModalButton::secondary("Tester", 3).kind,
            ButtonKind::Secondary
        );
        let spec = ModalSpec::new("Titre", ModalSize::Small);
        assert_eq!(spec.cancel, Some(CANCEL));
        assert_eq!(spec.icon, None);
        assert_eq!(spec.icon("i").icon, Some("i"));
    }

    #[test]
    fn footer_puts_cancel_left_and_the_primary_action_right() {
        let buttons = [
            ModalButton::primary("Submit", 'p'),
            ModalButton::danger("Abandonner", 'd'),
            ModalButton::secondary("Tester", 's'),
            ModalButton::secondary("Autre", 't'),
        ];
        assert_eq!(
            footer_slots(true, &buttons),
            vec![
                Slot::Cancel,
                Slot::Button(2),
                Slot::Button(3),
                Slot::Button(1),
                Slot::Button(0)
            ]
        );
        let info = [ModalButton::primary("OK", ())];
        assert_eq!(footer_slots(false, &info), vec![Slot::Button(0)]);
        assert_eq!(footer_slots::<()>(true, &[]), vec![Slot::Cancel]);
    }

    #[test]
    fn enter_targets_the_first_enabled_primary_button() {
        let buttons = [
            ModalButton::danger("Rollback", 0),
            ModalButton::primary("Commit", 1).enabled(false),
            ModalButton::primary("Autre", 2),
        ];
        assert_eq!(enter_target(&buttons), Some(2));
        // A confirmation of a destructive action has no Enter target.
        assert_eq!(enter_target(&[ModalButton::danger("Vider", 0)]), None);
    }

    #[test]
    fn keys_decide_cancel_and_primary() {
        let k = |escape, enter, popup_open, focused| Keys {
            escape,
            enter,
            popup_open,
            focused,
            fresh: false,
        };
        // The frame a modal appears, the Enter that opened it is not for it.
        let fresh = Keys {
            fresh: true,
            ..k(false, true, false, false)
        };
        assert_eq!(key_outcome(fresh, Some(0)), KeyOutcome::None);
        let fresh_escape = Keys {
            fresh: true,
            ..k(true, false, false, false)
        };
        assert_eq!(key_outcome(fresh_escape, Some(0)), KeyOutcome::Cancel);
        assert_eq!(
            key_outcome(k(false, false, false, false), Some(0)),
            KeyOutcome::None
        );
        assert_eq!(
            key_outcome(k(false, true, false, false), Some(1)),
            KeyOutcome::Button(1)
        );
        assert_eq!(
            key_outcome(k(false, true, false, false), None),
            KeyOutcome::None
        );
        // A multiline text edit (or a focused button) keeps Enter.
        assert_eq!(
            key_outcome(k(false, true, false, true), Some(0)),
            KeyOutcome::None
        );
        // Escape cancels, even from a text edit.
        assert_eq!(
            key_outcome(k(true, false, false, true), Some(0)),
            KeyOutcome::Cancel
        );
        assert_eq!(
            key_outcome(k(true, true, false, false), Some(0)),
            KeyOutcome::Cancel
        );
        // An open popup (combo, completion) owns both keys.
        assert_eq!(
            key_outcome(k(true, false, true, false), Some(0)),
            KeyOutcome::None
        );
        assert_eq!(
            key_outcome(k(false, true, true, false), Some(0)),
            KeyOutcome::None
        );
    }

    // --- Through egui -----------------------------------------------------

    fn key(key: Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }
    }

    /// Show the modal once per entry of `frames` (the events of that
    /// frame), with `body` drawn in it; the responses of every frame.
    fn run(
        frames: Vec<Vec<egui::Event>>,
        buttons: &[ModalButton<u8>],
        mut body: impl FnMut(&mut egui::Ui, usize),
    ) -> Vec<ModalResponse<u8>> {
        let ctx = egui::Context::default();
        let mut out = vec![];
        for (n, events) in frames.into_iter().enumerate() {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 800.0),
                )),
                events,
                focused: true,
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                let modal = egui::Modal::new(egui::Id::new("t")).show(ctx, |ui| {
                    show(
                        ui,
                        ModalSpec::new("Titre", ModalSize::Small),
                        |ui| body(ui, n),
                        None,
                        buttons,
                    )
                });
                out.push(modal.inner);
            });
        }
        out
    }

    fn buttons() -> Vec<ModalButton<u8>> {
        vec![
            ModalButton::danger("Abandonner", 1),
            ModalButton::primary("Submit", 2),
        ]
    }

    #[test]
    fn egui_enter_triggers_the_primary_button_and_escape_cancels() {
        let r = run(vec![vec![], vec![key(Key::Enter)]], &buttons(), |_, _| {});
        assert_eq!(r, vec![ModalResponse::None, ModalResponse::Button(2)]);
        let r = run(vec![vec![], vec![key(Key::Escape)]], &buttons(), |_, _| {});
        assert_eq!(r[1], ModalResponse::Cancel);
        let danger = [ModalButton::danger("Vider", 1)];
        let r = run(vec![vec![], vec![key(Key::Enter)]], &danger, |_, _| {});
        assert_eq!(
            r[1],
            ModalResponse::None,
            "Enter never confirms a destructive action"
        );
    }

    #[test]
    fn egui_escape_is_consumed_so_the_modal_does_not_also_close_twice() {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            events: vec![key(Key::Escape)],
            ..Default::default()
        };
        let mut closes = None;
        let _ = ctx.run(input, |ctx| {
            let modal = egui::Modal::new(egui::Id::new("t")).show(ctx, |ui| {
                show(
                    ui,
                    ModalSpec::new("T", ModalSize::Small),
                    |_| {},
                    None,
                    &buttons(),
                )
            });
            let close = modal.should_close();
            closes = Some((modal.inner, close));
        });
        assert_eq!(closes, Some((ModalResponse::Cancel, false)));
    }

    #[test]
    fn egui_enter_in_a_multiline_text_edit_stays_there() {
        let mut text = String::new();
        let r = run(vec![vec![], vec![key(Key::Enter)]], &buttons(), |ui, n| {
            let r = ui.add(egui::TextEdit::multiline(&mut text));
            if n == 0 {
                r.request_focus();
            }
        });
        assert_eq!(r[1], ModalResponse::None);
    }

    #[test]
    fn egui_enter_in_a_single_line_text_edit_submits() {
        let mut text = String::new();
        let r = run(vec![vec![], vec![key(Key::Enter)]], &buttons(), |ui, n| {
            let r = ui.add(egui::TextEdit::singleline(&mut text));
            if n == 0 {
                r.request_focus();
            }
        });
        assert_eq!(r[1], ModalResponse::Button(2));
    }

    #[test]
    fn egui_an_open_popup_owns_enter_and_escape() {
        let popup = egui::Id::new("combo");
        for k in [Key::Enter, Key::Escape] {
            let r = run(vec![vec![], vec![key(k)]], &buttons(), |ui, _| {
                ui.memory_mut(|m| m.open_popup(popup));
            });
            assert_eq!(r[1], ModalResponse::None, "{k:?}");
        }
    }
}
