//! TUI MongoDB / Redis logic, without a server: the engine's stubs fail
//! (« non implémenté ») and the TUI must say so, never panic.

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyModifiers};

use super::transfer::{Field, Kind, Transfer};
use super::*;
use crate::engine::config::AppConfig;
use crate::engine::models::{Column, ConnectionConfig, SchemaInfo, NULL_CELL};
use crate::engine::nosql::{TransferFormat, ABSENT_CELL};
use crate::engine::ops::query::StatementOutcome;
use crate::engine::ops::test_support::{mongo_offline, redis_offline};
use crate::tui::key_handlers::{handle_results_keys, handle_tables_keys, KeyAction};
use crate::tui::ActivePanel;

fn config(db_type: DatabaseType) -> ConnectionConfig {
    ConnectionConfig {
        name: "nosql".into(),
        database: String::new(),
        db_type,
        ..Default::default()
    }
}

/// A state connected (offline) to MongoDB, `shop` → `users`, `orders`.
async fn mongo_state() -> AppState {
    let mut state = AppState::new(AppConfig::default(), false, true);
    state.connection = Some(Arc::new(mongo_offline().await));
    state.current_connection_config = Some(config(DatabaseType::MongoDB));
    state.schemas = vec![SchemaInfo {
        name: "shop".into(),
        tables: vec!["users".into(), "orders".into()],
        expanded: true,
    }];
    state.query_tabs.current_tab_mut().nosql_scope = None;
    state
}

/// A state connected (offline) to Redis, databases db0..db2.
fn redis_state() -> AppState {
    let mut state = AppState::new(AppConfig::default(), false, true);
    state.connection = Some(Arc::new(redis_offline()));
    state.current_connection_config = Some(config(DatabaseType::Redis));
    state.schemas = (0..3)
        .map(|i| SchemaInfo {
            name: format!("db{i}"),
            tables: Vec::new(),
            expanded: false,
        })
        .collect();
    state.query_tabs.current_tab_mut().nosql_scope = None;
    state
}

fn key(k: &str, kind: RedisType) -> KeyInfo {
    KeyInfo {
        key: k.as_bytes().to_vec(),
        kind,
        ttl: None,
        size: None,
    }
}

fn leaf(label: &str, index: usize) -> KeyNode {
    KeyNode {
        label: label.into(),
        prefix: label.into(),
        children: Vec::new(),
        key: Some(index),
        count: 1,
    }
}

/// `user:` (user:1, user:2), then `config`.
fn tree() -> (Vec<KeyInfo>, Vec<KeyNode>) {
    let keys = vec![
        key("user:1", RedisType::Hash),
        key("user:2", RedisType::String),
        key("config", RedisType::List),
    ];
    let nodes = vec![
        KeyNode {
            label: "user:".into(),
            prefix: "user:".into(),
            children: vec![leaf("1", 0), leaf("2", 1)],
            key: None,
            count: 2,
        },
        leaf("config", 2),
    ];
    (keys, nodes)
}

/// Put the tree as the keys of db0 (as `set_redis_keys` would once
/// `key_tree` is implemented).
fn load_tree(state: &mut AppState) {
    let (keys, nodes) = tree();
    state.nosql.redis.insert(
        0,
        RedisKeys {
            pattern: "*".into(),
            keys,
            tree: nodes,
            ..Default::default()
        },
    );
    state.schemas[0].expanded = true;
    sync_redis_schema(state, 0);
}

fn cols(names: &[&str]) -> Vec<Column> {
    names
        .iter()
        .map(|n| Column {
            name: n.to_string(),
            type_name: String::new(),
            nullable: true,
            is_primary_key: false,
        })
        .collect()
}

fn result(names: &[&str], rows: &[&[&str]]) -> QueryResult {
    QueryResult {
        columns: cols(names),
        rows: rows
            .iter()
            .map(|r| r.iter().map(|c| c.to_string()).collect())
            .collect(),
        ..Default::default()
    }
}

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn bytes(s: &str) -> Result<Vec<u8>, String> {
    Ok(s.as_bytes().to_vec())
}

// ---------------------------------------------------------------- commands

