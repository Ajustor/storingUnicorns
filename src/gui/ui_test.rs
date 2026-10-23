//! Tests through egui: frames of input events, buttons found (and
//! clicked) by their label through the AccessKit tree.

use egui::accesskit::Role;
use egui::{Event, Key, Modifiers, OutputCommand, Pos2, RawInput, Rect};

/// One egui context driven frame by frame, in a 1280 × 800 window.
pub struct Driver {
    pub ctx: egui::Context,
    out: Option<egui::FullOutput>,
}

/// A button drawn by the last frame.
#[derive(Clone, Copy, Debug)]
pub struct Button {
    pub center: Pos2,
    pub enabled: bool,
}

impl Driver {
    pub fn new() -> Self {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        Self { ctx, out: None }
    }

    /// Run one frame with `events`, `f` drawing inside a modal like
    /// `dialogs::show` does; what `f` returned.
    pub fn modal<R>(&mut self, events: Vec<Event>, f: impl FnOnce(&mut egui::Ui) -> R) -> R {
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
            events,
            focused: true,
            ..Default::default()
        };
        let mut f = Some(f);
        let mut result = None;
        let out = self.ctx.run(input, |ctx| {
            if let Some(f) = f.take() {
                let modal = egui::Modal::new(egui::Id::new("dialog")).show(ctx, f);
                result = Some(modal.inner);
            }
        });
        self.out = Some(out);
        result.expect("one pass")
    }

    /// Run one frame with `events`, `f` drawing in a bottom panel like the
    /// status bar; what `f` returned.
    pub fn panel<R>(&mut self, events: Vec<Event>, f: impl FnOnce(&mut egui::Ui) -> R) -> R {
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
            events,
            focused: true,
            ..Default::default()
        };
        let mut f = Some(f);
        let mut result = None;
        let out = self.ctx.run(input, |ctx| {
            if let Some(f) = f.take() {
                egui::TopBottomPanel::bottom("panel").show(ctx, |ui| {
                    result = Some(ui.horizontal(f).inner);
                });
            }
        });
        self.out = Some(out);
        result.expect("one pass")
    }

    /// Centre of the widget (of any role: link, label…) of the last frame
    /// whose label contains `label`.
    pub fn widget(&self, label: &str) -> Option<Pos2> {
        let update = self
            .out
            .as_ref()?
            .platform_output
            .accesskit_update
            .as_ref()?;
        update.nodes.iter().find_map(|(_, node)| {
            let found = node.label()?.contains(label);
            let b = node.bounds()?;
            found.then(|| Pos2::new(((b.x0 + b.x1) / 2.0) as f32, ((b.y0 + b.y1) / 2.0) as f32))
        })
    }

    /// The button of the last frame whose label contains `label`.
    pub fn button(&self, label: &str) -> Option<Button> {
        let update = self
            .out
            .as_ref()?
            .platform_output
            .accesskit_update
            .as_ref()?;
        update.nodes.iter().find_map(|(_, node)| {
            let found = node.role() == Role::Button && node.label()?.contains(label);
            let b = node.bounds()?;
            found.then(|| Button {
                center: Pos2::new(((b.x0 + b.x1) / 2.0) as f32, ((b.y0 + b.y1) / 2.0) as f32),
                enabled: !node.is_disabled(),
            })
        })
    }

    /// Texts the last frame copied to the clipboard.
    pub fn copied(&self) -> Vec<String> {
        self.out
            .iter()
            .flat_map(|o| &o.platform_output.commands)
            .filter_map(|c| match c {
                OutputCommand::CopyText(t) => Some(t.clone()),
                _ => None,
            })
            .collect()
    }
}

/// A key press without modifiers.
pub fn key(key: Key) -> Event {
    Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    }
}

/// A click (press and release) at `pos`.
pub fn click(pos: Pos2) -> Vec<Event> {
    let button = |pressed| Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    vec![Event::PointerMoved(pos), button(true), button(false)]
}
