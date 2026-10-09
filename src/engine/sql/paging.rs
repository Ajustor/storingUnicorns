//! SQL for browsing a table page by page, with the user's filter and ordering.

use crate::engine::models::DatabaseType;

/// A page of a table as shown by a data grid.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DataQuery {
    /// Already-qualified, already-quoted table name.
    pub table: String,
    /// Raw SQL condition typed by the user (without `WHERE`), may be empty.
    pub filter: String,
    /// Raw SQL ordering typed by the user or built by header clicks (without `ORDER BY`), may be empty.
    pub order_by: String,
    /// 0-based page index.
    pub page: usize,
    pub page_size: usize,
}

/// `SELECT * FROM <table> [WHERE (<filter>)]`. The filter is wrapped in
/// parentheses so an `OR` inside it can't escape the condition.
fn select_from(select: &str, q: &DataQuery) -> String {
    let filter = q.filter.trim();
    if filter.is_empty() {
        format!("SELECT {select} FROM {}", q.table)
    } else {
        format!("SELECT {select} FROM {} WHERE ({filter})", q.table)
    }
}

/// The query for page `q.page` of `q.table`, filtered and ordered as asked.
pub fn build_select(q: &DataQuery, db: &DatabaseType) -> String {
    let mut sql = select_from("*", q);
    let order_by = q.order_by.trim();
    let offset = q.page * q.page_size;
    match db {
        DatabaseType::SQLServer | DatabaseType::Azure => {
            // `OFFSET … FETCH` requires an ORDER BY; order by a constant when
            // the user didn't choose one.
            let order_by = if order_by.is_empty() {
                "(SELECT NULL)"
            } else {
                order_by
            };
            sql.push_str(&format!(
                " ORDER BY {order_by} OFFSET {offset} ROWS FETCH NEXT {} ROWS ONLY",
                q.page_size
            ));
        }
        DatabaseType::Postgres | DatabaseType::MySQL | DatabaseType::SQLite => {
            if !order_by.is_empty() {
                sql.push_str(&format!(" ORDER BY {order_by}"));
            }
            sql.push_str(&format!(" LIMIT {} OFFSET {offset}", q.page_size));
        }
    }
    sql
}

/// The number of rows matching the filter (ordering and paging ignored).
pub fn build_count(q: &DataQuery) -> String {
    select_from("COUNT(*)", q)
}

/// Header click: none → `col ASC` → `col DESC` → none. `column` is quoted.
/// Clicking another column than the current ordering starts at `ASC`.
pub fn toggle_order(current: &str, column: &str) -> String {
    let current = current.trim();
    if current.eq_ignore_ascii_case(&format!("{column} ASC")) {
        format!("{column} DESC")
    } else if current.eq_ignore_ascii_case(&format!("{column} DESC")) {
        String::new()
    } else {
        format!("{column} ASC")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(filter: &str, order: &str, page: usize) -> DataQuery {
        DataQuery {
            table: "\"t\"".into(),
            filter: filter.into(),
            order_by: order.into(),
            page,
            page_size: 500,
        }
    }

    #[test]
    fn limit_offset_dialects() {
        assert_eq!(
            build_select(&q("", "", 0), &DatabaseType::Postgres),
            "SELECT * FROM \"t\" LIMIT 500 OFFSET 0"
        );
        assert_eq!(
            build_select(&q("a > 1", "a DESC", 2), &DatabaseType::SQLite),
            "SELECT * FROM \"t\" WHERE (a > 1) ORDER BY a DESC LIMIT 500 OFFSET 1000"
        );
        assert_eq!(
            build_select(&q("", "", 1), &DatabaseType::MySQL),
            "SELECT * FROM \"t\" LIMIT 500 OFFSET 500"
        );
    }

    #[test]
    fn sql_server_needs_order_by_for_offset() {
        assert_eq!(
            build_select(&q("", "", 0), &DatabaseType::SQLServer),
            "SELECT * FROM \"t\" ORDER BY (SELECT NULL) OFFSET 0 ROWS FETCH NEXT 500 ROWS ONLY"
        );
        assert_eq!(
            build_select(&q("x = 1", "[x]", 1), &DatabaseType::Azure),
            "SELECT * FROM \"t\" WHERE (x = 1) ORDER BY [x] OFFSET 500 ROWS FETCH NEXT 500 ROWS ONLY"
        );
    }

    #[test]
    fn filter_and_order_are_trimmed_and_wrapped() {
        // A user filter containing OR must not escape the WHERE when combined later.
        assert_eq!(
            build_count(&q("  a = 1 OR b = 2 ", "", 0)),
            "SELECT COUNT(*) FROM \"t\" WHERE (a = 1 OR b = 2)"
        );
        assert_eq!(build_count(&q("", "x", 0)), "SELECT COUNT(*) FROM \"t\"");
    }

    #[test]
    fn toggle_order_cycles() {
        assert_eq!(toggle_order("", "\"a\""), "\"a\" ASC");
        assert_eq!(toggle_order("\"a\" ASC", "\"a\""), "\"a\" DESC");
        assert_eq!(toggle_order("\"a\" DESC", "\"a\""), "");
        assert_eq!(toggle_order("\"b\" DESC", "\"a\""), "\"a\" ASC");
    }
}
