use storing_unicorns::models::DatabaseType;
use storing_unicorns::services::schema_service::{ColumnDefinition, SchemaModification, SchemaService};

// ========== ColumnDefinition Tests ==========

#[test]
fn column_definition_default() {
    let col = ColumnDefinition::default();
    assert!(col.name.is_empty());
    assert_eq!(col.data_type, "VARCHAR(255)");
    assert!(col.nullable);
    assert!(!col.is_primary_key);
    assert!(col.default_value.is_none());
}

// ========== AddColumn ==========

#[test]
fn generate_sql_add_column_postgres() {
    let modification = SchemaModification::AddColumn {
        table_name: "users".into(),
        column: ColumnDefinition {
            name: "email".into(),
            data_type: "VARCHAR(255)".into(),
            nullable: false,
            is_primary_key: false,
            default_value: None,
        },
    };
    let sql = SchemaService::generate_sql(&modification, &DatabaseType::Postgres);
    assert_eq!(sql, "ALTER TABLE users ADD COLUMN \"email\" VARCHAR(255) NOT NULL");
}

#[test]
fn generate_sql_add_column_mysql() {
    let modification = SchemaModification::AddColumn {
        table_name: "users".into(),
        column: ColumnDefinition {
            name: "score".into(),
            data_type: "INT".into(),
            nullable: true,
            is_primary_key: false,
            default_value: Some("0".into()),
        },
    };
    let sql = SchemaService::generate_sql(&modification, &DatabaseType::MySQL);
    assert_eq!(sql, "ALTER TABLE users ADD COLUMN `score` INT DEFAULT 0");
}

#[test]
fn generate_sql_add_column_sqlserver() {
    let modification = SchemaModification::AddColumn {
        table_name: "[dbo].[users]".into(),
        column: ColumnDefinition {
            name: "active".into(),
            data_type: "BIT".into(),
            nullable: false,
            is_primary_key: false,
            default_value: Some("1".into()),
        },
    };
    let sql = SchemaService::generate_sql(&modification, &DatabaseType::SQLServer);
    assert!(sql.contains("[active]"));
    assert!(sql.contains("NOT NULL"));
    assert!(sql.contains("DEFAULT 1"));
}

// ========== DropColumn ==========

#[test]
fn generate_sql_drop_column_postgres() {
    let modification = SchemaModification::DropColumn {
        table_name: "users".into(),
        column_name: "old_field".into(),
    };
    let sql = SchemaService::generate_sql(&modification, &DatabaseType::Postgres);
    assert_eq!(sql, "ALTER TABLE users DROP COLUMN \"old_field\"");
}

#[test]
fn generate_sql_drop_column_mysql() {
    let modification = SchemaModification::DropColumn {
        table_name: "users".into(),
        column_name: "temp".into(),
    };
    let sql = SchemaService::generate_sql(&modification, &DatabaseType::MySQL);
    assert_eq!(sql, "ALTER TABLE users DROP COLUMN `temp`");
}

// ========== RenameColumn ==========

#[test]
fn generate_sql_rename_column_postgres() {
    let modification = SchemaModification::RenameColumn {
        table_name: "users".into(),
        old_name: "fname".into(),
        new_name: "first_name".into(),
    };
    let sql = SchemaService::generate_sql(&modification, &DatabaseType::Postgres);
    assert_eq!(sql, "ALTER TABLE users RENAME COLUMN \"fname\" TO \"first_name\"");
}

#[test]
fn generate_sql_rename_column_sqlserver() {
    let modification = SchemaModification::RenameColumn {
        table_name: "users".into(),
        old_name: "fname".into(),
        new_name: "first_name".into(),
    };
    let sql = SchemaService::generate_sql(&modification, &DatabaseType::SQLServer);
    assert_eq!(sql, "EXEC sp_rename 'users.fname', 'first_name', 'COLUMN'");
}

#[test]
fn generate_sql_rename_column_azure() {
    let modification = SchemaModification::RenameColumn {
        table_name: "users".into(),
        old_name: "old_col".into(),
        new_name: "new_col".into(),
    };
    let sql = SchemaService::generate_sql(&modification, &DatabaseType::Azure);
    assert!(sql.contains("EXEC sp_rename"));
}

