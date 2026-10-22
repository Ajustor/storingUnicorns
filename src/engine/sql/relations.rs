//! Row-matching conditions for following foreign keys, both ways: from a
//! row to the row it references, and to the rows that reference it.

use crate::engine::db::utils::where_part;
use crate::engine::models::{is_null, Column, DatabaseType, ForeignKeyInfo, IncomingForeignKey};
use crate::engine::sql::statements::quote_chars;

/// The `WHERE` condition (without the keyword) selecting the rows whose
/// `target_columns` equal `values` (one per column; `NULL_CELL` gives
/// `IS NULL`), the columns quoted for `db`. A literal is typed from
/// `target_types` when known (a number is left unquoted in a numeric
/// column); a missing or `None` type gives a quoted string, which every
/// engine converts to the column's type. Extra columns or values are ignored.
pub fn match_condition(
    target_columns: &[String],
    values: &[String],
    target_types: &[Option<String>],
    db: &DatabaseType,
) -> String {
    let (q0, q1) = quote_chars(db);
    target_columns
        .iter()
        .zip(values)
        .enumerate()
        .map(|(i, (name, value))| {
            let col = Column {
                name: name.clone(),
                type_name: target_types.get(i).cloned().flatten().unwrap_or_default(),
                nullable: true,
                is_primary_key: false,
            };
            where_part(&col, value, q0, q1)
        })
        .collect::<Vec<_>>()
        .join(" AND ")
}

/// Values of `columns` in the current row, `None` when one is missing or
/// NULL: a NULL key part links to nothing.
fn key_values(columns: &[String], row: &dyn Fn(&str) -> Option<String>) -> Option<Vec<String>> {
    columns
        .iter()
        .map(|c| row(c).filter(|v| !is_null(v)))
        .collect()
}

/// For an outgoing FK and the current row (column name → value): the
/// referenced table (`fk.ref_table`, as given) and the condition selecting
/// the referenced row. `None` when a referencing value is NULL (no linked
/// row) or missing from the row.
pub fn follow(
    fk: &ForeignKeyInfo,
    row: &dyn Fn(&str) -> Option<String>,
    db: &DatabaseType,
) -> Option<(String, String)> {
    let values = key_values(&fk.columns, row)?;
    Some((
        fk.ref_table.clone(),
        match_condition(&fk.ref_columns, &values, &[], db),
    ))
}

