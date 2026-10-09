//! Results and "Sortie" log of a console, and how run, submit and refresh
//! outcomes update them.

use chrono::{DateTime, Local};

use crate::engine::models::QueryResult;
use crate::engine::ops::query::{editable_table, StatementOutcome};
use crate::engine::ops::rows::NO_PRIMARY_KEY;
use crate::engine::sql::statements::{extract_table_from_query, single_table_source};
use crate::gui::grid::GridState;
use crate::gui::status::not_connected;

use super::{ConsoleTab, MAX_ROWS};

pub struct ResultTab {
    /// Unique in its console (indices shift when tabs close).
    pub id: u64,
    /// "Résultat 1", or the table name when the query reads one table.
    pub title: String,
    pub sql: String,
    pub result: QueryResult,
    /// Connection the rows were read from: a Submit or a refresh goes there
    /// even after the console was bound to another connection.
    pub connection: String,
    pub pinned: bool,
    pub grid: GridState,
    /// Read from one table with a primary key: edits can be submitted.
    pub editable: bool,
    /// Last failed Submit (the transaction was rolled back).
    pub error: Option<String>,
}

impl ResultTab {
    pub fn new(
        id: u64,
        title: String,
        sql: String,
        result: QueryResult,
        connection: String,
    ) -> Self {
        let editable = is_editable(&sql, &result);
        Self {
            id,
            title,
            sql,
            result,
            connection,
            pinned: false,
            grid: GridState::default(),
            editable,
            error: None,
        }
    }
}

/// Connection result tab `r` must be submitted to / refreshed from, when it
/// is open (`open`); else the error to report.
pub fn result_connection(r: &ResultTab, open: impl Fn(&str) -> bool) -> Result<&str, String> {
    if open(&r.connection) {
        Ok(&r.connection)
    } else {
        Err(not_connected(&r.connection))
    }
}

/// A console result is editable when its rows are rows of one table whose
/// whole primary key is shown (`ops::query::editable_table`).
pub fn is_editable(sql: &str, result: &QueryResult) -> bool {
    editable_table(sql, result).is_some()
}

/// Why a result that reads a single table is read-only: that table has no
/// primary key (its rows can't be told apart).
pub fn read_only_hint(sql: &str, result: &QueryResult) -> Option<&'static str> {
    let unkeyed = single_table_source(sql).is_some() && result.primary_key.is_empty();
    (unkeyed && !is_editable(sql, result)).then_some(NO_PRIMARY_KEY)
}

/// Lines kept in the "Sortie" log (the oldest are dropped).
pub const MAX_LOG_LINES: usize = 2000;

pub struct LogLine {
    pub sql: String,
    pub message: String,
    pub ok: bool,
    /// `HH:MM:SS` of the run.
    pub(super) time: String,
    /// First line of `sql`, shortened (drawn every frame).
    pub(super) preview: String,
}

impl LogLine {
    pub fn new(at: DateTime<Local>, sql: String, message: String, ok: bool) -> Self {
        Self {
            time: at.format("%H:%M:%S").to_string(),
            preview: crate::gui::history_popup::preview(&sql, 120),
            sql,
            message,
            ok,
        }
    }
}