#[test]
fn generate_sql_rename_column_mysql() {
    let modification = SchemaModification::RenameColumn {
        table_name: "users".into(),
        old_name: "a".into(),
        new_name: "b".into(),
    };
    let sql = SchemaService::generate_sql(&modification, &DatabaseType::MySQL);
    assert_eq!(sql, "ALTER TABLE users RENAME COLUMN `a` TO `b`");
}

#[test]
fn generate_sql_rename_column_sqlite() {
    let modification = SchemaModification::RenameColumn {
        table_name: "users".into(),
        old_name: "x".into(),
        new_name: "y".into(),
    };
    let sql = SchemaService::generate_sql(&modification, &DatabaseType::SQLite);
    assert_eq!(sql, "ALTER TABLE users RENAME COLUMN \"x\" TO \"y\"");
}

// ========== ModifyColumn ==========

#[test]
fn generate_sql_modify_column_postgres() {
    let modification = SchemaModification::ModifyColumn {
        table_name: "users".into(),
        column: ColumnDefinition {
            name: "age".into(),
            data_type: "BIGINT".into(),
            nullable: true,
            is_primary_key: false,
            default_value: None,
        },
    };
    let sql = SchemaService::generate_sql(&modification, &DatabaseType::Postgres);
    assert_eq!(sql, "ALTER TABLE users ALTER COLUMN \"age\" TYPE BIGINT");
}

#[test]
fn generate_sql_modify_column_mysql() {
    let modification = SchemaModification::ModifyColumn {
        table_name: "users".into(),
        column: ColumnDefinition {
            name: "age".into(),
            data_type: "BIGINT".into(),
            nullable: false,
            is_primary_key: false,
            default_value: None,
        },
    };
    let sql = SchemaService::generate_sql(&modification, &DatabaseType::MySQL);
    assert_eq!(sql, "ALTER TABLE users MODIFY COLUMN `age` BIGINT NOT NULL");
}

#[test]
fn generate_sql_modify_column_sqlite() {
    let modification = SchemaModification::ModifyColumn {
        table_name: "users".into(),
        column: ColumnDefinition {
            name: "age".into(),
            data_type: "INT".into(),
            nullable: true,
            is_primary_key: false,
            default_value: None,
        },
    };
    let sql = SchemaService::generate_sql(&modification, &DatabaseType::SQLite);
    assert!(sql.contains("SQLite doesn't support MODIFY COLUMN"));
}

#[test]
fn generate_sql_modify_column_sqlserver() {
    let modification = SchemaModification::ModifyColumn {
        table_name: "tbl".into(),
        column: ColumnDefinition {
            name: "col".into(),
            data_type: "NVARCHAR(MAX)".into(),
            nullable: true,
            is_primary_key: false,
            default_value: None,
        },
    };
    let sql = SchemaService::generate_sql(&modification, &DatabaseType::SQLServer);
    assert_eq!(sql, "ALTER TABLE tbl ALTER COLUMN [col] NVARCHAR(MAX) NULL");
}

// ========== CreateTable ==========

#[test]
fn generate_sql_create_table_postgres() {
    let modification = SchemaModification::CreateTable {
        table_name: "new_table".into(),
        columns: vec![
            ColumnDefinition {
                name: "id".into(),
                data_type: "SERIAL".into(),
                nullable: false,
                is_primary_key: true,
                default_value: None,
            },
            ColumnDefinition {
                name: "name".into(),
                data_type: "VARCHAR(100)".into(),
                nullable: false,
                is_primary_key: false,
                default_value: None,
            },
            ColumnDefinition {
                name: "score".into(),
                data_type: "INT".into(),
                nullable: true,
                is_primary_key: false,
                default_value: Some("0".into()),
            },
        ],
    };
    let sql = SchemaService::generate_sql(&modification, &DatabaseType::Postgres);
    assert!(sql.contains("CREATE TABLE new_table"));
    assert!(sql.contains("\"id\" SERIAL NOT NULL PRIMARY KEY"));
    assert!(sql.contains("\"name\" VARCHAR(100) NOT NULL"));
    assert!(sql.contains("\"score\" INT DEFAULT 0"));
}

