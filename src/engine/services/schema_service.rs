/// Represents a column definition for schema operations
#[derive(Debug, Clone)]
pub struct ColumnDefinition {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub is_primary_key: bool,
    pub default_value: Option<String>,
}

impl Default for ColumnDefinition {
    fn default() -> Self {
        Self {
            name: String::new(),
            data_type: String::from("VARCHAR(255)"),
            nullable: true,
            is_primary_key: false,
            default_value: None,
        }
    }
}

/// Types of schema modifications
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum SchemaModification {
    AddColumn {
        table_name: String,
        column: ColumnDefinition,
    },
    DropColumn {
        table_name: String,
        column_name: String,
    },
    RenameColumn {
        table_name: String,
        old_name: String,
        new_name: String,
    },
    ModifyColumn {
        table_name: String,
        column: ColumnDefinition,
    },
    CreateTable {
        table_name: String,
        columns: Vec<ColumnDefinition>,
    },
    DropTable {
        table_name: String,
    },
    RenameTable {
        old_name: String,
        new_name: String,
    },
    AddIndex {
        table_name: String,
        index_name: String,
        columns: Vec<String>,
        unique: bool,
    },
    DropIndex {
        table_name: String,
        index_name: String,
    },
}

/// Schema modification service
pub struct SchemaService;

