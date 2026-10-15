//! Opening a connection in the background: the event loop keeps drawing
//! (the link of a browser sign-in, see `apply`) and Esc cancels the attempt.

use std::sync::mpsc;

use tokio::task::JoinHandle;

use crate::engine::db::entra_interactive::{self, Interaction};
use crate::engine::db::DatabaseConnection;
use crate::engine::models::ConnectionConfig;

use super::AppState;

/// Status once a connection being opened is cancelled.
pub const CANCELLED: &str = "Connexion annulée";

/// A connection being opened.
pub struct PendingConnect {
    pub config: ConnectionConfig,
    task: JoinHandle<anyhow::Result<DatabaseConnection>>,
}

/// Receive the engine's sign-in interactions (sent from its worker
/// threads); `poll` applies them.
pub fn listen(state: &mut AppState) {
    let (tx, rx) = mpsc::channel();
    entra_interactive::set_interaction_handler(move |interaction| {
        let _ = tx.send(interaction);
    });
    state.interactions = Some(rx);
}

/// Open the selected connection in the background; `poll` picks up the
/// outcome. Another attempt still running is dropped; the same one goes on.
pub fn start(state: &mut AppState) {
    let Some(config) = state
        .config
        .connections
        .get(state.selected_connection)
        .cloned()
    else {
        state.set_status("No connections configured. Press 'n' to add one.");
        return;
    };
    if let Some(pending) = state.pending_connect.take() {
        if pending.config.name == config.name {
            state.pending_connect = Some(pending);
            return;
        }
        pending.task.abort();
        state.sign_in = None;
    }
    state.set_status(format!("Connecting to {}...", config.name));
    state.is_loading = true;
    state.is_connecting = true;
    state.connection_error = None;
    let task = tokio::spawn({
        let config = config.clone();
        async move { DatabaseConnection::connect(&config).await }
    });
    state.pending_connect = Some(PendingConnect { config, task });
}

/// Esc while a connection is being opened: drop the attempt, which also
/// closes the listener of a browser sign-in it waits for. Returns whether
/// an attempt was cancelled.
pub fn cancel(state: &mut AppState) -> bool {
    let Some(pending) = state.pending_connect.take() else {
        return false;
    };
    pending.task.abort();
    state.is_loading = false;
    state.is_connecting = false;
    state.sign_in = None;
    state.sign_in_warning = None;
    state.set_status(CANCELLED);
    true
}

/// Apply the sign-in interactions received, then take the outcome of the
/// connection being opened once it is over.
pub async fn poll(state: &mut AppState) {
    let received: Vec<Interaction> = state
        .interactions
        .as_ref()
        .map(|rx| rx.try_iter().collect())
        .unwrap_or_default();
    for interaction in received {
        apply(state, interaction);
    }
    if !state
        .pending_connect
        .as_ref()
        .is_some_and(|p| p.task.is_finished())
    {
        return;
    }
    let Some(PendingConnect { config, task }) = state.pending_connect.take() else {
        return;
    };
    state.sign_in = None;
    let outcome = match task.await {
        Ok(outcome) => outcome,
        Err(e) => Err(anyhow::anyhow!(e)),
    };
    match outcome {
        Ok(conn) => {
            if let Some(old) = state.connection.take() {
                old.close().await;
            }
            state.connection = Some(conn);
            state.current_connection_config = Some(config.clone());
            state.set_status(format!("Connected to {}", config.name));
            super::handle_refresh_tables(state).await;
            if let Some(warning) = state.sign_in_warning.take() {
                let status = format!("{} — {warning}", state.status_message);
                state.set_status(status);
            }
        }
        Err(e) => {
            // The failure matters more than a keyring warning (logged).
            state.sign_in_warning = None;
            let message = format!("Connection failed: {e}");
            state.set_status(&message);
            state.connection_error = Some(message);
        }
    }
    state.is_loading = false;
    state.is_connecting = false;
}