/// Title of the `index`-th (1-based) result tab of `sql`.
pub fn result_title(sql: &str, index: usize, truncated: bool) -> String {
    let table = extract_table_from_query(sql)
        .and_then(|t| {
            t.rsplit('.').next().map(|t| {
                t.trim_matches(|c| matches!(c, '"' | '`' | '[' | ']'))
                    .to_string()
            })
        })
        .filter(|t| !t.is_empty());
    let title = table.unwrap_or_else(|| format!("Résultat {index}"));
    if truncated {
        format!("{title} ({MAX_ROWS}+)")
    } else {
        title
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Log message of a successful outcome.
fn ok_message(result: &QueryResult, elapsed_ms: u128) -> String {
    if result.columns.is_empty() {
        format!(
            "{} en {elapsed_ms} ms",
            plural(
                result.rows_affected as usize,
                "ligne affectée",
                "lignes affectées"
            )
        )
    } else if result.truncated {
        format!("{MAX_ROWS}+ lignes (limité) en {elapsed_ms} ms")
    } else {
        format!(
            "{} en {elapsed_ms} ms",
            plural(result.rows.len(), "ligne", "lignes")
        )
    }
}

/// Apply a run's outcomes (run on `connection`): unpinned result tabs are
/// replaced by one tab per row-returning outcome, each outcome is logged,
/// and the first new result (or "Sortie" on error / no rows) is activated.
/// Returns the status-bar summary.
pub fn apply_outcomes(
    console: &mut ConsoleTab,
    connection: &str,
    outcomes: Vec<StatementOutcome>,
    now: DateTime<Local>,
) -> String {
    console.running_since = None;
    console.results.retain(|r| r.pinned);
    let first_new = console.results.len();
    let mut failed = false;
    let mut total_ms = 0;
    let mut last_rows = None;
    for o in outcomes {
        total_ms += o.elapsed_ms;
        match o.result {
            Ok(result) => {
                console.push_log(LogLine::new(
                    now,
                    o.sql.clone(),
                    ok_message(&result, o.elapsed_ms),
                    true,
                ));
                last_rows = Some(if result.columns.is_empty() {
                    plural(
                        result.rows_affected as usize,
                        "ligne affectée",
                        "lignes affectées",
                    )
                } else if result.truncated {
                    format!("{MAX_ROWS}+ lignes")
                } else {
                    plural(result.rows.len(), "ligne", "lignes")
                });
                if !result.columns.is_empty() {
                    let title = result_title(&o.sql, console.results.len() + 1, result.truncated);
                    let id = console.next_result_id();
                    console.results.push(ResultTab::new(
                        id,
                        title,
                        o.sql,
                        result,
                        connection.into(),
                    ));
                }
            }
            Err(e) => {
                failed = true;
                console.push_log(LogLine::new(now, o.sql, e, false));
            }
        }
    }
    console.active_result = if failed || console.results.len() == first_new {
        console.results.len()
    } else {
        first_new
    };
    console.refresh_known_columns();
    match (failed, last_rows) {
        (true, _) => format!("Erreur · {total_ms} ms"),
        (false, Some(rows)) => format!("{rows} · {total_ms} ms"),
        (false, None) => format!("{total_ms} ms"),
    }
}

/// What to do once a console Submit finished.
#[derive(Debug, PartialEq, Eq)]
pub struct SubmitReport {
    /// Status-bar message: success, or the error.
    pub status: Result<String, String>,
    /// Result tab to re-run (id, SQL, connection), when it still exists.
    pub refresh: Option<(u64, String, String)>,
}

/// Apply the outcome of the Submit of `console.submitting`: on success its
/// edits are cleared and it is to be re-run; on failure the error is shown
/// on it. The outcome is reported even if the result tab was closed or
/// replaced meanwhile.
pub fn apply_submitted(console: &mut ConsoleTab, outcome: Result<usize, String>) -> SubmitReport {
    let id = console.submitting.take();
    let result = id.and_then(|id| console.results.iter_mut().find(|r| r.id == id));
    match outcome {
        Ok(n) => SubmitReport {
            status: Ok(format!("{n} modification(s) appliquée(s)")),
            refresh: result.map(|r| {
                r.grid.discard_edits();
                r.error = None;
                (r.id, r.sql.clone(), r.connection.clone())
            }),
        },
        Err(e) => {
            if let Some(r) = result {
                r.error = Some(e.clone());
            }
            SubmitReport {
                status: Err(format!("Submit annulé (transaction annulée) : {e}")),
                refresh: None,
            }
        }
    }
}

/// Outcome of the re-run of result tab `id` after a Submit: its rows are
/// replaced (edits and focus dropped), the run is logged. Returns the
/// status-bar summary.
pub fn apply_refresh(
    console: &mut ConsoleTab,
    id: u64,
    outcomes: Vec<StatementOutcome>,
    now: DateTime<Local>,
) -> String {
    console.running_since = None;
    let mut summary = String::new();
    for o in outcomes {
        let message = match &o.result {
            Ok(result) => ok_message(result, o.elapsed_ms),
            Err(e) => e.clone(),
        };
        summary = if o.result.is_ok() {
            message.clone()
        } else {
            format!("Erreur · {} ms", o.elapsed_ms)
        };
        console.push_log(LogLine::new(now, o.sql, message, o.result.is_ok()));
        let Some(tab) = console.results.iter_mut().find(|r| r.id == id) else {
            continue;
        };
        match o.result {
            Ok(result) if !result.columns.is_empty() => {
                tab.editable = is_editable(&tab.sql, &result);
                tab.result = result;
                tab.grid.new_result();
                tab.error = None;
            }
            Ok(_) => {}
            Err(e) => tab.error = Some(e),
        }
    }
    console.refresh_known_columns();
    summary
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::engine::models::Column;
    use crate::gui::grid;

    fn result(columns: &[&str], rows: usize) -> QueryResult {
        QueryResult {
            columns: columns
                .iter()
                .map(|c| Column {
                    name: c.to_string(),
                    type_name: "TEXT".into(),
                    nullable: true,
                    is_primary_key: false,
                })
                .collect(),
            rows: vec![vec!["x".to_string(); columns.len()]; rows],
            ..Default::default()
        }
    }

    fn tab(title: &str, pinned: bool) -> ResultTab {
        ResultTab {
            pinned,
            ..ResultTab::new(
                0,
                title.into(),
                format!("SELECT * FROM {title}"),
                result(&["a"], 1),
                "c".into(),
            )
        }
    }

    fn ok(sql: &str, r: QueryResult) -> StatementOutcome {
        StatementOutcome {
            sql: sql.into(),
            result: Ok(r),
            elapsed_ms: 5,
        }
    }

    #[test]
    fn result_title_from_table_or_index() {
        assert_eq!(result_title("SELECT * FROM users", 1, false), "users");
        assert_eq!(
            result_title("select id from \"main\".\"users\" where 1", 2, false),
            "users"
        );
        assert_eq!(result_title("SELECT 1", 3, false), "Résultat 3");
        assert_eq!(result_title("SELECT 1", 1, true), "Résultat 1 (1000+)");
        assert_eq!(
            result_title("SELECT * FROM [dbo].[t]", 1, true),
            "t (1000+)"
        );
    }

    #[test]
    fn apply_outcomes_keeps_pinned_and_logs() {
        let mut c = ConsoleTab::new(String::new(), 0);
        c.results = vec![tab("kept", true), tab("dropped", false)];
        c.running_since = Some(Instant::now());
        let mut affected = QueryResult {
            rows_affected: 2,
            ..Default::default()
        };
        affected.columns.clear();
        let mut big = result(&["id", "name"], 3);
        big.truncated = true;
        let summary = apply_outcomes(
            &mut c,
            "c",
            vec![
                ok("UPDATE t SET a = 1", affected),
                ok("SELECT id, name FROM users", big),
                ok("SELECT 42", result(&["?column?"], 1)),
            ],
            Local::now(),
        );
        let titles: Vec<_> = c.results.iter().map(|r| r.title.as_str()).collect();
        assert_eq!(titles, ["kept", "users (1000+)", "Résultat 3"]);
        assert_eq!(c.active_result, 1, "first new result");
        let messages: Vec<_> = c.log.iter().map(|l| l.message.as_str()).collect();
        assert_eq!(
            messages,
            [
                "2 lignes affectées en 5 ms",
                "1000+ lignes (limité) en 5 ms",
                "1 ligne en 5 ms"
            ]
        );
        assert!(c.log.iter().all(|l| l.ok));
        assert!(c.running_since.is_none());
        assert_eq!(summary, "1 ligne · 15 ms");
        assert_eq!(c.known_columns, ["?column?", "a", "id", "name"]);
    }

    #[test]
    fn apply_outcomes_activates_output_on_error_or_no_rows() {
        let mut c = ConsoleTab::new(String::new(), 0);
        c.results = vec![tab("old", false)];
        let summary = apply_outcomes(
            &mut c,
            "c",
            vec![
                ok("SELECT * FROM t", result(&["a"], 2)),
                StatementOutcome {
                    sql: "SELEC".into(),
                    result: Err("syntax error".into()),
                    elapsed_ms: 1,
                },
            ],
            Local::now(),
        );
        assert_eq!(c.results.len(), 1);
        assert_eq!(c.results[0].title, "t");
        assert_eq!(c.active_result, 1, "Sortie");
        assert_eq!(c.log[1].message, "syntax error");
        assert!(!c.log[1].ok);
        assert_eq!(summary, "Erreur · 6 ms");

        let affected = QueryResult {
            rows_affected: 1,
            ..QueryResult::default()
        };
        apply_outcomes(
            &mut c,
            "c",
            vec![ok("DELETE FROM t", affected)],
            Local::now(),
        );
        assert!(c.results.is_empty(), "unpinned results replaced");
        assert_eq!(c.active_result, 0, "Sortie when nothing returned rows");
        assert_eq!(c.log[2].message, "1 ligne affectée en 5 ms");
    }

    fn with_pk(mut r: QueryResult) -> QueryResult {
        r.columns[0].is_primary_key = true;
        r.primary_key = vec![r.columns[0].name.clone()];
        r
    }

    #[test]
    fn editable_when_one_table_with_a_primary_key() {
        let keyed = with_pk(result(&["id", "name"], 1));
        assert!(is_editable("SELECT * FROM users", &keyed));
        assert!(is_editable("SELECT id, valid_from FROM users", &keyed));
        assert!(!is_editable("SELECT 1", &keyed), "no table");
        assert!(
            !is_editable("SELECT * FROM users u JOIN roles r ON r.id = u.id", &keyed),
            "join"
        );
        assert!(
            !is_editable("SELECT * FROM users UNION SELECT * FROM users", &keyed),
            "union"
        );
        assert!(
            !is_editable("SELECT * FROM users", &result(&["id"], 1)),
            "no key"
        );
        let mut partial = with_pk(result(&["id", "name"], 1));
        partial.primary_key.push("tenant".into());
        assert!(
            !is_editable("SELECT * FROM users", &partial),
            "a key column is missing"
        );
        let mut c = ConsoleTab::new(String::new(), 0);
        apply_outcomes(
            &mut c,
            "c",
            vec![ok("SELECT * FROM users", keyed)],
            Local::now(),
        );
        assert!(c.results[0].editable);
    }

    #[test]
    fn read_only_hint_only_for_a_single_table_without_key() {
        let keyed = with_pk(result(&["id", "name"], 1));
        assert_eq!(read_only_hint("SELECT * FROM users", &keyed), None);
        let unkeyed = result(&["a", "b"], 1);
        assert_eq!(
            read_only_hint("SELECT * FROM logs", &unkeyed),
            Some("Lecture seule : pas de clé primaire")
        );
        assert!(!is_editable("SELECT * FROM logs", &unkeyed));
        // Not a single table: read-only without that hint.
        assert_eq!(read_only_hint("SELECT 1 AS one", &unkeyed), None);
    }

    #[test]
    fn apply_refresh_replaces_only_its_result_and_drops_edits() {
        let mut c = ConsoleTab::new(String::new(), 0);
        apply_outcomes(
            &mut c,
            "c",
            vec![
                ok("SELECT * FROM a", with_pk(result(&["id"], 1))),
                ok("SELECT * FROM b", with_pk(result(&["id"], 2))),
            ],
            Local::now(),
        );
        let (first, second) = (c.results[0].id, c.results[1].id);
        assert_ne!(first, second);
        let b = &mut c.results[1];
        b.grid.edits.set(
            &b.result.rows,
            grid::changes::RowRef::Base(0),
            0,
            "z".into(),
        );
        b.error = Some("old".into());

        let summary = apply_refresh(
            &mut c,
            second,
            vec![ok("SELECT * FROM b", with_pk(result(&["id"], 5)))],
            Local::now(),
        );
        assert_eq!(summary, "5 lignes en 5 ms");
        assert_eq!(c.results.len(), 2, "other result tabs kept");
        assert_eq!(c.results[0].result.rows.len(), 1);
        let b = &c.results[1];
        assert_eq!((b.id, b.title.as_str()), (second, "b"));
        assert_eq!(b.result.rows.len(), 5);
        assert!(b.grid.edits.is_empty() && b.error.is_none());
        assert_eq!(c.log.len(), 3);

        let summary = apply_refresh(
            &mut c,
            first,
            vec![StatementOutcome {
                sql: "SELECT * FROM a".into(),
                result: Err("gone".into()),
                elapsed_ms: 2,
            }],
            Local::now(),
        );
        assert_eq!(summary, "Erreur · 2 ms");
        assert_eq!(c.results[0].error.as_deref(), Some("gone"));
        assert!(!c.log[3].ok);
    }

    #[test]
    fn results_are_submitted_to_the_connection_they_were_read_from() {
        let mut c = ConsoleTab::new(String::new(), 0);
        apply_outcomes(
            &mut c,
            "prod",
            vec![ok("SELECT * FROM a", with_pk(result(&["id"], 1)))],
            Local::now(),
        );
        // The console is rebound to "dev" afterwards: the result stays on prod.
        let r = &c.results[0];
        assert_eq!(r.connection, "prod");
        assert_eq!(
            result_connection(r, |n| n == "prod" || n == "dev"),
            Ok("prod")
        );
        assert_eq!(
            result_connection(r, |n| n == "dev"),
            Err("prod n'est pas connectée".to_string())
        );
    }

    #[test]
    fn submit_outcome_is_reported_even_without_its_result_tab() {
        let mut c = ConsoleTab::new(String::new(), 0);
        apply_outcomes(
            &mut c,
            "prod",
            vec![ok("SELECT * FROM a", with_pk(result(&["id"], 1)))],
            Local::now(),
        );
        let id = c.results[0].id;
        let r = &mut c.results[0];
        r.grid.edits.set(
            &r.result.rows,
            grid::changes::RowRef::Base(0),
            0,
            "z".into(),
        );

        c.submitting = Some(id);
        let failed = apply_submitted(&mut c, Err("NOT NULL".into()));
        assert_eq!(
            failed.status,
            Err("Submit annulé (transaction annulée) : NOT NULL".into())
        );
        assert_eq!(c.results[0].error.as_deref(), Some("NOT NULL"));
        assert!(!c.results[0].grid.edits.is_empty(), "edits kept");

        c.submitting = Some(id);
        let ok = apply_submitted(&mut c, Ok(1));
        assert_eq!(ok.status, Ok("1 modification(s) appliquée(s)".into()));
        assert_eq!(
            ok.refresh,
            Some((id, "SELECT * FROM a".into(), "prod".into()))
        );
        assert!(c.results[0].grid.edits.is_empty() && c.results[0].error.is_none());
        assert_eq!(c.submitting, None);

        // The result tab was closed while its submit ran.
        c.submitting = Some(id);
        c.results.clear();
        let gone = apply_submitted(&mut c, Ok(2));
        assert_eq!(gone.status, Ok("2 modification(s) appliquée(s)".into()));
        assert_eq!(gone.refresh, None);
        let gone = apply_submitted(&mut c, Err("x".into()));
        assert!(gone.status.is_err());
    }

    #[test]
    fn rerun_asks_only_for_edits_in_unpinned_results() {
        let mut c = ConsoleTab::new(String::new(), 0);
        apply_outcomes(
            &mut c,
            "c",
            vec![
                ok("SELECT * FROM a", with_pk(result(&["id"], 1))),
                ok("SELECT * FROM b", with_pk(result(&["id"], 1))),
            ],
            Local::now(),
        );
        assert!(!c.rerun_loses_edits());
        c.results[0].pinned = true;
        let a = &mut c.results[0];
        a.grid.edits.set(
            &a.result.rows,
            grid::changes::RowRef::Base(0),
            0,
            "z".into(),
        );
        assert!(!c.rerun_loses_edits(), "pinned results are kept");
        // An open editor counts, once committed.
        c.results[1].grid.editing = Some((grid::changes::RowRef::Base(0), 0, "y".into()));
        assert!(c.rerun_loses_edits());
        assert!(c.results[1].grid.editing.is_none());
        assert!(!c.results[1].grid.edits.is_empty());
    }

    #[test]
    fn results_are_read_only_while_submitted_or_replaced() {
        let mut c = ConsoleTab::new(String::new(), 0);
        apply_outcomes(
            &mut c,
            "c",
            vec![
                ok("SELECT * FROM a", with_pk(result(&["id"], 1))),
                ok("SELECT * FROM b", with_pk(result(&["id"], 1))),
            ],
            Local::now(),
        );
        c.results[1].pinned = true;
        let (a, b) = (c.results[0].id, c.results[1].id);
        assert_eq!(c.result_lock(0, false), None);
        assert_eq!(c.result_lock(9, true), None, "no such result");

        c.submitting = Some(a);
        assert_eq!(c.result_lock(0, false), Some("Envoi en cours…"));
        assert_eq!(c.result_lock(1, false), None);
        c.submitting = None;

        // A run replaces the unpinned results only.
        assert_eq!(c.result_lock(0, true), Some("Chargement…"));
        assert_eq!(c.result_lock(1, true), None);

        // The re-run after a submit replaces that result only.
        c.refreshing = Some(b);
        assert_eq!(c.result_lock(0, true), None);
        assert_eq!(c.result_lock(1, true), Some("Chargement…"));
    }
}