#[test]
fn enter_on_a_collection_reads_its_first_documents() {
    assert_eq!(
        collection_command("users"),
        r#"db.getCollection("users").find({}).limit(100)"#
    );
    // Dots and quotes stay in the string.
    assert_eq!(
        collection_command(r#"fs."chunks""#),
        r#"db.getCollection("fs.\"chunks\"").find({}).limit(100)"#
    );
}

#[test]
fn enter_on_a_key_reads_it_by_type() {
    let cmd = |kind| key_read_command(b"user:1", &kind);
    assert_eq!(cmd(RedisType::String).unwrap(), "GET user:1");
    assert_eq!(cmd(RedisType::Hash).unwrap(), "HGETALL user:1");
    assert_eq!(cmd(RedisType::List).unwrap(), "LRANGE user:1 0 99");
    assert_eq!(cmd(RedisType::Set).unwrap(), "SSCAN user:1 0 COUNT 100");
    assert_eq!(
        cmd(RedisType::ZSet).unwrap(),
        "ZRANGE user:1 0 99 WITHSCORES"
    );
    assert_eq!(
        cmd(RedisType::Stream).unwrap(),
        "XRANGE user:1 - + COUNT 100"
    );
    assert_eq!(cmd(RedisType::Other("ReJSON-RL".into())), None);
}

#[test]
fn keys_are_quoted_like_redis_cli() {
    assert_eq!(redis_quote(b"user:1"), "user:1");
    assert_eq!(redis_quote(b"caf\xc3\xa9"), "café");
    assert_eq!(redis_quote(b"a b"), "\"a b\"");
    assert_eq!(redis_quote(b""), "\"\"");
    assert_eq!(redis_quote(b"say \"hi\"\n"), r#""say \"hi\"\n""#);
    assert_eq!(redis_quote(b"x\\y"), r#""x\\y""#);
    assert_eq!(redis_quote(b"\xff\x00a"), r#""\xff\x00a""#);
    assert_eq!(
        argv_text(&[b"SET".to_vec(), b"k 1".to_vec(), b"v".to_vec()]),
        r#"SET "k 1" v"#
    );
}

// ---------------------------------------------------------------- key tree

#[test]
fn groups_are_folded_until_opened() {
    let (keys, nodes) = tree();
    let mut open = HashSet::new();
    let rows = visible_rows(&nodes, &open);
    assert_eq!(rows.len(), 2);
    assert_eq!(row_text(&rows[0], &keys), "▶ user: (2)");
    assert_eq!(row_text(&rows[1], &keys), "[L] config");
    open.insert("user:".to_string());
    let rows = visible_rows(&nodes, &open);
    let texts: Vec<String> = rows.iter().map(|r| row_text(r, &keys)).collect();
    assert_eq!(texts, ["▼ user: (2)", "  [H] 1", "  [S] 2", "[L] config"]);
}

#[test]
fn a_key_shows_its_ttl() {
    let mut keys = vec![key("session", RedisType::String)];
    keys[0].ttl = Some(30);
    let row = KeyRow::Key {
        depth: 1,
        label: "session".into(),
        index: 0,
    };
    assert_eq!(row_text(&row, &keys), "  [S] session ⏱30s");
}

#[tokio::test]
async fn the_tables_panel_folds_redis_groups_and_scrolls_through_them() {
    let mut state = redis_state();
    load_tree(&mut state);
    assert_eq!(state.schemas[0].tables, ["▶ user: (2)", "[L] config"]);
    state.active_panel = ActivePanel::Tables;
    state.selected_schema = 0;
    state.selected_table = 1; // the group
    handle_tables_keys(&mut state, KeyCode::Enter, KeyModifiers::NONE).await;
    assert_eq!(
        state.schemas[0].tables,
        ["▼ user: (2)", "  [H] 1", "  [S] 2", "[L] config"]
    );
    // Down walks the keys of the group, then the next ones.
    for want in [2, 3, 4] {
        state.select_next();
        assert_eq!((state.selected_schema, state.selected_table), (0, want));
    }
    assert_eq!(
        selected_key_row(&state),
        Some((
            0,
            KeyRow::Key {
                depth: 0,
                label: "config".into(),
                index: 2
            }
        ))
    );
    // Space on the group folds it again.
    state.selected_table = 1;
    handle_tables_keys(&mut state, KeyCode::Char(' '), KeyModifiers::NONE).await;
    assert_eq!(state.schemas[0].tables, ["▶ user: (2)", "[L] config"]);
}

#[test]
fn many_keys_scroll_the_tables_panel() {
    let mut state = redis_state();
    let keys: Vec<KeyInfo> = (0..30)
        .map(|i| key(&format!("k{i:02}"), RedisType::String))
        .collect();
    let nodes = (0..30).map(|i| leaf(&format!("k{i:02}"), i)).collect();
    state.nosql.redis.insert(
        0,
        RedisKeys {
            keys,
            tree: nodes,
            truncated: true,
            ..Default::default()
        },
    );
    state.schemas[0].expanded = true;
    sync_redis_schema(&mut state, 0);
    // 30 keys and the « more » line.
    assert_eq!(state.schemas[0].tables.len(), 31);
    assert!(state.schemas[0].tables[30].contains("affiner le motif"));
    state.active_panel = ActivePanel::Tables;
    for _ in 0..20 {
        state.select_next();
    }
    assert_eq!(state.selected_table, 20);
    assert!(state.tables_scroll > 0, "the cursor stays in view");
}

#[tokio::test]
async fn enter_on_a_key_runs_its_read_command_in_its_database() {
    let mut state = redis_state();
    load_tree(&mut state);
    state.selected_table = 2; // config (list)
    tables_enter(&mut state).await;
    assert_eq!(state.query_input(), "LRANGE config 0 99");
    assert_eq!(
        state.query_tabs.current_tab().nosql_scope.as_deref(),
        Some("0")
    );
    // The engine stub fails: said, no panic.
    assert!(
        state.status_message.contains("non implémenté"),
        "{}",
        state.status_message
    );
}

#[tokio::test]
async fn enter_on_a_collection_runs_find_in_its_database() {
    let mut state = mongo_state().await;
    state.selected_schema = 0;
    state.selected_table = 2; // orders
    tables_enter(&mut state).await;
    assert_eq!(
        state.query_input(),
        r#"db.getCollection("orders").find({}).limit(100)"#
    );
    assert_eq!(
        state.query_tabs.current_tab().nosql_scope.as_deref(),
        Some("shop")
    );
    assert!(
        state.status_message.contains("Erreur"),
        "{}",
        state.status_message
    );
}

#[tokio::test]
async fn unfolding_a_redis_database_scans_its_keys() {
    let mut state = redis_state();
    state.selected_schema = 1;
    state.selected_table = 0;
    tables_enter(&mut state).await;
    assert!(state.schemas[1].expanded);
    assert!(
        state.status_message.starts_with("Clés de db1 :"),
        "{}",
        state.status_message
    );
}

// ---------------------------------------------------------------- disabled keys

#[tokio::test]
async fn sql_only_keys_say_why_they_do_nothing() {
    let mut state = redis_state();
    state.active_panel = ActivePanel::Tables;
    let action = handle_tables_keys(&mut state, KeyCode::Char('T'), KeyModifiers::SHIFT).await;
    assert_eq!(action, KeyAction::Consumed);
    assert_eq!(state.dialog_mode, DialogMode::None);
    assert_eq!(
        state.status_message,
        "Vidage par lot : indisponible pour Redis"
    );
    handle_tables_keys(&mut state, KeyCode::Char('s'), KeyModifiers::NONE).await;
    assert_eq!(state.dialog_mode, DialogMode::None, "no SQL schema dialog");
    assert_eq!(state.status_message, "Pas de schéma ni d'index pour Redis");

    let mut state = mongo_state().await;
    state.active_panel = ActivePanel::Tables;
    state.selected_table = 0; // a database
    handle_tables_keys(&mut state, KeyCode::Char('s'), KeyModifiers::NONE).await;
    assert_eq!(state.dialog_mode, DialogMode::None);
    assert!(state.status_message.contains("Sélectionner une collection"));
    // On a collection: its indexes (the stub's error shown inside).
    state.selected_table = 1;
    handle_tables_keys(&mut state, KeyCode::Char('s'), KeyModifiers::NONE).await;
    assert_eq!(state.dialog_mode, DialogMode::Indexes);
    let view = state.nosql.indexes.as_ref().unwrap();
    assert_eq!(view.coll.to_string(), "shop.users");
    assert!(view.error.as_deref().unwrap().contains("non implémenté"));
}

#[tokio::test]
async fn t_asks_before_emptying_a_collection_or_a_database() {
    let mut state = mongo_state().await;
    state.active_panel = ActivePanel::Tables;
    state.selected_table = 1;
    handle_tables_keys(&mut state, KeyCode::Char('t'), KeyModifiers::NONE).await;
    assert_eq!(state.dialog_mode, DialogMode::NoSqlConfirm);
    let confirm = state.nosql.confirm.as_ref().unwrap();
    assert_eq!(confirm.title, "Vider la collection shop.users ?");
    assert!(matches!(confirm.action, ConfirmAction::ClearCollection(_)));

    let mut state = redis_state();
    state.active_panel = ActivePanel::Tables;
    state.selected_schema = 2;
    handle_tables_keys(&mut state, KeyCode::Char('t'), KeyModifiers::NONE).await;
    assert_eq!(
        state.nosql.confirm.as_ref().map(|c| &c.action),
        Some(&ConfirmAction::FlushDb(2))
    );
}

#[tokio::test]
async fn redis_n_and_slash_open_the_new_key_and_pattern_forms() {
    let mut state = redis_state();
    state.active_panel = ActivePanel::Tables;
    state.selected_schema = 1;
    handle_tables_keys(&mut state, KeyCode::Char('n'), KeyModifiers::NONE).await;
    assert_eq!(state.dialog_mode, DialogMode::NoSqlForm);
    assert_eq!(
        state.nosql.form.as_ref().unwrap().purpose,
        FormPurpose::NewKey(1)
    );
    state.close_dialog();
    handle_tables_keys(&mut state, KeyCode::Char('/'), KeyModifiers::NONE).await;
    let form = state.nosql.form.as_ref().unwrap();
    assert_eq!(form.purpose, FormPurpose::Pattern { schema: 1 });
    assert_eq!(form.fields[0].value, "*");
    assert!(!state.tables_filter_active, "not the name filter");
}

// ---------------------------------------------------------------- console

#[test]
fn ctrl_enter_runs_the_unit_or_the_line_of_the_cursor() {
    let text = "GET a\n\nSET b 1\n";
    let db = DatabaseType::Redis;
    assert_eq!(unit_range(&db, text, 2), Some(0..5));
    assert_eq!(unit_range(&db, text, 7), Some(7..14));
    assert_eq!(unit_range(&db, text, 6), None, "a blank line");
    assert_eq!(unit_range(&db, text, 100), None);
}

#[tokio::test]
async fn the_scope_of_a_console_follows_the_engine() {
    let mut state = redis_state();
    assert_eq!(current_scope(&state).database, "0");
    state.query_tabs.current_tab_mut().nosql_scope = Some("4".into());
    assert_eq!(current_scope(&state).database, "4");
    // A MongoDB database left by another connection isn't a Redis one.
    state.query_tabs.current_tab_mut().nosql_scope = Some("shop".into());
    assert_eq!(current_scope(&state).database, "0");

    let mut state = mongo_state().await;
    state.query_tabs.current_tab_mut().nosql_scope = Some("shop".into());
    assert_eq!(current_scope(&state).database, "shop");
}

#[tokio::test]
async fn a_run_shows_its_last_result_and_keeps_its_source() {
    let mut state = mongo_state().await;
    let source = Source::Collection {
        coll: CollectionRef {
            db: "shop".into(),
            coll: "users".into(),
        },
        docs: vec![bson::doc! { "_id": 1, "name": "Ann" }],
    };
    let outcome = |r: Result<QueryResult, String>, source| NoSqlOutcome {
        outcome: StatementOutcome {
            sql: "x".into(),
            result: r,
            elapsed_ms: 3,
        },
        source,
    };
    apply_outcomes(
        &mut state,
        vec![
            outcome(Ok(result(&["n"], &[&["1"]])), None),
            outcome(
                Ok(result(&["_id", "name"], &[&["1", "Ann"]])),
                Some(source.clone()),
            ),
        ],
    );
    assert_eq!(state.query_result.as_ref().unwrap().columns.len(), 2);
    assert_eq!(state.nosql.source, Some(source));
    assert_eq!(state.active_panel, ActivePanel::Results);
    assert_eq!(
        state.status_message,
        "2 commande(s) exécutée(s) : 1 ligne(s) en 3 ms"
    );

    apply_outcomes(
        &mut state,
        vec![
            outcome(Ok(result(&["n"], &[&["1"]])), None),
            outcome(Err("syntaxe".into()), None),
        ],
    );
    assert_eq!(state.status_message, "Erreur (commande 2/2) : syntaxe");
    assert_eq!(state.nosql.source, None, "the first result is shown");
}

#[tokio::test]
async fn f5_runs_the_console_through_the_engine() {
    let mut state = mongo_state().await;
    state.set_query("db.users.find({})".into());
    crate::tui::handle_execute_query(&mut state).await;
    assert_eq!(state.dialog_mode, DialogMode::None);
    assert!(
        state.status_message.starts_with("Erreur : MongoDB"),
        "{}",
        state.status_message
    );
    // The SQL runner is never used: no « Query error ».
    assert!(!state.status_message.contains("Query"));
}

// ---------------------------------------------------------------- confirmations

#[test]
fn the_question_defaults_to_no() {
    assert_eq!(confirm_answer(KeyCode::Char('o')), Some(true));
    assert_eq!(confirm_answer(KeyCode::Char('y')), Some(true));
    assert_eq!(confirm_answer(KeyCode::Char('n')), Some(false));
    assert_eq!(confirm_answer(KeyCode::Enter), Some(false));
    assert_eq!(confirm_answer(KeyCode::Esc), Some(false));
    assert_eq!(confirm_answer(KeyCode::Char('x')), None);
}

#[tokio::test]
async fn destructive_commands_wait_for_a_yes() {
    let mut state = mongo_state().await;
    let text = "db.users.deleteMany({})";
    ask(
        &mut state,
        "1 commande(s) destructive(s) : exécuter ?".into(),
        vec!["Supprime tous les documents de shop.users".into()],
        ConfirmAction::Run {
            text: text.into(),
            range: None,
        },
    );
    // Enter (the default) says no: nothing runs.
    assert!(!crate::tui::handle_dialog_input(
        &mut state,
        KeyCode::Enter,
        KeyModifiers::NONE
    ));
    assert_eq!(state.dialog_mode, DialogMode::None);
    assert_eq!(state.status_message, "Annulé");
    assert!(state.query_result.is_none());

    ask(
        &mut state,
        "?".into(),
        Vec::new(),
        ConfirmAction::Run {
            text: text.into(),
            range: None,
        },
    );
    assert!(!crate::tui::handle_dialog_input(
        &mut state,
        KeyCode::Char('z'),
        KeyModifiers::NONE
    ));
    assert_eq!(state.dialog_mode, DialogMode::NoSqlConfirm, "still asked");
    // `o`: confirmed, run.
    assert!(crate::tui::handle_dialog_input(
        &mut state,
        KeyCode::Char('o'),
        KeyModifiers::NONE
    ));
    confirmed(&mut state).await;
    assert_eq!(state.dialog_mode, DialogMode::None);
    assert!(
        state.status_message.starts_with("Erreur"),
        "ran: {}",
        state.status_message
    );
}

#[tokio::test]
async fn declining_an_index_drop_goes_back_to_the_indexes() {
    let mut state = mongo_state().await;
    let coll = CollectionRef {
        db: "shop".into(),
        coll: "users".into(),
    };
    state.nosql.indexes = Some(IndexesView {
        coll: coll.clone(),
        indexes: vec![
            MongoIndex {
                name: "_id_".into(),
                keys: bson::doc! { "_id": 1 },
                unique: true,
                sparse: false,
                ttl: None,
                partial: None,
                hidden: false,
            },
            MongoIndex {
                name: "email_1".into(),
                keys: bson::doc! { "email": 1 },
                unique: true,
                sparse: false,
                ttl: None,
                partial: None,
                hidden: false,
            },
        ],
        selected: 0,
        error: None,
    });
    state.dialog_mode = DialogMode::Indexes;
    indexes_key(&mut state, KeyCode::Char('d'));
    assert_eq!(state.dialog_mode, DialogMode::Indexes, "not _id_");
    assert!(state.status_message.contains("_id_"));
    indexes_key(&mut state, KeyCode::Down);
    indexes_key(&mut state, KeyCode::Char('d'));
    assert_eq!(state.dialog_mode, DialogMode::NoSqlConfirm);
    assert_eq!(
        state.nosql.confirm.as_ref().unwrap().action,
        ConfirmAction::DropIndex {
            coll,
            name: "email_1".into()
        }
    );
    declined(&mut state);
    assert_eq!(state.dialog_mode, DialogMode::Indexes);
    assert!(state.nosql.indexes.is_some());
    // `n`: the new index form, Esc back to the list.
    indexes_key(&mut state, KeyCode::Char('n'));
    assert_eq!(state.dialog_mode, DialogMode::NoSqlForm);
    form_key(&mut state, KeyCode::Esc);
    assert_eq!(state.dialog_mode, DialogMode::Indexes);
}

// ---------------------------------------------------------------- row edits

#[test]
fn missing_and_null_fields_survive_the_row_editor() {
    assert_eq!(edit_text(ABSENT_CELL), "");
    assert_eq!(edit_text(NULL_CELL), "null");
    assert_eq!(edit_text("Ann"), "Ann");
    assert_eq!(cell_from_text("", ABSENT_CELL), ABSENT_CELL);
    assert_eq!(cell_from_text("null", NULL_CELL), NULL_CELL);
    assert_eq!(cell_from_text("Bob", "Ann"), "Bob");
    assert_eq!(cell_from_text("1", ABSENT_CELL), "1");
}

#[test]
fn row_edits_become_row_changes_for_doc_changes() {
    let old = strings(&["1", "Ann"]);
    let new = strings(&["1", "Bob"]);
    let c = row_changes(Some(&old), Some(&new));
    assert_eq!(c.updates, vec![(old.clone(), new.clone())]);
    let c = row_changes(None, Some(&strings(&["", "Bob"])));
    assert_eq!(c.inserts, vec![vec![None, Some("Bob".to_string())]]);
    let c = row_changes(Some(&old), None);
    assert_eq!(c.deletes, vec![old]);
}

#[test]
fn row_edits_of_each_redis_type() {
    let edits = |kind: RedisType, i, old: Option<&[&str]>, new: Option<&[&str]>| {
        let old = old.map(strings);
        let new = new.map(strings);
        key_row_edits(&kind, i, old.as_deref(), new.as_deref(), &bytes)
    };
    let b = |s: &str| s.as_bytes().to_vec();
    assert_eq!(
        edits(RedisType::String, 0, Some(&["a"]), Some(&["b"])).unwrap(),
        [KeyEdit::SetString(b("b"))]
    );
    assert_eq!(
        edits(RedisType::String, 0, Some(&["a"]), None).unwrap(),
        [KeyEdit::Delete]
    );
    assert_eq!(
        edits(RedisType::List, 3, Some(&["4", "a"]), Some(&["4", "b"])).unwrap(),
        [KeyEdit::ListSet {
            index: 3,
            value: b("b")
        }]
    );
    assert_eq!(
        edits(RedisType::List, 0, None, Some(&["", "z"])).unwrap(),
        [KeyEdit::ListPush {
            left: false,
            value: b("z")
        }]
    );
    assert_eq!(
        edits(RedisType::List, 2, Some(&["3", "a"]), None).unwrap(),
        [KeyEdit::ListRemove { index: 2 }]
    );
    assert_eq!(
        edits(RedisType::Set, 0, Some(&["0", "a"]), Some(&["0", "b"])).unwrap(),
        [KeyEdit::SetRemove(b("a")), KeyEdit::SetAdd(b("b"))]
    );
    assert!(edits(RedisType::Set, 0, Some(&["a"]), Some(&["a"]))
        .unwrap()
        .is_empty());
    assert_eq!(
        edits(RedisType::ZSet, 0, Some(&["m", "1"]), Some(&["m", "2.5"])).unwrap(),
        [KeyEdit::ZAdd {
            member: b("m"),
            score: 2.5
        }]
    );
    assert_eq!(
        edits(RedisType::ZSet, 0, Some(&["m", "1"]), Some(&["n", "1"])).unwrap(),
        [
            KeyEdit::ZRemove(b("m")),
            KeyEdit::ZAdd {
                member: b("n"),
                score: 1.0
            }
        ]
    );
    assert!(edits(RedisType::ZSet, 0, None, Some(&["m", "x"]))
        .unwrap_err()
        .contains("Score invalide"));
    assert_eq!(
        edits(RedisType::Hash, 0, Some(&["f", "1"]), Some(&["f", "2"])).unwrap(),
        [KeyEdit::HSet {
            field: b("f"),
            value: b("2")
        }]
    );
    assert_eq!(
        edits(RedisType::Hash, 0, Some(&["f", "1"]), Some(&["g", "1"])).unwrap(),
        [
            KeyEdit::HDel(b("f")),
            KeyEdit::HSet {
                field: b("g"),
                value: b("1")
            }
        ]
    );
    assert_eq!(
        edits(RedisType::Hash, 0, Some(&["f", "1"]), None).unwrap(),
        [KeyEdit::HDel(b("f"))]
    );
    assert_eq!(
        edits(RedisType::Stream, 0, None, Some(&["*", "a 1 b 2"])).unwrap(),
        [KeyEdit::XAdd {
            id: None,
            fields: vec![(b("a"), b("1")), (b("b"), b("2"))]
        }]
    );
    assert_eq!(
        edits(RedisType::Stream, 0, None, Some(&["5-1", r#"{"a": "1"}"#])).unwrap(),
        [KeyEdit::XAdd {
            id: Some("5-1".into()),
            fields: vec![(b("a"), b("1"))]
        }]
    );
    assert_eq!(
        edits(RedisType::Stream, 0, Some(&["5-1", "a 1"]), None).unwrap(),
        [KeyEdit::XDel("5-1".into())]
    );
    assert!(edits(
        RedisType::Stream,
        0,
        Some(&["5-1", "a"]),
        Some(&["5-1", "b"])
    )
    .is_err());
    assert!(edits(RedisType::Other("ReJSON-RL".into()), 0, None, Some(&["x"])).is_err());
    // A cell the parser refuses refuses the edit.
    let refuse = |_: &str| -> Result<Vec<u8>, String> { Err("\\x invalide".into()) };
    assert!(key_row_edits(
        &RedisType::String,
        0,
        None,
        Some(&strings(&["\\x"])),
        &refuse
    )
    .is_err());
}

fn hash_source() -> Source {
    Source::Key {
        db: 2,
        key: b"user:1".to_vec(),
        kind: RedisType::Hash,
    }
}

#[tokio::test]
async fn results_rows_open_the_editor_only_with_a_source() {
    let mut state = redis_state();
    state.query_result = Some(result(&["champ", "valeur"], &[&["name", "Ann"]]));
    state.active_panel = ActivePanel::Results;
    handle_results_keys(&mut state, KeyCode::Enter).await;
    assert_eq!(state.dialog_mode, DialogMode::None);
    assert!(state
        .status_message
        .starts_with("Résultat en lecture seule"));

    state.nosql.source = Some(hash_source());
    handle_results_keys(&mut state, KeyCode::Enter).await;
    assert_eq!(state.dialog_mode, DialogMode::EditRow);
    assert_eq!(state.editing_row, Some(strings(&["name", "Ann"])));
    assert_eq!(state.editing_table_name.as_deref(), Some("db2 user:1"));
    state.close_dialog();

    handle_results_keys(&mut state, KeyCode::Char('a')).await;
    assert_eq!(state.dialog_mode, DialogMode::AddRow);
    assert_eq!(state.editing_row, Some(strings(&["", ""])));
    state.close_dialog();

    handle_results_keys(&mut state, KeyCode::Char('d')).await;
    assert_eq!(state.dialog_mode, DialogMode::NoSqlConfirm);
    assert_eq!(
        state.nosql.confirm.as_ref().unwrap().title,
        "Supprimer cet élément de user:1 ?"
    );
}

#[tokio::test]
async fn a_list_index_column_is_not_typed_and_a_string_has_no_add() {
    let mut state = redis_state();
    state.query_result = Some(result(&["#", "valeur"], &[&["0", "a"]]));
    state.nosql.source = Some(Source::Key {
        db: 0,
        key: b"l".to_vec(),
        kind: RedisType::List,
    });
    open_row_editor(&mut state, true);
    assert_eq!(state.system_columns, [0]);
    assert_eq!(state.editing_column, 1);
    state.close_dialog();

    state.nosql.source = Some(Source::Key {
        db: 0,
        key: b"s".to_vec(),
        kind: RedisType::String,
    });
    open_row_editor(&mut state, true);
    assert_eq!(state.dialog_mode, DialogMode::None);
    assert!(state
        .status_message
        .contains("Une chaîne n'a qu'une valeur"));
}

#[tokio::test]
async fn debug_mode_never_writes() {
    for mut state in [redis_state(), mongo_state().await] {
        state.debug_mode = true;
        state.query_result = Some(result(&["_id", "name"], &[&["1", "Ann"]]));
        state.nosql.source = Some(if is_redis(&state) {
            hash_source()
        } else {
            Source::Collection {
                coll: CollectionRef {
                    db: "shop".into(),
                    coll: "users".into(),
                },
                docs: vec![bson::doc! { "_id": 1, "name": "Ann" }],
            }
        });
        state.set_query("before".into());
        open_row_editor(&mut state, false);
        state.editing_row.as_mut().unwrap()[1] = "Bob".into();
        crate::tui::handle_save_row(&mut state).await;
        assert_eq!(state.dialog_mode, DialogMode::None);
        let status = state.status_message.clone();
        // The commands in the editor, or the engine's refusal (its stub):
        // never an attempt to write.
        assert!(
            status.starts_with("Debug :") || status.starts_with("Modification refusée"),
            "{status}"
        );
        assert!(!status.contains("Échec"), "{status}");
        if status.starts_with("Debug :") {
            assert_ne!(state.query_input(), "before");
        }
        assert_eq!(state.query_result.as_ref().unwrap().rows[0][1], "Ann");
    }
}

#[test]
fn edit_previews_fall_back_while_the_engine_cannot_print() {
    let edit = KeyEdit::HDel(b"f".to_vec());
    let text = edit_preview(b"k", &edit);
    assert!(text == "HDEL k f" || text.contains("HDel"), "{text}");
    let coll = CollectionRef {
        db: "shop".into(),
        coll: "users".into(),
    };
    let change = DocChange::Delete { id: Bson::Int32(1) };
    let text = change_preview(&coll, &change);
    assert!(text.contains("users"), "{text}");
}

// ---------------------------------------------------------------- JSON view

#[test]
fn v_shows_the_document_or_the_row_as_json() {
    let r = result(
        &["_id", "name", "tags", "age"],
        &[&["1", "Ann", r#"["a","b"]"#, ABSENT_CELL]],
    );
    let text = row_text_view(&r, None, 0).unwrap();
    let json: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        json,
        serde_json::json!({"_id": "1", "name": "Ann", "tags": ["a", "b"]})
    );
    assert!(row_text_view(&r, None, 5).is_none());

    let source = Source::Collection {
        coll: CollectionRef {
            db: "shop".into(),
            coll: "users".into(),
        },
        docs: vec![bson::doc! { "_id": 1, "name": "Ann", "address": { "city": "Lyon" } }],
    };
    let text = row_text_view(&r, Some(&source), 0).unwrap();
    assert!(text.contains("Lyon"), "the whole document: {text}");
    assert!(text.lines().count() > 3, "indented: {text}");
}

#[tokio::test]
async fn the_json_view_scrolls_and_closes() {
    let mut state = mongo_state().await;
    state.query_result = Some(result(&["a"], &[&["{\"x\": [1, 2, 3]}"]]));
    state.active_panel = ActivePanel::Results;
    handle_results_keys(&mut state, KeyCode::Char('v')).await;
    assert_eq!(state.dialog_mode, DialogMode::JsonView);
    let lines = state.nosql.json.as_ref().unwrap().lines.len();
    assert!(lines > 3);
    json_view_key(&mut state, KeyCode::End);
    assert_eq!(state.nosql.json.as_ref().unwrap().scroll, lines - 1);
    json_view_key(&mut state, KeyCode::Down);
    assert_eq!(state.nosql.json.as_ref().unwrap().scroll, lines - 1);
    json_view_key(&mut state, KeyCode::Home);
    assert_eq!(state.nosql.json.as_ref().unwrap().scroll, 0);
    json_view_key(&mut state, KeyCode::Esc);
    assert_eq!(state.dialog_mode, DialogMode::None);
    assert!(state.nosql.json.is_none());
}

// ---------------------------------------------------------------- forms

#[test]
fn index_options_come_from_the_form() {
    let parse = |t: &str| -> Result<Document, String> {
        match t {
            "{email: 1}" => Ok(bson::doc! { "email": 1 }),
            "{a: {$exists: true}}" => Ok(bson::doc! { "a": { "$exists": true } }),
            other => Err(format!("illisible : {other}")),
        }
    };
    let (keys, options) = index_spec(
        &strings(&[
            "{email: 1}",
            "by_email",
            "oui",
            "non",
            "3600",
            "{a: {$exists: true}}",
        ]),
        &parse,
    )
    .unwrap();
    assert_eq!(keys, bson::doc! { "email": 1 });
    assert_eq!(
        options,
        bson::doc! {
            "name": "by_email",
            "unique": true,
            "expireAfterSeconds": 3600_i64,
            "partialFilterExpression": { "a": { "$exists": true } },
        }
    );
    let (_, options) =
        index_spec(&strings(&["{email: 1}", "", "non", "oui", "", ""]), &parse).unwrap();
    assert_eq!(options, bson::doc! { "sparse": true });
    assert!(
        index_spec(&strings(&["", "", "non", "non", "", ""]), &parse)
            .unwrap_err()
            .contains("Clés")
    );
    assert!(index_spec(
        &strings(&["{email: 1}", "", "non", "non", "-1", ""]),
        &parse
    )
    .unwrap_err()
    .contains("TTL invalide"));
    assert!(index_spec(&strings(&["{x}", "", "non", "non", "", ""]), &parse).is_err());
}

#[test]
fn new_key_values_by_type() {
    assert_eq!(
        new_key_value("string", "a b").unwrap(),
        RedisValue::String(b"a b".to_vec())
    );
    assert_eq!(
        new_key_value("list", r#"["a", 2]"#).unwrap(),
        RedisValue::List(vec![b"a".to_vec(), b"2".to_vec()])
    );
    assert_eq!(
        new_key_value("zset", r#"{"m": 1.5}"#).unwrap(),
        RedisValue::ZSet(vec![(b"m".to_vec(), 1.5)])
    );
    assert_eq!(
        new_key_value("hash", r#"{"f": "v"}"#).unwrap(),
        RedisValue::Hash(vec![(b"f".to_vec(), b"v".to_vec())])
    );
    assert!(matches!(
        new_key_value("stream", r#"{"f": "v"}"#).unwrap(),
        RedisValue::Stream(e) if e.len() == 1
    ));
    assert!(new_key_value("set", "[]").unwrap_err().contains("vide"));
    assert!(new_key_value("hash", "[1]").unwrap_err().contains("objet"));
    assert!(new_key_value("list", "{")
        .unwrap_err()
        .contains("JSON invalide"));
    assert!(new_key_value("zset", r#"{"m": "x"}"#).is_err());
    assert_eq!(parse_ttl(" "), Ok(None));
    assert_eq!(parse_ttl("60"), Ok(Some(60)));
    assert!(parse_ttl("0").is_err());
    assert!(parse_ttl("abc").is_err());
}

#[test]
fn forms_type_text_and_cycle_choices() {
    let mut form = Form::new_key(0);
    for c in "k:1".chars() {
        assert_eq!(form.key(KeyCode::Char(c)), None);
    }
    form.key(KeyCode::Backspace);
    form.key(KeyCode::Char('2'));
    assert_eq!(form.fields[0].value, "k:2");
    form.key(KeyCode::Tab);
    assert_eq!(form.active, 1);
    form.key(KeyCode::Right);
    assert_eq!(form.fields[1].value, "list");
    assert_eq!(form.value_hint(), r#"["a", "b"]"#);
    form.key(KeyCode::Left);
    form.key(KeyCode::Left);
    assert_eq!(form.fields[1].value, "stream", "wraps");
    form.key(KeyCode::Char('x'));
    assert_eq!(form.fields[1].value, "stream", "a choice isn't typed");
    form.key(KeyCode::BackTab);
    assert_eq!(form.active, 0);
    assert_eq!(form.key(KeyCode::Enter), Some(true));
    assert_eq!(form.key(KeyCode::Esc), Some(false));
}

#[tokio::test]
async fn submitting_a_new_key_reports_the_engine_error() {
    let mut state = redis_state();
    let mut form = Form::new_key(0);
    form.fields[0].value = "user:9".into();
    form.fields[2].value = "Ann".into();
    open_form(&mut state, form);
    submit_form(&mut state).await;
    // The stub refuses: the form stays open with the error.
    assert_eq!(state.dialog_mode, DialogMode::NoSqlForm);
    assert!(
        state.status_message.starts_with("Création de user:9 :"),
        "{}",
        state.status_message
    );
    // A bad value never reaches the engine.
    state.nosql.form.as_mut().unwrap().fields[1].value = "hash".into();
    submit_form(&mut state).await;
    assert!(state.status_message.contains("JSON invalide"));
}

// ---------------------------------------------------------------- help

#[tokio::test]
async fn the_help_bar_follows_the_engine() {
    use crate::tui::ui::help_bar::get_help_entries;
    let mut state = redis_state();
    state.active_panel = ActivePanel::Tables;
    let keys: Vec<&str> = get_help_entries(&state).iter().map(|e| e.0).collect();
    assert!(keys.contains(&"n") && keys.contains(&"/"), "{keys:?}");
    assert!(!keys.contains(&"s"), "{keys:?}");
    state.active_panel = ActivePanel::Results;
    let entries = get_help_entries(&state);
    assert!(entries.contains(&("v", "JSON")), "{entries:?}");
    assert!(!entries.iter().any(|e| e.1 == "Add row"), "{entries:?}");

    let mut state = mongo_state().await;
    state.active_panel = ActivePanel::Tables;
    let entries = get_help_entries(&state);
    assert!(entries.contains(&("s", "Index")), "{entries:?}");
    assert!(!entries.iter().any(|e| e.1 == "Schema"), "{entries:?}");
    state.active_panel = ActivePanel::QueryEditor;
    let entries = get_help_entries(&state);
    assert!(
        entries.contains(&("F5/Ctrl+G", "Tout exécuter")),
        "{entries:?}"
    );
    state.dialog_mode = DialogMode::NoSqlConfirm;
    let entries = get_help_entries(&state);
    assert_eq!(entries[0], ("o", "Oui"));
    assert!(help_text(&DatabaseType::MongoDB).contains("s=index"));
    assert!(help_text(&DatabaseType::Redis).contains("FLUSHDB"));
}

// ---------------------------------------------------------------- transfer

fn mongo_schemas() -> Vec<SchemaInfo> {
    vec![
        SchemaInfo {
            name: "shop".into(),
            tables: vec!["users".into(), "fs.chunks".into()],
            expanded: true,
        },
        SchemaInfo {
            name: "local".into(),
            tables: vec!["startup_log".into()],
            expanded: false,
        },
    ]
}

#[test]
fn transfer_formats_and_fields_by_engine() {
    let t = Transfer::results(DatabaseType::MongoDB, None);
    assert_eq!(t.formats(), [TransferFormat::Json, TransferFormat::Csv]);
    assert_eq!(t.fields(), [Field::Format, Field::Path]);
    assert_eq!(t.path, "export.json");

    let hidden = |s: &str| s == "local";
    let t = Transfer::batch(
        Kind::Export,
        DatabaseType::MongoDB,
        &mongo_schemas(),
        &hidden,
        Some(("shop", Some("fs.chunks"))),
    );
    assert_eq!(t.formats(), TransferFormat::ALL);
    assert_eq!(
        t.fields(),
        [Field::Format, Field::Path, Field::Filter, Field::List]
    );
    // Hidden databases left out; the collection of the cursor chosen.
    assert_eq!(
        t.items,
        [
            ("shop".to_string(), "users".to_string(), false),
            ("shop".to_string(), "fs.chunks".to_string(), true)
        ]
    );
    assert_eq!(t.selected, 1);
    let chosen = t.chosen();
    assert_eq!(chosen[0].0.to_string(), "shop.fs.chunks");
    assert!(chosen[0].1.ends_with("shop.fs.chunks.json"));

    let mut t = Transfer::batch(
        Kind::Import,
        DatabaseType::MongoDB,
        &mongo_schemas(),
        &hidden,
        None,
    );
    assert_eq!(
        t.fields(),
        [Field::Format, Field::Path, Field::Upsert, Field::List]
    );
    t.cycle_format(false); // JSON -> CSV (backwards)
    assert_eq!(t.format, TransferFormat::Csv);
    assert_eq!(
        t.fields(),
        [
            Field::Format,
            Field::Path,
            Field::Upsert,
            Field::KeepStrings,
            Field::ParseDates,
            Field::List
        ]
    );
    t.options.keep_strings = true;
    assert!(
        !t.fields().contains(&Field::ParseDates),
        "dates are text then"
    );

    let redis: Vec<SchemaInfo> = (0..2)
        .map(|i| SchemaInfo {
            name: format!("db{i}"),
            tables: Vec::new(),
            expanded: false,
        })
        .collect();
    let t = Transfer::batch(Kind::Export, DatabaseType::Redis, &redis, &|_| false, None);
    assert_eq!(t.formats(), [TransferFormat::Json, TransferFormat::Csv]);
    assert_eq!(
        t.fields(),
        [Field::Format, Field::Database, Field::Pattern, Field::Path]
    );
    let t = Transfer::batch(Kind::Import, DatabaseType::Redis, &redis, &|_| false, None);
    assert_eq!(t.fields(), [Field::Database, Field::Path, Field::Upsert]);
}

#[test]
fn transfer_keys_edit_the_modal() {
    let redis: Vec<SchemaInfo> = (0..3)
        .map(|i| SchemaInfo {
            name: format!("db{i}"),
            tables: Vec::new(),
            expanded: false,
        })
        .collect();
    let mut t = Transfer::batch(Kind::Export, DatabaseType::Redis, &redis, &|_| false, None);
    assert_eq!(t.path, "db0.json");
    t.active = Field::Format;
    t.key(KeyCode::Right);
    assert_eq!(
        (t.format, t.path.as_str()),
        (TransferFormat::Csv, "db0.csv")
    );
    t.key(KeyCode::Tab);
    assert_eq!(t.active, Field::Database);
    t.key(KeyCode::Right);
    assert_eq!((t.redis_db(), t.path.as_str()), (Some(1), "db1.csv"));
    t.key(KeyCode::Tab);
    assert_eq!(t.active, Field::Pattern);
    t.key(KeyCode::Backspace);
    for c in "user:*".chars() {
        t.key(KeyCode::Char(c));
    }
    assert_eq!(t.text, "user:*");
    t.key(KeyCode::Tab);
    t.key(KeyCode::Tab);
    assert_eq!(t.active, Field::Format, "wraps");
    assert_eq!(t.key(KeyCode::Enter), Some(true));
    assert!(t.check().is_ok());
    assert_eq!(t.key(KeyCode::Esc), Some(false));

    let mut t = Transfer::batch(
        Kind::Export,
        DatabaseType::MongoDB,
        &mongo_schemas(),
        &|_| false,
        None,
    );
    assert_eq!(t.active, Field::List);
    assert!(t.check().unwrap_err().contains("au moins une collection"));
    t.key(KeyCode::Char('a'));
    assert!(t.items.iter().all(|i| i.2));
    t.key(KeyCode::Char('n'));
    assert!(t.items.iter().all(|i| !i.2));
    t.key(KeyCode::Down);
    t.key(KeyCode::Char(' '));
    assert_eq!(t.chosen().len(), 1);
    assert_eq!(t.chosen()[0].0.coll, "fs.chunks");
    assert!(t.check().is_ok());

    let mut t = Transfer::batch(Kind::Import, DatabaseType::Redis, &redis, &|_| false, None);
    for c in "dump.txt".chars() {
        t.key(KeyCode::Char(c));
    }
    assert!(t.check().unwrap_err().contains("Extension"));
}

#[test]
fn an_import_folder_selects_its_files_and_adds_new_collections() {
    let mut t = Transfer::batch(
        Kind::Import,
        DatabaseType::MongoDB,
        &mongo_schemas(),
        &|_| false,
        None,
    );
    t.match_files(&strings(&[
        "shop.users.json",
        "shop.carts.json",
        "shop.users.csv",
        "other.things.json",
        "readme.md",
    ]));
    let chosen: Vec<String> = t.chosen().iter().map(|c| c.0.to_string()).collect();
    assert_eq!(chosen, ["shop.users", "shop.carts"]);
    t.cycle_format(false); // CSV
    t.match_files(&strings(&["shop.users.csv"]));
    let chosen: Vec<String> = t.chosen().iter().map(|c| c.0.to_string()).collect();
    assert_eq!(chosen, ["shop.users"]);
}

#[test]
fn results_export_as_json_lines_or_csv() {
    let r = result(&["_id", "name"], &[&["1", "Ann"], &["2", ABSENT_CELL]]);
    let json: serde_json::Value =
        serde_json::from_str(&transfer::results_file(&r, None, TransferFormat::Json)).unwrap();
    assert_eq!(
        json,
        serde_json::json!([{"_id": "1", "name": "Ann"}, {"_id": "2"}])
    );
    let lines = transfer::results_file(&r, None, TransferFormat::JsonLines);
    assert_eq!(lines.lines().count(), 2);
    assert_eq!(
        transfer::results_file(&r, None, TransferFormat::Csv),
        "_id,name\n1,Ann\n2,\n"
    );
    // Documents keep their types.
    let docs = vec![
        bson::doc! { "_id": 1, "name": "Ann" },
        bson::doc! { "_id": 2 },
    ];
    let json: serde_json::Value = serde_json::from_str(&transfer::results_file(
        &r,
        Some(&docs),
        TransferFormat::Json,
    ))
    .unwrap();
    assert_eq!(
        json,
        serde_json::json!([{"_id": 1, "name": "Ann"}, {"_id": 2}])
    );
}

#[tokio::test]
async fn export_and_import_keys_open_the_nosql_modal() {
    use crate::tui::key_handlers::{handle_global_keys, resolve_fallback};
    let mut state = mongo_state().await;
    state.active_panel = ActivePanel::Tables;
    // `E` stands for Shift+F6 in the Tables panel.
    let (code, mods) =
        resolve_fallback(KeyCode::Char('E'), KeyModifiers::SHIFT, ActivePanel::Tables);
    handle_global_keys(&mut state, code, mods).await;
    assert_eq!(state.dialog_mode, DialogMode::NoSqlTransfer);
    assert_eq!(state.nosql.transfer.as_ref().unwrap().kind, Kind::Export);
    assert!(state.batch_export_state.is_none(), "not the SQL dialog");
    state.close_dialog();
    handle_global_keys(&mut state, KeyCode::F(7), KeyModifiers::NONE).await;
    assert_eq!(state.nosql.transfer.as_ref().unwrap().kind, Kind::Import);
    assert!(state.import_state.is_none());
    state.close_dialog();
    // F6 without a result says so.
    handle_global_keys(&mut state, KeyCode::F(6), KeyModifiers::NONE).await;
    assert_eq!(state.dialog_mode, DialogMode::None);
    assert!(state.status_message.contains("Aucun résultat"));
    state.query_result = Some(result(&["a"], &[&["1"]]));
    handle_global_keys(&mut state, KeyCode::F(6), KeyModifiers::NONE).await;
    assert_eq!(state.nosql.transfer.as_ref().unwrap().kind, Kind::Results);
    assert!(state.export_state.is_none());
}

#[tokio::test]
async fn running_a_transfer_reports_the_engine_error() {
    use ratatui::{backend::TestBackend, Terminal};
    let mut state = mongo_state().await;
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    let dir = std::env::temp_dir().join(format!("su-tui-nosql-{}", std::process::id()));
    let mut t = Transfer::batch(
        Kind::Export,
        DatabaseType::MongoDB,
        &mongo_schemas(),
        &|_| false,
        Some(("shop", Some("users"))),
    );
    t.path = dir.display().to_string();
    state.nosql.transfer = Some(t);
    state.dialog_mode = DialogMode::NoSqlTransfer;
    transfer::run(&mut terminal, &mut state).await;
    assert_eq!(state.dialog_mode, DialogMode::None);
    assert!(
        state.status_message.contains("1 erreur(s) : shop.users :"),
        "{}",
        state.status_message
    );

    // The shown result is written by the TUI itself.
    let file = dir.join("out.csv");
    state.query_result = Some(result(&["a"], &[&["1"]]));
    let mut t = Transfer::results(DatabaseType::MongoDB, None);
    t.format = TransferFormat::Csv;
    t.path = file.display().to_string();
    state.nosql.transfer = Some(t);
    transfer::run(&mut terminal, &mut state).await;
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "a\n1\n");
    let _ = std::fs::remove_dir_all(&dir);
}
