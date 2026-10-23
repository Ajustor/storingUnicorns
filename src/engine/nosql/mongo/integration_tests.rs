//! Integration tests against real MongoDB servers (plan 13, Task 2).
//!
//! Every test is `#[ignore]` and returns early when its environment variable
//! is missing. Run them with `cargo test -- --ignored integration_mongo` and:
//!
//! - `SU_IT_MONGO=mongodb://127.0.0.1:47017`
//! - `SU_IT_MONGO_AUTH=mongodb://root:p%40ss%3Aw%2Frd@127.0.0.1:47018/?authSource=admin`
//!
//! ```text
//! docker run -d --name su-it-mongo -p 47017:27017 mongo:8
//! docker run -d --name su-it-mongo-auth -p 47018:27017 -e MONGO_INITDB_ROOT_USERNAME=root -e "MONGO_INITDB_ROOT_PASSWORD=p@ss:w/rd" mongo:8
//! ```
//!
//! Each run drops and recreates the database `su_it`.

use bson::{doc, Bson, Document};

use super::*;
use crate::engine::db::DatabaseConnection;
use crate::engine::models::{ConnectionConfig, NULL_CELL};
use crate::engine::nosql::{self, ImportOptions, NoSqlScope, TransferFormat, ABSENT_CELL};
use crate::engine::ops::rows::RowChanges;
use crate::engine::presets::parse_url;

const DB: &str = "su_it";

/// The connection the dialog builds from a pasted URI.
fn config_from_uri(uri: &str) -> ConnectionConfig {
    let p = parse_url(uri).unwrap_or_else(|e| panic!("{uri}: {e}"));
    ConnectionConfig {
        name: "it".into(),
        db_type: p.db_type,
        host: p.host,
        port: p.port,
        username: p.username,
        password: p.password,
        database: p.database.unwrap_or_default(),
        ssl_mode: p.ssl_mode,
        uri: p.uri,
        ..Default::default()
    }
}

fn coll(name: &str) -> CollectionRef {
    CollectionRef {
        db: DB.into(),
        coll: name.into(),
    }
}

/// Run a script in `su_it`; every unit must succeed.
async fn run(conn: &DatabaseConnection, script: &str) -> Vec<nosql::NoSqlOutcome> {
    let mut scope = NoSqlScope {
        database: DB.into(),
    };
    let out = nosql::run_script(conn, script, None, &mut scope, 1000).await;
    for o in &out {
        if let Err(e) = &o.outcome.result {
            panic!("{}: {e}", o.outcome.sql);
        }
    }
    assert_eq!(
        out.len(),
        nosql::units(&conn.db_type(), script).len(),
        "{script}"
    );
    out
}

fn result(o: &nosql::NoSqlOutcome) -> &QueryResult {
    o.outcome.result.as_ref().unwrap()
}

fn query(name: &str) -> DocQuery {
    DocQuery {
        coll: coll(name),
        filter: String::new(),
        sort: String::new(),
        projection: String::new(),
        skip: 0,
        limit: 0,
    }
}

async fn all_docs(c: &MongoClient, name: &str) -> Vec<Document> {
    find_page(
        c,
        &DocQuery {
            sort: "{_id: 1}".into(),
            ..query(name)
        },
    )
    .await
    .unwrap()
    .docs
}

