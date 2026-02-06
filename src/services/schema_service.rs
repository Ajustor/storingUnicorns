use anyhow::Result;

use crate::db::DatabaseConnection;

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
        db_type: &crate::models::DatabaseType,
    ) -> String {
        let (quote_start, quote_end) = Self::get_quote_chars(db_type);

        match modification {
            SchemaModification::AddColumn { table_name, column } => {
                let not_null = if column.nullable { "" } else { " NOT NULL" };
                let default = column
                    .default_value
                    .as_ref()
                    .map(|v| format!(" DEFAULT {}", v))
                    .unwrap_or_default();
                format!(
                    "ALTER TABLE {} ADD COLUMN {}{}{} {}{}{}",
                    table_name,
                    quote_start,
                    column.name,
                    quote_end,
                    column.data_type,
                    not_null,
                    default
                )
            }
            SchemaModification::DropColumn {
                table_name,
                column_name,
            } => {
                format!(
                    "ALTER TABLE {} DROP COLUMN {}{}{}",
                    table_name, quote_start, column_name, quote_end
                )
            }
            SchemaModification::RenameColumn {
                table_name,
                old_name,
                new_name,
            } => match db_type {
                crate::models::DatabaseType::Postgres => {
                    format!(
                        "ALTER TABLE {} RENAME COLUMN {}{}{} TO {}{}{}",
                        table_name,
                        quote_start,
                        old_name,
                        quote_end,
                        quote_start,
                        new_name,
                        quote_end
                    )
                }
                crate::models::DatabaseType::MySQL => {
                    // MySQL requires CHANGE with full column definition, but for rename we use RENAME COLUMN (MySQL 8.0+)
                    format!(
                        "ALTER TABLE {} RENAME COLUMN {}{}{} TO {}{}{}",
                        table_name,
                        quote_start,
                        old_name,
                        quote_end,
                        quote_start,
                        new_name,
                        quote_end
                    )
                }
                crate::models::DatabaseType::SQLite => {
                    format!(
                        "ALTER TABLE {} RENAME COLUMN {}{}{} TO {}{}{}",
                        table_name,
                        quote_start,
                        old_name,
                        quote_end,
                        quote_start,
                        new_name,
                        quote_end
                    )
                }
                crate::models::DatabaseType::SQLServer | crate::models::DatabaseType::Azure => {
                    format!(
                        "EXEC sp_rename '{}.{}', '{}', 'COLUMN'",
                        table_name, old_name, new_name
                    )
                }
            },
            SchemaModification::ModifyColumn { table_name, column } => {
                let not_null = if column.nullable {
                    " NULL"
                } else {
                    " NOT NULL"
                };
                match db_type {
                    crate::models::DatabaseType::Postgres => {
                        format!(
                            "ALTER TABLE {} ALTER COLUMN {}{}{} TYPE {}",
                            table_name, quote_start, column.name, quote_end, column.data_type
                        )
                    }
                    crate::models::DatabaseType::MySQL => {
                        format!(
                            "ALTER TABLE {} MODIFY COLUMN {}{}{} {}{}",
                            table_name,
                            quote_start,
                            column.name,
                            quote_end,
                            column.data_type,
                            not_null
                        )
                    }
                    crate::models::DatabaseType::SQLite => {
                        // SQLite doesn't support ALTER COLUMN, requires table recreation
                        format!(
                            "-- SQLite doesn't support MODIFY COLUMN, recreate table required\n-- Column: {} {}",
                            column.name, column.data_type
                        )
                    }
                    crate::models::DatabaseType::SQLServer | crate::models::DatabaseType::Azure => {
                        format!(
                            "ALTER TABLE {} ALTER COLUMN {}{}{} {}{}",
                            table_name,
                            quote_start,
                            column.name,
                            quote_end,
                            column.data_type,
                            not_null
                        )
                    }
                }
            }
            SchemaModification::CreateTable {
                table_name,
                columns,
            } => {
                let column_defs: Vec<String> = columns
                    .iter()
                    .map(|col| {
                        let mut def =
                            format!("{}{}{} {}", quote_start, col.name, quote_end, col.data_type);
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
                crate::models::DatabaseType::Postgres | crate::models::DatabaseType::MySQL => {
                    format!("ALTER TABLE {} RENAME TO {}", old_name, new_name)
                }
                crate::models::DatabaseType::SQLite => {
                    format!("ALTER TABLE {} RENAME TO {}", old_name, new_name)
                }
                crate::models::DatabaseType::SQLServer | crate::models::DatabaseType::Azure => {
                    format!("EXEC sp_rename '{}', '{}'", old_name, new_name)
                }
            },
            SchemaModification::AddIndex {
                table_name,
                index_name,
                columns,
                unique,
            } => {
                let unique_str = if *unique { "UNIQUE " } else { "" };
                let cols: Vec<String> = columns
                    .iter()
                    .map(|c| format!("{}{}{}", quote_start, c, quote_end))
                    .collect();
                format!(
                    "CREATE {}INDEX {}{}{} ON {} ({})",
                    unique_str,
                    quote_start,
                    index_name,
                    quote_end,
                    table_name,
                    cols.join(", ")
                )
            }
            SchemaModification::DropIndex {
                table_name,
                index_name,
            } => match db_type {
                crate::models::DatabaseType::Postgres | crate::models::DatabaseType::SQLite => {
                    format!("DROP INDEX {}{}{}", quote_start, index_name, quote_end)
                }
                crate::models::DatabaseType::MySQL => {
                    format!(
                        "DROP INDEX {}{}{} ON {}",
                        quote_start, index_name, quote_end, table_name
                    )
                }
                crate::models::DatabaseType::SQLServer | crate::models::DatabaseType::Azure => {
                    format!(
                        "DROP INDEX {}{}{} ON {}",
                        quote_start, index_name, quote_end, table_name
                    )
                }
            },
        }
    }

    /// Execute a schema modification
    pub async fn execute(
        conn: &DatabaseConnection,
        modification: &SchemaModification,
        db_type: &crate::models::DatabaseType,
    ) -> Result<()> {
        let sql = Self::generate_sql(modification, db_type);
        conn.execute_query(&sql).await?;
        Ok(())
    }

    fn get_quote_chars(db_type: &crate::models::DatabaseType) -> (char, char) {
        match db_type {
            crate::models::DatabaseType::Postgres => ('"', '"'),
            crate::models::DatabaseType::MySQL => ('`', '`'),
            crate::models::DatabaseType::SQLite => ('"', '"'),
            crate::models::DatabaseType::SQLServer | crate::models::DatabaseType::Azure => {
                ('[', ']')
            }
        }
    }
}

/// Common data types for each database
pub fn get_common_data_types(db_type: &crate::models::DatabaseType) -> Vec<&'static str> {
    match db_type {
        crate::models::DatabaseType::Postgres => vec![
            "INTEGER",
            "BIGINT",
            "SERIAL",
            "BIGSERIAL",
            "SMALLINT",
            "DECIMAL",
            "NUMERIC",
            "REAL",
            "DOUBLE PRECISION",
            "VARCHAR(255)",
            "CHAR(1)",
            "TEXT",
            "BOOLEAN",
            "DATE",
            "TIME",
            "TIMESTAMP",
            "TIMESTAMPTZ",
            "UUID",
            "JSONB",
            "JSON",
            "BYTEA",
        ],
        crate::models::DatabaseType::MySQL => vec![
            "INT",
            "BIGINT",
            "SMALLINT",
            "TINYINT",
            "DECIMAL(10,2)",
            "FLOAT",
            "DOUBLE",
            "VARCHAR(255)",
            "CHAR(1)",
            "TEXT",
            "MEDIUMTEXT",
            "LONGTEXT",
            "BOOLEAN",
            "DATE",
            "TIME",
            "DATETIME",
            "TIMESTAMP",
            "BLOB",
            "JSON",
        ],
        crate::models::DatabaseType::SQLite => vec!["INTEGER", "REAL", "TEXT", "BLOB", "NUMERIC"],
        crate::models::DatabaseType::SQLServer | crate::models::DatabaseType::Azure => vec![
            "INT",
            "BIGINT",
            "SMALLINT",
            "TINYINT",
            "DECIMAL(10,2)",
            "FLOAT",
            "REAL",
            "VARCHAR(255)",
            "NVARCHAR(255)",
            "CHAR(1)",
            "NCHAR(1)",
            "TEXT",
            "NTEXT",
            "BIT",
            "DATE",
            "TIME",
            "DATETIME",
            "DATETIME2",
            "DATETIMEOFFSET",
            "UNIQUEIDENTIFIER",
            "VARBINARY(MAX)",
        ],
    }
}