impl SchemaService {
    /// Generate SQL for a schema modification based on database type
    pub fn generate_sql(
        modification: &SchemaModification,
        db_type: &crate::engine::models::DatabaseType,
    ) -> String {
        use crate::engine::models::DatabaseType;
        let quotes = crate::engine::sql::statements::quote_chars(db_type);
        let q = |name: &str| crate::engine::sql::statements::quote_ident(name, quotes);
        // Text inside a '…' string literal (sp_rename arguments).
        let lit = |text: &str| text.replace('\'', "''");

        match modification {
            SchemaModification::AddColumn { table_name, column } => {
                let not_null = if column.nullable { "" } else { " NOT NULL" };
                let default = column
                    .default_value
                    .as_ref()
                    .map(|v| format!(" DEFAULT {}", v))
                    .unwrap_or_default();
                format!(
                    "ALTER TABLE {} ADD COLUMN {} {}{}{}",
                    table_name,
                    q(&column.name),
                    column.data_type,
                    not_null,
                    default
                )
            }
            SchemaModification::DropColumn {
                table_name,
                column_name,
            } => format!("ALTER TABLE {} DROP COLUMN {}", table_name, q(column_name)),
            SchemaModification::RenameColumn {
                table_name,
                old_name,
                new_name,
            } => match db_type {
                // MySQL: RENAME COLUMN needs MySQL 8.0+ (CHANGE would need
                // the full column definition).
                DatabaseType::Postgres | DatabaseType::MySQL | DatabaseType::SQLite => format!(
                    "ALTER TABLE {} RENAME COLUMN {} TO {}",
                    table_name,
                    q(old_name),
                    q(new_name)
                ),
                DatabaseType::SQLServer | DatabaseType::Azure => format!(
                    "EXEC sp_rename '{}.{}', '{}', 'COLUMN'",
                    lit(table_name),
                    lit(&q(old_name)),
                    lit(new_name)
                ),
            },
            SchemaModification::ModifyColumn { table_name, column } => {
                let not_null = if column.nullable {
                    " NULL"
                } else {
                    " NOT NULL"
                };
                match db_type {
                    DatabaseType::Postgres => format!(
                        "ALTER TABLE {} ALTER COLUMN {} TYPE {}",
                        table_name,
                        q(&column.name),
                        column.data_type
                    ),
                    DatabaseType::MySQL => format!(
                        "ALTER TABLE {} MODIFY COLUMN {} {}{}",
                        table_name,
                        q(&column.name),
                        column.data_type,
                        not_null
                    ),
                    // SQLite doesn't support ALTER COLUMN, requires table recreation
                    DatabaseType::SQLite => format!(
                        "-- SQLite doesn't support MODIFY COLUMN, recreate table required\n-- Column: {} {}",
                        column.name.replace('\n', " "),
                        column.data_type
                    ),
                    DatabaseType::SQLServer | DatabaseType::Azure => format!(
                        "ALTER TABLE {} ALTER COLUMN {} {}{}",
                        table_name,
                        q(&column.name),
                        column.data_type,
                        not_null
                    ),
                }
            }
            SchemaModification::CreateTable {
                table_name,
                columns,
            } => {
                let column_defs: Vec<String> = columns
                    .iter()
                    .map(|col| {
                        let mut def = format!("{} {}", q(&col.name), col.data_type);
                        if !col.nullable {
                            def.push_str(" NOT NULL");
                        }
                        if col.is_primary_key {
                            def.push_str(" PRIMARY KEY");
                        }
                        if let Some(ref default) = col.default_value {
                            def.push_str(&format!(" DEFAULT {}", default));
                        }
                        def
                    })
                    .collect();
                format!(
                    "CREATE TABLE {} (\n  {}\n)",
                    table_name,
                    column_defs.join(",\n  ")
                )
            }
            SchemaModification::DropTable { table_name } => {
                format!("DROP TABLE {}", table_name)
            }
            SchemaModification::RenameTable { old_name, new_name } => match db_type {
                DatabaseType::Postgres | DatabaseType::MySQL | DatabaseType::SQLite => {
                    format!("ALTER TABLE {} RENAME TO {}", old_name, new_name)
                }
                DatabaseType::SQLServer | DatabaseType::Azure => {
                    format!("EXEC sp_rename '{}', '{}'", lit(old_name), lit(new_name))
                }
            },
            SchemaModification::AddIndex {
                table_name,
                index_name,
                columns,
                unique,
            } => {
                let unique_str = if *unique { "UNIQUE " } else { "" };
                let cols: Vec<String> = columns.iter().map(|c| q(c)).collect();
                format!(
                    "CREATE {}INDEX {} ON {} ({})",
                    unique_str,
                    q(index_name),
                    table_name,
                    cols.join(", ")
                )
            }
            SchemaModification::DropIndex {
                table_name,
                index_name,
            } => match db_type {
                DatabaseType::Postgres | DatabaseType::SQLite => {
                    format!("DROP INDEX {}", q(index_name))
                }
                DatabaseType::MySQL | DatabaseType::SQLServer | DatabaseType::Azure => {
                    format!("DROP INDEX {} ON {}", q(index_name), table_name)
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::models::DatabaseType;

    #[test]
    fn identifiers_are_quoted_with_the_closing_quote_doubled() {
        let column = ColumnDefinition {
            name: "we\"ird".into(),
            data_type: "TEXT".into(),
            ..Default::default()
        };
        let sql = |m: &SchemaModification, db| SchemaService::generate_sql(m, &db);
        let add = SchemaModification::AddColumn {
            table_name: "\"t\"".into(),
            column: column.clone(),
        };
        assert_eq!(
            sql(&add, DatabaseType::Postgres),
            "ALTER TABLE \"t\" ADD COLUMN \"we\"\"ird\" TEXT"
        );
        let rename = SchemaModification::RenameColumn {
            table_name: "t".into(),
            old_name: "a`b".into(),
            new_name: "c".into(),
        };
        assert_eq!(
            sql(&rename, DatabaseType::MySQL),
            "ALTER TABLE t RENAME COLUMN `a``b` TO `c`"
        );
        let rename = SchemaModification::RenameColumn {
            table_name: "[dbo].[t]".into(),
            old_name: "x]y".into(),
            new_name: "it's".into(),
        };
        assert_eq!(
            sql(&rename, DatabaseType::SQLServer),
            "EXEC sp_rename '[dbo].[t].[x]]y]', 'it''s', 'COLUMN'"
        );
        let index = SchemaModification::AddIndex {
            table_name: "t".into(),
            index_name: "i\"x".into(),
            columns: vec!["a\"".into(), "b".into()],
            unique: true,
        };
        assert_eq!(
            sql(&index, DatabaseType::SQLite),
            "CREATE UNIQUE INDEX \"i\"\"x\" ON t (\"a\"\"\", \"b\")"
        );
        let create = SchemaModification::CreateTable {
            table_name: "t".into(),
            columns: vec![column],
        };
        assert_eq!(
            sql(&create, DatabaseType::Postgres),
            "CREATE TABLE t (\n  \"we\"\"ird\" TEXT\n)"
        );
        let drop = SchemaModification::DropIndex {
            table_name: "t".into(),
            index_name: "i]".into(),
        };
        assert_eq!(sql(&drop, DatabaseType::SQLServer), "DROP INDEX [i]]] ON t");
    }
}