async fn scenario(uri: &str) {
    let config = config_from_uri(uri);
    let conn = DatabaseConnection::connect(&config).await.unwrap();
    let c = conn.mongo().unwrap();
    drop_database(c, DB).await.unwrap();

    // --- console CRUD ---------------------------------------------------
    let out = run(
        &conn,
        r#"use su_it
db.users.insertMany([
  {_id: 1, name: "Ann", age: 31, address: {city: "Paris"}, tags: ["a"]},
  {_id: 2, name: "Bob", age: 25, address: {city: "Lyon"}},
  {_id: 3, name: "Cid", age: 40, born: ISODate("1984-05-06T07:08:09.010Z")},
  {_id: 4, name: "Dee", age: null}
])
db.users.find({age: {$gte: 30}}).sort({age: -1})
db.users.insertOne({_id: 5, name: "Eve", age: NumberLong(22)})
db.users.updateOne({_id: 2}, {$set: {age: 26}, $unset: {address: ""}})
db.users.updateMany({age: {$lt: 30}}, {$inc: {age: 1}})
db.users.replaceOne({_id: 4}, {name: "Dee", age: 50})
db.users.deleteOne({_id: 5})
db.users.countDocuments({age: {$gt: 30}})
db.users.distinct("address.city")
db.users.find({}, {_id: 0, name: 1}).limit(2)
show collections
db.users.find({_id: 1}, {"address.city": 1})
db.users.find({_id: 1}, {})"#,
    )
    .await;
    assert_eq!(result(&out[1]).rows_affected, 4);
    assert_eq!(result(&out[1]).rows[0][0], "4 insérés");
    let found = result(&out[2]);
    let names: Vec<&str> = found.rows.iter().map(|r| r[1].as_str()).collect();
    assert_eq!(names, ["Cid", "Ann"]);
    assert_eq!(found.primary_key, ["_id"]);
    match &out[2].source {
        Some(nosql::Source::Collection { coll: cr, docs }) => {
            assert_eq!(cr, &coll("users"));
            assert_eq!(docs.len(), 2);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(result(&out[3]).rows[0][0], "1 inséré, _id 5");
    assert_eq!(result(&out[4]).rows[0][0], "1 correspondant, 1 modifié");
    // Bob (26) and Eve (22).
    assert_eq!(result(&out[5]).rows[0][0], "2 correspondants, 2 modifiés");
    assert_eq!(result(&out[6]).rows_affected, 1);
    assert_eq!(result(&out[7]).rows[0][0], "1 supprimé");
    assert_eq!(result(&out[8]).rows, [["3"]]);
    assert_eq!(result(&out[9]).rows, [["Paris"]]);
    assert!(out[10].source.is_none(), "_id excluded: read-only");
    assert_eq!(result(&out[10]).rows.len(), 2);
    assert_eq!(result(&out[11]).rows, [["users"]]);
    // A projection keeping `_id` is read-only too (hidden fields), with
    // its reason; `{}` projects nothing.
    assert!(out[12].source.is_none(), "projected: read-only");
    assert_eq!(
        out[12].read_only.as_deref(),
        Some(nosql::PROJECTION_READ_ONLY)
    );
    assert!(out[10].read_only.is_some());
    assert!(out[13].source.is_some() && out[13].read_only.is_none());
    let docs = all_docs(c, "users").await;
    assert_eq!(docs[1], doc! { "_id": 2, "name": "Bob", "age": 27 });
    assert_eq!(docs[3], doc! { "_id": 4, "name": "Dee", "age": 50 });

    // --- syntax error stops the script, scope follows `use` -------------
    let mut scope = NoSqlScope {
        database: String::new(),
    };
    let out = nosql::run_script(
        &conn,
        "db.users.find()\nuse su_it\ndb.users.find({a: })\ndb.users.drop()",
        None,
        &mut scope,
        1000,
    )
    .await;
    assert!(out[0]
        .outcome
        .result
        .as_ref()
        .unwrap_err()
        .contains("Aucune base"));
    assert_eq!(out.len(), 1);
    let out = nosql::run_script(
        &conn,
        "use su_it\ndb.users.find({a: })",
        None,
        &mut scope,
        1000,
    )
    .await;
    assert_eq!(scope.database, DB);
    assert_eq!(out.len(), 2);
    assert!(out[1]
        .outcome
        .result
        .as_ref()
        .unwrap_err()
        .contains("position"));

    // --- catalog -----------------------------------------------------------
    let schemas = conn.get_tables_by_schema().await.unwrap();
    let su = schemas.iter().find(|s| s.name == DB).expect("su_it listed");
    assert_eq!(su.tables, ["users"]);
    let fields = sample_fields(c, &coll("users"), 200).await.unwrap();
    let names: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
    for f in [
        "_id",
        "name",
        "age",
        "address",
        "address.city",
        "tags",
        "born",
    ] {
        assert!(names.contains(&f), "{f} in {names:?}");
    }
    assert_eq!(
        conn.get_primary_keys(&coll("users").key()).await.unwrap(),
        ["_id"]
    );

    // --- pages, sort, projection, count ------------------------------------
    let page = find_page(
        c,
        &DocQuery {
            filter: "{age: {$gte: 27}}".into(),
            sort: "{age: 1}".into(),
            projection: "{name: 1}".into(),
            skip: 1,
            limit: 2,
            ..query("users")
        },
    )
    .await
    .unwrap();
    let names: Vec<&str> = page.result.rows.iter().map(|r| r[1].as_str()).collect();
    assert_eq!(names, ["Ann", "Cid"]);
    assert_eq!(page.result.columns.len(), 2);
    assert_eq!(
        count(
            c,
            &DocQuery {
                filter: "{age: {$gte: 27}}".into(),
                ..query("users")
            }
        )
        .await
        .unwrap(),
        4
    );
    assert_eq!(count(c, &query("users")).await.unwrap(), 4);
    let bad = find_page(
        c,
        &DocQuery {
            filter: "{a:".into(),
            ..query("users")
        },
    )
    .await;
    assert!(bad.err().unwrap().to_string().starts_with("Filtre : "));

    // --- grid edits ----------------------------------------------------------
    let page = find_page(
        c,
        &DocQuery {
            sort: "{_id: 1}".into(),
            ..query("users")
        },
    )
    .await
    .unwrap();
    let cols: Vec<&str> = page
        .result
        .columns
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    let col = |n: &str| cols.iter().position(|c| *c == n).unwrap();
    let ann = page.result.rows[0].clone();
    let mut ann_edited = ann.clone();
    ann_edited[col("age")] = "32".into();
    ann_edited[col("tags")] = ABSENT_CELL.into();
    ann_edited[col("name")] = NULL_CELL.into();
    let mut new_row = vec![None; cols.len()];
    new_row[col("_id")] = Some("10".into());
    new_row[col("name")] = Some("Zed".into());
    let changes = RowChanges {
        updates: vec![(ann.clone(), ann_edited)],
        inserts: vec![new_row],
        deletes: vec![page.result.rows[1].clone()],
    };
    let writes = doc_changes(&page, &changes).unwrap();
    assert_eq!(writes.len(), 5);
    assert_eq!(apply_changes(c, &coll("users"), &writes).await.unwrap(), 5);
    let docs = all_docs(c, "users").await;
    assert_eq!(
        docs[0],
        doc! { "_id": 1, "name": Bson::Null, "age": 32, "address": { "city": "Paris" } }
    );
    assert!(docs.iter().all(|d| d.get("_id") != Some(&Bson::Int32(2))));
    assert_eq!(docs.last().unwrap(), &doc! { "_id": 10, "name": "Zed" });
    let ghost = [
        DocChange::Set {
            id: Bson::Int32(1),
            field: "x".into(),
            value: Bson::Int32(1),
        },
        DocChange::Delete {
            id: Bson::Int32(99),
        },
    ];
    let e = apply_changes(c, &coll("users"), &ghost)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        e.starts_with("1 modification appliquée, échec : document introuvable"),
        "{e}"
    );
    let replaced = DocChange::Replace {
        id: Bson::Int32(10),
        doc: doc! { "_id": 10, "name": "Zoe" },
    };
    apply_changes(c, &coll("users"), &[replaced]).await.unwrap();
    assert_eq!(
        all_docs(c, "users").await.last().unwrap(),
        &doc! { "_id": 10, "name": "Zoe" }
    );

    // --- aggregation ------------------------------------------------------------
    let agg = aggregate(
        c,
        &coll("users"),
        "[{$match: {age: {$gte: 30}}}, {$group: {_id: null, n: {$sum: 1}}}]",
        1000,
    )
    .await
    .unwrap();
    assert_eq!(agg.docs, vec![doc! { "_id": Bson::Null, "n": 3 }]);
    let capped = aggregate(c, &coll("users"), "[{$sort: {_id: 1}}]", 2)
        .await
        .unwrap();
    assert_eq!(capped.docs.len(), 2);
    assert!(capped.result.truncated);
    // A pipeline writing to a collection is refused outside the console.
    let e = aggregate(c, &coll("users"), "[{$out: \"users_copy\"}]", 10)
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("$out") && e.contains("console"), "{e}");
    let names = c.client.database(DB).list_collection_names().await.unwrap();
    assert!(!names.contains(&"users_copy".to_string()), "{names:?}");
    let out = run(
        &conn,
        "db.users.aggregate([{$count: \"n\"}], {allowDiskUse: true})",
    )
    .await;
    assert_eq!(result(&out[0]).rows, [["4"]]);

    // --- indexes -------------------------------------------------------------------
    let unique = create_index(
        c,
        &coll("users"),
        doc! { "name": 1 },
        doc! { "unique": true, "sparse": true },
    )
    .await
    .unwrap();
    assert_eq!(unique, "name_1");
    let ttl = create_index(
        c,
        &coll("users"),
        doc! { "born": 1 },
        // Long enough for the TTL monitor to leave 1984 alone.
        doc! { "name": "born_ttl", "expireAfterSeconds": 2_000_000_000 },
    )
    .await
    .unwrap();
    assert_eq!(ttl, "born_ttl");
    let indexes = list_indexes(c, &coll("users")).await.unwrap();
    let names: Vec<&str> = indexes.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(names, ["_id_", "name_1", "born_ttl"]);
    assert!(indexes[1].unique && indexes[1].sparse);
    assert_eq!(indexes[2].ttl, Some(2_000_000_000));
    let infos = conn.get_indexes(&coll("users").key()).await.unwrap();
    assert!(infos[0].primary);
    assert_eq!(infos[1].columns, ["name: 1"]);
    let dup = run_dup(&conn).await;
    assert!(dup.contains("clé en double"), "{dup}");
    drop_index(c, &coll("users"), "name_1").await.unwrap();
    let out = run(
        &conn,
        "db.users.dropIndex(\"born_ttl\")\ndb.users.getIndexes()",
    )
    .await;
    assert_eq!(result(&out[1]).rows.len(), 1);
    assert!(list_indexes(c, &coll("missing")).await.unwrap().is_empty());

    // --- danger: reported, not run ----------------------------------------------
    let w = nosql::warnings(
        &conn.db_type(),
        "db.users.aggregate([{$merge: {into: \"users\"}}])",
        None,
    );
    assert_eq!(w.len(), 1);
    assert!(w[0].reason.contains("$merge"), "{w:?}");
    let script = "db.users.find()\ndb.users.deleteMany({})";
    let w = nosql::warnings(&conn.db_type(), script, None);
    assert_eq!(w.len(), 1);
    assert_eq!(w[0].unit, "db.users.deleteMany({})");
    assert_eq!(count(c, &query("users")).await.unwrap(), 4);

    // --- export → import ----------------------------------------------------------
    let dir = tempfile::tempdir().unwrap();
    let source = all_docs(c, "users").await;
    for format in TransferFormat::ALL {
        let path = dir
            .path()
            .join(format!("{DB}.users.{}", format.extension()));
        let mut calls = 0;
        let n = export_collection(c, &coll("users"), "", &path, format, |_, _, name| {
            assert_eq!(name, "su_it.users");
            calls += 1;
        })
        .await
        .unwrap();
        assert_eq!(n, 4);
        assert!(calls >= 2);
        let target = format!("copy_{}", format.extension());
        let opts = ImportOptions {
            parse_dates: true,
            ..Default::default()
        };
        // CSV: null and missing are both an empty field.
        let expected: Vec<Document> = match format {
            TransferFormat::Csv => source
                .iter()
                .map(|d| {
                    d.iter()
                        .filter(|(_, v)| **v != Bson::Null)
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect()
                })
                .collect(),
            _ => source.clone(),
        };
        let stats = import_collection(c, &coll(&target), &path, format, &opts, |_, _, _| {})
            .await
            .unwrap();
        assert_eq!(
            (stats.total, stats.inserted, stats.updated),
            (4, 4, 0),
            "{format:?}"
        );
        assert!(stats.errors.is_empty(), "{:?}", stats.errors);
        assert_eq!(all_docs(c, &target).await, expected, "{format:?}");
        // Again: every _id exists.
        let again = import_collection(c, &coll(&target), &path, format, &opts, |_, _, _| {})
            .await
            .unwrap();
        assert_eq!(again.inserted, 0);
        assert_eq!(again.errors.len(), 4, "{format:?}");
        assert!(again.errors[0].contains("_id"), "{:?}", again.errors);
        // Upsert: replaced.
        let upsert = ImportOptions {
            upsert: true,
            ..opts
        };
        let replaced = import_collection(c, &coll(&target), &path, format, &upsert, |_, _, _| {})
            .await
            .unwrap();
        assert_eq!((replaced.inserted, replaced.updated), (0, 4), "{format:?}");
        assert!(replaced.errors.is_empty());
    }
    let filtered = dir.path().join("filtered.jsonl");
    let n = export_collection(
        c,
        &coll("users"),
        "{age: {$gt: 40}}",
        &filtered,
        TransferFormat::JsonLines,
        |_, _, _| {},
    )
    .await
    .unwrap();
    assert_eq!(n, 1);

    // --- a collection with a dot in its name -------------------------------------
    create_collection(c, DB, "fs.chunks").await.unwrap();
    run(&conn, "db.fs.chunks.insertOne({_id: 1, n: 1})\ndb.getCollection(\"fs.chunks\").insertOne({_id: 2})").await;
    let dotted = CollectionRef::parse(r#""su_it"."fs.chunks""#).unwrap();
    assert_eq!(dotted.coll, "fs.chunks");
    let page = find_page(
        c,
        &DocQuery {
            coll: dotted.clone(),
            ..query("x")
        },
    )
    .await
    .unwrap();
    assert_eq!(page.docs.len(), 2);
    assert_eq!(conn.get_primary_keys(&dotted.key()).await.unwrap(), ["_id"]);
    let schemas = conn.get_tables_by_schema().await.unwrap();
    let su = schemas.iter().find(|s| s.name == DB).unwrap();
    assert!(su.tables.contains(&"fs.chunks".to_string()));
    drop_collection(c, &dotted).await.unwrap();
    assert!(!get_tables_by_schema(c)
        .await
        .unwrap()
        .iter()
        .any(|s| s.tables.contains(&"fs.chunks".to_string())));
}

/// Insert a duplicate of a unique field; the error text.
async fn run_dup(conn: &DatabaseConnection) -> String {
    let mut scope = NoSqlScope {
        database: DB.into(),
    };
    let out = nosql::run_script(
        conn,
        "db.users.insertOne({name: \"Cid\"})",
        None,
        &mut scope,
        10,
    )
    .await;
    out[0].outcome.result.clone().unwrap_err()
}

#[tokio::test]
#[ignore]
async fn integration_mongo_plain() {
    let Ok(uri) = std::env::var("SU_IT_MONGO") else {
        return;
    };
    scenario(&uri).await;
}

#[tokio::test]
#[ignore]
async fn integration_mongo_auth() {
    let Ok(uri) = std::env::var("SU_IT_MONGO_AUTH") else {
        return;
    };
    let config = config_from_uri(&uri);
    assert!(config.password.is_some());
    assert!(!config.uri.as_deref().unwrap_or_default().contains("p%40ss"));
    // A wrong password: a French authentication error.
    let wrong = ConnectionConfig {
        password: Some("nope".into()),
        ..config.clone()
    };
    let e = connect(&wrong).await.err().unwrap().to_string();
    assert!(e.contains("authentification refusée"), "{e}");
    // Fields instead of a URI.
    let fields = ConnectionConfig {
        uri: None,
        host: Some("127.0.0.1".into()),
        port: Some(
            uri.rsplit_once(':')
                .and_then(|(_, rest)| rest.split('/').next())
                .and_then(|p| p.parse().ok())
                .unwrap_or(47018),
        ),
        username: Some("root".into()),
        auth_source: Some("admin".into()),
        ..config.clone()
    };
    let c = connect(&fields).await.unwrap();
    test(&c).await.unwrap();
    scenario(&uri).await;
}