/// One sign-in interaction: while a browser sign-in waits, the status line
/// shows its whole link (the browser may not have opened, or be a text one
/// that can't take over the terminal), to select and copy.
fn apply(state: &mut AppState, interaction: Interaction) {
    match interaction {
        Interaction::Waiting { connection, url } => {
            state.set_status(format!(
                "Connexion Azure AD à {connection} : ouvre ce lien dans un navigateur \
                 (Échap pour annuler)"
            ));
            state.sign_in = Some((connection, url));
        }
        Interaction::Done { connection } => {
            if state
                .sign_in
                .as_ref()
                .is_some_and(|(c, _)| *c == connection)
            {
                state.sign_in = None;
                if let Some(pending) = &state.pending_connect {
                    let status = format!("Connecting to {}...", pending.config.name);
                    state.set_status(status);
                }
            }
        }
        Interaction::Warning {
            connection,
            message,
        } => {
            tracing::warn!("{connection}: {message}");
            // Shown with the connection's outcome, which would hide it.
            if state.pending_connect.is_some() {
                state.sign_in_warning = Some(message);
            } else {
                state.set_status(message);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::engine::config::AppConfig;
    use crate::engine::models::DatabaseType;

    fn state_with(connections: Vec<ConnectionConfig>) -> AppState {
        AppState::new(
            AppConfig {
                connections,
                ..Default::default()
            },
            false,
            true,
        )
    }

    /// A PostgreSQL connection to a server that never answers: opening it
    /// hangs (like a browser sign-in) while `_server` lives.
    fn hanging(name: &str) -> (ConnectionConfig, std::net::TcpListener) {
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let config = ConnectionConfig {
            name: name.into(),
            db_type: DatabaseType::Postgres,
            host: Some("127.0.0.1".into()),
            port: Some(server.local_addr().unwrap().port()),
            username: Some("u".into()),
            database: "d".into(),
            ..Default::default()
        };
        (config, server)
    }

    /// An empty SQLite database in `dir`.
    fn sqlite(dir: &tempfile::TempDir) -> ConnectionConfig {
        let path = dir.path().join("t.db");
        std::fs::File::create(&path).unwrap();
        ConnectionConfig {
            name: "file".into(),
            db_type: DatabaseType::SQLite,
            database: path.display().to_string(),
            ..Default::default()
        }
    }

    fn waiting(connection: &str, url: &str) -> Interaction {
        Interaction::Waiting {
            connection: connection.into(),
            url: url.into(),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn esc_cancels_the_connection_being_opened() {
        let (config, _server) = hanging("slow");
        let mut state = state_with(vec![config]);
        start(&mut state);
        assert!(state.is_connecting);
        apply(&mut state, waiting("slow", "https://login/x"));
        assert!(state.sign_in.is_some());

        assert!(cancel(&mut state));
        assert!(!state.is_connecting && !state.is_loading);
        assert_eq!(state.sign_in, None);
        assert_eq!(state.status_message, CANCELLED);
        assert!(!cancel(&mut state), "nothing left to cancel");
        poll(&mut state).await;
        assert!(state.connection.is_none());
        assert_eq!(state.status_message, CANCELLED);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_outcome_is_picked_up_by_the_loop() {
        let dir = tempfile::tempdir().unwrap();
        let config = sqlite(&dir);
        let mut state = state_with(vec![config]);
        start(&mut state);
        assert!(state.is_connecting);
        let started = Instant::now();
        while state.is_connecting {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "never connected"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
            poll(&mut state).await;
        }
        assert!(state.connection.is_some());
        assert_eq!(
            state
                .current_connection_config
                .as_ref()
                .map(|c| c.name.as_str()),
            Some("file")
        );
        assert!(state.pending_connect.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failure_is_reported() {
        let config = ConnectionConfig {
            name: "bad".into(),
            db_type: DatabaseType::SQLite,
            database: "/definitely/not/here/x.db".into(),
            ..Default::default()
        };
        let mut state = state_with(vec![config]);
        start(&mut state);
        let started = Instant::now();
        while state.is_connecting {
            assert!(started.elapsed() < Duration::from_secs(10), "never ended");
            tokio::time::sleep(Duration::from_millis(10)).await;
            poll(&mut state).await;
        }
        assert!(state.connection.is_none());
        assert!(state.status_message.starts_with("Connection failed"));
        assert!(state.connection_error.is_some());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn opening_the_same_connection_again_keeps_the_attempt() {
        let (slow, _server) = hanging("slow");
        let (other, _other_server) = hanging("other");
        let mut state = state_with(vec![slow, other]);
        start(&mut state);
        apply(&mut state, waiting("slow", "https://login/x"));
        start(&mut state);
        assert!(state.sign_in.is_some(), "the same sign-in goes on");
        state.selected_connection = 1;
        start(&mut state);
        assert_eq!(state.sign_in, None, "the first attempt was dropped");
        assert_eq!(
            state
                .pending_connect
                .as_ref()
                .map(|p| p.config.name.as_str()),
            Some("other")
        );
        cancel(&mut state);
    }

    #[test]
    fn the_link_shows_while_the_browser_sign_in_waits() {
        let mut state = state_with(Vec::new());
        apply(&mut state, waiting("Prod", "https://login/x"));
        assert_eq!(
            state.sign_in,
            Some(("Prod".to_string(), "https://login/x".to_string()))
        );
        assert!(state.status_message.contains("Échap pour annuler"));
        apply(
            &mut state,
            Interaction::Done {
                connection: "Other".into(),
            },
        );
        assert!(state.sign_in.is_some(), "another connection's end");
        apply(
            &mut state,
            Interaction::Done {
                connection: "Prod".into(),
            },
        );
        assert_eq!(state.sign_in, None);
    }

    #[test]
    fn a_warning_waits_for_the_connection_outcome() {
        let mut state = state_with(Vec::new());
        let warning = |m: &str| Interaction::Warning {
            connection: "Prod".into(),
            message: m.into(),
        };
        // No connection being opened (a reconnection): shown at once.
        apply(&mut state, warning("Trousseau indisponible"));
        assert_eq!(state.status_message, "Trousseau indisponible");
        assert_eq!(state.sign_in_warning, None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_warning_is_shown_with_the_connection() {
        let dir = tempfile::tempdir().unwrap();
        let config = sqlite(&dir);
        let mut state = state_with(vec![config]);
        start(&mut state);
        apply(
            &mut state,
            Interaction::Warning {
                connection: "file".into(),
                message: "Trousseau indisponible".into(),
            },
        );
        assert_eq!(
            state.sign_in_warning.as_deref(),
            Some("Trousseau indisponible")
        );
        let started = Instant::now();
        while state.is_connecting {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "never connected"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
            poll(&mut state).await;
        }
        assert!(
            state.status_message.ends_with(" — Trousseau indisponible"),
            "{}",
            state.status_message
        );
    }
}
