//! Transaction mode controls of a connection (DataGrip's "Tx: Auto / Manual"
//! with Commit and Rollback), drawn in the console and data tab toolbars.

use egui::RichText;
use egui_phosphor::regular as icon;

use crate::engine::ops::tx::TxStatus;

use super::theme::{ACCENT, ERROR, SUCCESS};

/// What the controls show of a connection's transaction mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TxView {
    /// Disconnected: nothing to choose.
    #[default]
    Off,
    Auto,
    /// The dedicated session is being opened.
    Opening,
    Manual {
        status: TxStatus,
        /// A Commit or Rollback is running.
        ending: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxAction {
    /// Switch to manual (`true`) or auto-commit mode.
    SetManual(bool),
    Commit,
    Rollback,
}

/// Mode selector, then in manual mode the pending statements and the Commit
/// and Rollback buttons.
pub fn controls(ui: &mut egui::Ui, id: impl std::hash::Hash, view: TxView) -> Option<TxAction> {
    if view == TxView::Off {
        return None;
    }
    let mut action = None;
    let manual = !matches!(view, TxView::Auto);
    let selected = if manual { "Tx : Manuel" } else { "Tx : Auto" };
    egui::ComboBox::from_id_salt(("tx_mode", id))
        .selected_text(selected)
        .width(110.0)
        .show_ui(ui, |ui| {
            if ui
                .selectable_label(!manual, "Auto")
                .on_hover_text("Chaque instruction est validée dès son exécution")
                .clicked()
                && manual
            {
                action = Some(TxAction::SetManual(false));
            }
            if ui
                .selectable_label(manual, "Manuel")
                .on_hover_text(
                    "Les instructions et les Submit restent dans une transaction \
                     jusqu'à Commit ou Rollback",
                )
                .clicked()
                && !manual
            {
                action = Some(TxAction::SetManual(true));
            }
        })
        .response
        .on_hover_text("Mode de transaction de la connexion (consoles et grilles)");
    let (status, ending) = match view {
        TxView::Opening => {
            ui.spinner();
            return action;
        }
        TxView::Manual { status, ending } => (status, ending),
        _ => return action,
    };
    let pending = status.open && status.statements > 0;
    if status.failed {
        ui.label(RichText::new(format!("{} En échec", icon::WARNING)).color(ERROR))
            .on_hover_text("Une instruction a échoué : seul Rollback est possible");
    } else if pending {
        ui.label(RichText::new(format!("{} {}", icon::CIRCLE, status.statements)).color(ACCENT))
            .on_hover_text(format!(
                "{} instruction(s) en attente de Commit",
                status.statements
            ));
    }
    let can_end = status.open && !ending;
    if ui
        .add_enabled(
            can_end && !status.failed,
            egui::Button::new(RichText::new(format!("{} Commit", icon::CHECK)).color(SUCCESS)),
        )
        .on_hover_text("Valider la transaction")
        .clicked()
    {
        action = Some(TxAction::Commit);
    }
    if ui
        .add_enabled(
            can_end,
            egui::Button::new(format!("{} Rollback", icon::ARROW_COUNTER_CLOCKWISE)),
        )
        .on_hover_text("Annuler la transaction")
        .clicked()
    {
        action = Some(TxAction::Rollback);
    }
    if ending {
        ui.spinner();
    }
    action
}
