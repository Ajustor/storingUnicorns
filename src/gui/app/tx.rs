//! Manual transaction mode of the connections: switching, Commit, Rollback.

use std::sync::Arc;

use crate::engine::ops::tx::ManualTx;
use crate::gui::dialogs::Dialog;
use crate::gui::sessions::TxMode;
use crate::gui::tx::TxAction;

use super::App;

impl App {
    /// Apply what the transaction controls of `connection` asked for.
    pub fn tx_action(&mut self, connection: &str, action: TxAction) {
        let Some(session) = self.sessions.get_mut(connection) else {
            self.not_connected(connection);
            return;
        };
        match (action, &mut session.tx) {
            (TxAction::SetManual(true), tx @ TxMode::Auto) => {
                *tx = TxMode::Opening;
                let (config, conn) = (session.config.clone(), session.conn.clone());
                self.worker.open_tx(connection.to_string(), config, conn);
            }
            (TxAction::SetManual(false), TxMode::Manual { ending: false, .. }) => {
                if session.pending_statements() > 0 {
                    self.dialog = Some(Dialog::EndTx(connection.to_string()));
                } else {
                    self.leave_manual(connection);
                }
            }
            (TxAction::Commit | TxAction::Rollback, TxMode::Manual { tx, ending, .. })
                if !*ending =>
            {
                *ending = true;
                let commit = action == TxAction::Commit;
                self.worker
                    .end_tx(connection.to_string(), tx.clone(), commit);
            }
            _ => {}
        }
    }

    /// Answer of "Terminer la transaction ?": commit (else roll back), then
    /// go back to auto-commit.
    pub fn end_tx_then_auto(&mut self, connection: &str, commit: bool) {
        if let Some(TxMode::Manual { then_auto, .. }) =
            self.sessions.get_mut(connection).map(|s| &mut s.tx)
        {
            *then_auto = true;
        }
        let action = if commit {
            TxAction::Commit
        } else {
            TxAction::Rollback
        };
        self.tx_action(connection, action);
    }

    /// Back to auto-commit: close the dedicated session (rolling back what
    /// it still holds) and reload the grids, which showed its changes.
    pub fn leave_manual(&mut self, connection: &str) {
        let Some(session) = self.sessions.get_mut(connection) else {
            return;
        };
        if let TxMode::Manual { tx, .. } = std::mem::take(&mut session.tx) {
            self.worker.close_tx(tx);
            self.info(format!("{connection} : mode de transaction automatique"));
            self.reload_data_tabs(connection, None);
        }
    }

    pub(super) fn on_tx_opened(&mut self, name: String, outcome: Result<Arc<ManualTx>, String>) {
        let session = self.sessions.get_mut(&name);
        match (session, outcome) {
            (Some(s), Ok(tx)) if matches!(s.tx, TxMode::Opening) => {
                s.tx = TxMode::Manual {
                    tx,
                    ending: false,
                    then_auto: false,
                };
                self.info(format!(
                    "{name} : mode de transaction manuel, validez avec Commit ou annulez avec Rollback"
                ));
                self.reload_data_tabs(&name, None);
            }
            // Disconnected meanwhile.
            (_, Ok(tx)) => self.worker.close_tx(tx),
            (session, Err(e)) => {
                if let Some(s) = session {
                    s.tx = TxMode::Auto;
                }
                self.error(format!("{name} : mode manuel impossible : {e}"));
            }
        }
    }

    pub(super) fn on_tx_ended(
        &mut self,
        name: String,
        commit: bool,
        outcome: Result<usize, String>,
    ) {
        let mut leave = false;
        let failed = outcome.is_err();
        if let Some(TxMode::Manual {
            ending, then_auto, ..
        }) = self.sessions.get_mut(&name).map(|s| &mut s.tx)
        {
            *ending = false;
            leave = std::mem::take(then_auto) && outcome.is_ok();
        }
        match outcome {
            Ok(n) if commit => {
                self.success(format!("{name} : Commit, {n} instruction(s) validée(s)"))
            }
            Ok(n) => self.success(format!("{name} : Rollback, {n} instruction(s) annulée(s)")),
            Err(e) => {
                let what = if commit { "Commit" } else { "Rollback" };
                self.error(format!("{name} : {what} en échec : {e}"));
            }
        }
        if leave {
            self.leave_manual(&name);
        } else if !commit || failed {
            // Rolled back (a failed PostgreSQL commit too): rows changed.
            self.reload_data_tabs(&name, None);
        }
    }
}
