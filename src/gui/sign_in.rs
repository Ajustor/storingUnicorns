//! Entra ID browser sign-ins waiting for the user: the engine reports them
//! from its worker threads (`entra_interactive::set_interaction_handler`),
//! they are applied on the UI thread and shown as a banner with the link.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::mpsc::{self, Receiver};

use egui_phosphor::regular as icon;

use crate::engine::db::entra_interactive::{self, Interaction};

/// Status once a connection (or its test) waiting for the browser is
/// cancelled.
pub const CANCELLED: &str = "Connexion annulée";

pub struct SignIns {
    rx: Receiver<Interaction>,
    /// Connection → sign-in URL, while its browser sign-in waits.
    waiting: BTreeMap<String, String>,
    /// Last warning not shown yet (the OS keyring is unavailable).
    warning: Option<String>,
}

impl SignIns {
    /// Receive the engine's interactions; `repaint` wakes the UI thread.
    pub fn install(repaint: impl Fn() + Send + Sync + 'static) -> Self {
        let (tx, rx) = mpsc::channel();
        entra_interactive::set_interaction_handler(move |interaction| {
            let _ = tx.send(interaction);
            repaint();
        });
        Self::new(rx)
    }

    fn new(rx: Receiver<Interaction>) -> Self {
        Self {
            rx,
            waiting: BTreeMap::new(),
            warning: None,
        }
    }

    /// Apply the interactions received since the last call.
    pub fn poll(&mut self) {
        while let Ok(interaction) = self.rx.try_recv() {
            self.apply(interaction);
        }
    }

    fn apply(&mut self, interaction: Interaction) {
        match interaction {
            Interaction::Waiting { connection, url } => {
                self.waiting.insert(connection, url);
            }
            Interaction::Done { connection } => {
                self.waiting.remove(&connection);
            }
            Interaction::Warning {
                connection,
                message,
            } => {
                tracing::warn!("{connection}: {message}");
                self.warning = Some(message);
            }
        }
    }

    /// The sign-in shown in the banner: `(connection, url)`.
    pub fn current(&self) -> Option<(&str, &str)> {
        self.waiting
            .iter()
            .next()
            .map(|(c, u)| (c.as_str(), u.as_str()))
    }

    /// URL of the sign-in `connection` waits for.
    pub fn url_of(&self, connection: &str) -> Option<&str> {
        self.waiting.get(connection).map(String::as_str)
    }

    /// Stop showing the wait of `connection`, cancelled (its `Done` follows).
    pub fn dismiss(&mut self, connection: &str) {
        self.waiting.remove(connection);
    }

    /// The warning not shown yet, once.
    pub fn take_warning(&mut self) -> Option<String> {
        self.warning.take()
    }
}

/// Whether the wait of `connection` belongs to something « Annuler » can
/// stop: the connection being opened, or the form's test.
pub fn cancellable(connection: &str, connecting: &BTreeSet<String>, testing: bool) -> bool {
    connecting.contains(connection) || testing
}

/// « Terminez la connexion à <connection> dans votre navigateur… » with
/// « Copier le lien », and « Annuler » when `cancellable`; whether
/// « Annuler » was clicked.
pub fn waiting_row(ui: &mut egui::Ui, connection: &str, url: &str, cancellable: bool) -> bool {
    let mut cancel = false;
    ui.horizontal_wrapped(|ui| {
        ui.spinner();
        ui.label(format!(
            "Terminez la connexion à {connection} dans votre navigateur…"
        ));
        if ui
            .button(format!("{} Copier le lien", icon::COPY))
            .on_hover_text(
                "Si le navigateur ne s'est pas ouvert, collez ce lien dans sa barre d'adresse",
            )
            .clicked()
        {
            ui.ctx().copy_text(url.to_string());
        }
        cancel = cancellable && ui.button("Annuler").clicked();
    });
    cancel
}

#[cfg(test)]
mod tests {
    use super::*;

    fn waiting(connection: &str, url: &str) -> Interaction {
        Interaction::Waiting {
            connection: connection.into(),
            url: url.into(),
        }
    }

    fn done(connection: &str) -> Interaction {
        Interaction::Done {
            connection: connection.into(),
        }
    }

    #[test]
    fn the_banner_shows_while_the_browser_sign_in_waits() {
        let (tx, rx) = mpsc::channel();
        let mut s = SignIns::new(rx);
        s.poll();
        assert_eq!(s.current(), None);

        tx.send(waiting("Prod", "https://login/prod")).unwrap();
        s.poll();
        assert_eq!(s.current(), Some(("Prod", "https://login/prod")));
        assert_eq!(s.url_of("Prod"), Some("https://login/prod"));
        assert_eq!(s.url_of("Dev"), None);

        tx.send(done("Prod")).unwrap();
        s.poll();
        assert_eq!(s.current(), None);
    }

    #[test]
    fn each_connection_waits_on_its_own() {
        let (tx, rx) = mpsc::channel();
        let mut s = SignIns::new(rx);
        tx.send(waiting("B", "https://b")).unwrap();
        tx.send(waiting("A", "https://a")).unwrap();
        s.poll();
        assert_eq!(s.current(), Some(("A", "https://a")));
        tx.send(done("A")).unwrap();
        s.poll();
        assert_eq!(s.current(), Some(("B", "https://b")));
    }

    #[test]
    fn a_cancelled_wait_hides_before_its_done_arrives() {
        let (tx, rx) = mpsc::channel();
        let mut s = SignIns::new(rx);
        tx.send(waiting("Prod", "https://login")).unwrap();
        s.poll();
        s.dismiss("Prod");
        assert_eq!(s.current(), None);
        // The aborted sign-in still reports its end: nothing to show.
        tx.send(done("Prod")).unwrap();
        s.poll();
        assert_eq!(s.current(), None);
    }

    #[test]
    fn only_a_connection_or_a_test_waiting_can_be_cancelled() {
        let connecting = BTreeSet::from(["Prod".to_string()]);
        assert!(cancellable("Prod", &connecting, false));
        assert!(cancellable("Dev", &connecting, true), "the form's test");
        assert!(!cancellable("Dev", &connecting, false), "nothing to cancel");
    }

    #[test]
    fn a_warning_is_given_once() {
        let (tx, rx) = mpsc::channel();
        let mut s = SignIns::new(rx);
        tx.send(Interaction::Warning {
            connection: "Prod".into(),
            message: "Trousseau indisponible".into(),
        })
        .unwrap();
        s.poll();
        assert_eq!(s.current(), None, "not a wait");
        assert_eq!(s.take_warning().as_deref(), Some("Trousseau indisponible"));
        assert_eq!(s.take_warning(), None);
    }
}