// ========== DropTable ==========

#[test]
fn generate_sql_drop_table() {
    let modification = SchemaModification::DropTable {
        table_name: "old_table".into(),
    };
    let sql = SchemaService::generate_sql(&modification, &DatabaseType::Postgres);
    assert_eq!(sql, "DROP TABLE old_table");
}

// ========== RenameTable ==========

#[test]
fn generate_sql_rename_table_postgres() {
    let modification = SchemaModification::RenameTable {
        old_name: "old_name".into(),
        new_name: "new_name".into(),
    };
    let sql = SchemaService::generate_sql(&modification, &DatabaseType::Postgres);
    assert_eq!(sql, "ALTER TABLE old_name RENAME TO new_name");
}

#[test]
fn generate_sql_rename_table_sqlserver() {
    let modification = SchemaModification::RenameTable {
        old_name: "old_name".into(),
        new_name: "new_name".into(),
    };
    let sql = SchemaService::generate_sql(&modification, &DatabaseType::SQLServer);
    assert_eq!(sql, "EXEC sp_rename 'old_name', 'new_name'");
}

// ========== AddIndex ==========

#[test]
fn generate_sql_add_index_non_unique() {
    let modification = SchemaModification::AddIndex {
        table_name: "users".into(),
        index_name: "idx_name".into(),
        columns: vec!["name".into()],
        unique: false,
    };
    let sql = SchemaService::generate_sql(&modification, &DatabaseType::Postgres);
    assert_eq!(sql, "CREATE INDEX \"idx_name\" ON users (\"name\")");
}

#[test]
fn generate_sql_add_index_unique_multiple_columns() {
    let modification = SchemaModification::AddIndex {
        table_name: "orders".into(),
        index_name: "idx_uniq".into(),
        columns: vec!["user_id".into(), "product_id".into()],
        unique: true,
    };
    let sql = SchemaService::generate_sql(&modification, &DatabaseType::Postgres);
    assert_eq!(
        sql,
        "CREATE UNIQUE INDEX \"idx_uniq\" ON orders (\"user_id\", \"product_id\")"
    );
}

#[test]
fn generate_sql_add_index_mysql() {
    let modification = SchemaModification::AddIndex {
        table_name: "users".into(),
        index_name: "idx_email".into(),
        columns: vec!["email".into()],
        unique: false,
    };
    let sql = SchemaService::generate_sql(&modification, &DatabaseType::MySQL);
    assert_eq!(sql, "CREATE INDEX `idx_email` ON users (`email`)");
}

// ========== DropIndex ==========

#[test]
fn generate_sql_drop_index_postgres() {
    let modification = SchemaModification::DropIndex {
        table_name: "users".into(),
        index_name: "idx_email".into(),
    };
    let sql = SchemaService::generate_sql(&modification, &DatabaseType::Postgres);
    assert_eq!(sql, "DROP INDEX \"idx_email\"");
}

#[test]
fn generate_sql_drop_index_mysql() {
    let modification = SchemaModification::DropIndex {
        table_name: "users".into(),
        index_name: "idx_email".into(),
    };
    let sql = SchemaService::generate_sql(&modification, &DatabaseType::MySQL);
    assert_eq!(sql, "DROP INDEX `idx_email` ON users");
}

#[test]
fn generate_sql_drop_index_sqlserver() {
    let modification = SchemaModification::DropIndex {
        table_name: "users".into(),
        index_name: "idx_email".into(),
    };
    let sql = SchemaService::generate_sql(&modification, &DatabaseType::SQLServer);
    assert_eq!(sql, "DROP INDEX [idx_email] ON users");
}

#[test]
fn generate_sql_drop_index_sqlite() {
    let modification = SchemaModification::DropIndex {
        table_name: "users".into(),
        index_name: "idx_email".into(),
    };
    let sql = SchemaService::generate_sql(&modification, &DatabaseType::SQLite);
    assert_eq!(sql, "DROP INDEX \"idx_email\"");
}