/// For an incoming FK and the current row (column name → value): the
/// referencing table and the condition selecting the rows that point at it.
/// `None` when a referenced value is NULL or missing (nothing can point at it).
pub fn referencing(
    fk: &IncomingForeignKey,
    row: &dyn Fn(&str) -> Option<String>,
    db: &DatabaseType,
) -> Option<(String, String)> {
    let values = key_values(&fk.ref_columns, row)?;
    Some((
        fk.table.clone(),
        match_condition(&fk.columns, &values, &[], db),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::models::NULL_CELL;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn row<'a>(cells: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name: &str| {
            cells
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn simple_condition_quotes_per_engine() {
        let cols = s(&["id"]);
        let vals = s(&["42"]);
        assert_eq!(
            match_condition(&cols, &vals, &[], &DatabaseType::Postgres),
            "\"id\" = '42'"
        );
        assert_eq!(
            match_condition(&cols, &vals, &[], &DatabaseType::SQLite),
            "\"id\" = '42'"
        );
        assert_eq!(
            match_condition(&cols, &vals, &[], &DatabaseType::MySQL),
            "`id` = '42'"
        );
        assert_eq!(
            match_condition(&cols, &vals, &[], &DatabaseType::SQLServer),
            "[id] = '42'"
        );
        assert_eq!(
            match_condition(&cols, &vals, &[], &DatabaseType::Azure),
            "[id] = '42'"
        );
    }

    #[test]
    fn identifiers_with_quotes_are_escaped() {
        let vals = s(&["1"]);
        assert_eq!(
            match_condition(&s(&["a\"b"]), &vals, &[], &DatabaseType::Postgres),
            "\"a\"\"b\" = '1'"
        );
        assert_eq!(
            match_condition(&s(&["a`b"]), &vals, &[], &DatabaseType::MySQL),
            "`a``b` = '1'"
        );
        assert_eq!(
            match_condition(&s(&["a]b"]), &vals, &[], &DatabaseType::SQLServer),
            "[a]]b] = '1'"
        );
    }

    #[test]
    fn composite_condition_in_column_order() {
        let c = match_condition(
            &s(&["a", "b"]),
            &s(&["1", "x"]),
            &[Some("integer".into()), Some("varchar(10)".into())],
            &DatabaseType::Postgres,
        );
        assert_eq!(c, "\"a\" = 1 AND \"b\" = 'x'");
    }

    #[test]
    fn null_value_gives_is_null() {
        let c = match_condition(
            &s(&["a", "b"]),
            &[NULL_CELL.to_string(), "NULL".to_string()],
            &[Some("int".into()), None],
            &DatabaseType::MySQL,
        );
        // The text "NULL" is a string, not NULL.
        assert_eq!(c, "`a` IS NULL AND `b` = 'NULL'");
    }

    #[test]
    fn typed_literals() {
        let types = [
            Some("bigint".to_string()),
            Some("numeric(10,2)".to_string()),
            Some("text".to_string()),
            None,
        ];
        let c = match_condition(
            &s(&["i", "n", "t", "u"]),
            &s(&["7", "1.50", "7", "7"]),
            &types,
            &DatabaseType::Postgres,
        );
        assert_eq!(
            c,
            "\"i\" = 7 AND \"n\" = 1.50 AND \"t\" = '7' AND \"u\" = '7'"
        );
        // Not a number in a numeric column: quoted, never run as SQL.
        let c = match_condition(
            &s(&["i"]),
            &s(&["1 OR 1=1"]),
            &[Some("int".into())],
            &DatabaseType::SQLServer,
        );
        assert_eq!(c, "[i] = '1 OR 1=1'");
    }

    #[test]
    fn values_with_quotes_are_escaped() {
        let c = match_condition(
            &s(&["name"]),
            &s(&["O'Brien"]),
            &[Some("varchar(50)".into())],
            &DatabaseType::SQLite,
        );
        assert_eq!(c, "\"name\" = 'O''Brien'");
    }

    #[test]
    fn follow_targets_the_referenced_row() {
        let fk = ForeignKeyInfo {
            name: "fk".into(),
            columns: s(&["cb", "ca"]),
            ref_table: "su.parent".into(),
            ref_columns: s(&["a", "b"]),
        };
        let cells = [("id", "9"), ("ca", "2"), ("cb", "1")];
        let r = follow(&fk, &row(&cells), &DatabaseType::Postgres);
        assert_eq!(
            r,
            Some((
                "su.parent".to_string(),
                "\"a\" = '1' AND \"b\" = '2'".to_string()
            ))
        );
    }

    #[test]
    fn follow_with_a_null_or_missing_value_links_nothing() {
        let fk = ForeignKeyInfo {
            name: "fk".into(),
            columns: s(&["ca", "cb"]),
            ref_table: "parent".into(),
            ref_columns: s(&["a", "b"]),
        };
        let cells = [("ca", "1"), ("cb", NULL_CELL)];
        assert_eq!(follow(&fk, &row(&cells), &DatabaseType::MySQL), None);
        let cells = [("ca", "1")];
        assert_eq!(follow(&fk, &row(&cells), &DatabaseType::MySQL), None);
    }

    #[test]
    fn referencing_selects_the_pointing_rows() {
        let fk = IncomingForeignKey {
            name: "child_fk".into(),
            table: "dbo.child".into(),
            columns: s(&["parent_id"]),
            ref_columns: s(&["id"]),
        };
        let cells = [("id", "it's")];
        assert_eq!(
            referencing(&fk, &row(&cells), &DatabaseType::SQLServer),
            Some(("dbo.child".to_string(), "[parent_id] = 'it''s'".to_string()))
        );
        let cells = [("id", NULL_CELL)];
        assert_eq!(
            referencing(&fk, &row(&cells), &DatabaseType::SQLServer),
            None
        );
    }

    #[test]
    fn referencing_composite_keeps_the_pairing() {
        let fk = IncomingForeignKey {
            name: String::new(),
            table: "main.child".into(),
            columns: s(&["cb", "ca"]),
            ref_columns: s(&["a", "b"]),
        };
        let cells = [("a", "1"), ("b", "2")];
        assert_eq!(
            referencing(&fk, &row(&cells), &DatabaseType::SQLite),
            Some((
                "main.child".to_string(),
                "\"cb\" = '1' AND \"ca\" = '2'".to_string()
            ))
        );
    }
}
