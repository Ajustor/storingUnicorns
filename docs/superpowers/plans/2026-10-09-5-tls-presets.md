# TLS et préréglages PG/MySQL — plan d'implémentation

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Connexions PostgreSQL / MySQL chiffrées (mode SSL, CA), mots de passe
quelconques, préréglages Supabase / Neon / PlanetScale / CockroachDB / Redshift /
MariaDB / TimescaleDB, collage d'URL, en GUI et en TUI.

**Architecture:** Le modèle (`ConnectionConfig`) gagne `ssl_mode`, `ssl_ca`,
`flavor`. Un module pur `engine::presets` porte la table des préréglages et
l'analyse d'URL. `postgres::connect` / `mysql::connect` construisent leurs
options champ par champ depuis la config (plus d'URL), avec TLS rustls + racines
du système. Les erreurs TLS sont traduites en français. Les UIs (GUI egui, TUI
ratatui) ne font qu'appliquer ces fonctions.

**Tech Stack:** Rust 2021, sqlx 0.8 (`tls-rustls-ring-native-roots`), `url` 2 +
`percent-encoding` 2, egui 0.31, ratatui 0.29.

**Spec:** `docs/superpowers/specs/2026-10-09-tls-presets-design.md`.

## Conventions du dépôt

- Build/tests sous Windows : `RUSTC_WRAPPER="C:/Users/alexa/scoop/apps/sccache/current/sccache.exe" cargo test -q`
  (sans cette variable, cargo échoue : « sccache not found »). Les exemples
  ci-dessous écrivent `cargo test` ; ajoute la variable.
- Messages et libellés en français ; commentaires et messages de commit en anglais.
- Commits terminés par `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Ne pas lancer `cargo fmt` sur des fichiers entiers qui ne sont pas déjà
  formatés ; formater ses propres blocs.
- Base : 279 tests réussis, 7 ignorés ; clippy : 27 avertissements. Ne pas en
  ajouter.

## Fichiers

| Fichier | Rôle |
|---|---|
| `Cargo.toml` | feature TLS sqlx, `url`, `percent-encoding` |
| `src/engine/models/connection.rs` | `SslMode`, `Flavor`, nouveaux champs, `display_type`, `to_connection_string` sans mot de passe |
| `src/engine/presets.rs` (nouveau) | table des préréglages, `Flavor::from_host`, `parse_url` |
| `src/engine/mod.rs` | `pub mod presets;` |
| `src/engine/db/postgres.rs`, `mysql.rs` | `connect_options(config)` + `connect(config)` |
| `src/engine/db/tls.rs` (nouveau) | traduction des erreurs de connexion TLS |
| `src/engine/db/connector.rs` | appelle les nouveaux `connect`, traduit les erreurs |
| `src/engine/ops/schema.rs`, `models/connection.rs` | `TableDetails` tolère l'échec des index / clés étrangères |
| `src/gui/explorer/tree.rs`, `mod.rs` | ligne « Non disponible sur ce serveur » ; type affiché |
| `src/gui/dialogs/connection.rs` | modèle, URL, section SSL |
| `src/tui/app_state.rs`, `src/tui/mod.rs`, `src/tui/ui/modals/new_connection.rs`, `src/tui/ui/widgets.rs` | mêmes champs en TUI ; type affiché |
| `src/engine/ops/integration_tests.rs` | MariaDB, CockroachDB, PG TLS, mot de passe spécial |
| `CHANGELOG.md`, `README.md` | documentation |

---

### Task 1 : modèle — `SslMode`, `Flavor`, nouveaux champs

**Files:**
- Modify: `src/engine/models/connection.rs`
- Modify: tout littéral `ConnectionConfig { … }` sans `..Default::default()`
  (le compilateur les liste ; aujourd'hui notamment `src/gui/dialogs/connection.rs`
  `validate`, `src/engine/ops/integration_tests.rs` `mssql_config`)

- [ ] **Step 1 : tests qui échouent** — à la fin de `connection.rs`, dans un
  `#[cfg(test)] mod tests` (le créer s'il n'existe pas) :

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_config_without_new_fields_loads() {
        let json = r#"{"name":"a","db_type":"Postgres","host":"h","port":5432,
            "username":"u","password":"p","database":"d"}"#;
        let c: ConnectionConfig = serde_json::from_str(json).unwrap();
        assert_eq!(c.ssl_mode, None);
        assert_eq!(c.ssl_ca, None);
        assert_eq!(c.flavor, None);
        assert_eq!(c.effective_ssl_mode(), SslMode::Prefer);
    }

    #[test]
    fn new_fields_round_trip() {
        let c = ConnectionConfig {
            ssl_mode: Some(SslMode::VerifyFull),
            ssl_ca: Some("C:/ca.pem".into()),
            flavor: Some(Flavor::Supabase),
            ..Default::default()
        };
        let back: ConnectionConfig =
            serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(back.ssl_mode, Some(SslMode::VerifyFull));
        assert_eq!(back.ssl_ca, Some(std::path::PathBuf::from("C:/ca.pem")));
        assert_eq!(back.flavor, Some(Flavor::Supabase));
    }

    #[test]
    fn connection_string_never_contains_the_password() {
        let c = ConnectionConfig {
            username: Some("u".into()),
            password: Some("s3cr3t".into()),
            ..Default::default()
        };
        assert!(!c.to_connection_string().contains("s3cr3t"));
        assert_eq!(c.to_connection_string(), "postgres://u@localhost:5432/postgres");
    }

    #[test]
    fn display_type_shows_a_matching_flavor_only() {
        let mut c = ConnectionConfig {
            flavor: Some(Flavor::Supabase),
            ..Default::default()
        };
        assert_eq!(c.display_type(), "Supabase");
        c.db_type = DatabaseType::MySQL; // Supabase is PostgreSQL: ignored
        assert_eq!(c.display_type(), "MySQL");
        c.flavor = Some(Flavor::MariaDb);
        assert_eq!(c.display_type(), "MariaDB");
    }
}
```

Note : les fichiers de connexions sont en TOML (`config/mod.rs`) ; serde est le
même, le test JSON suffit pour les valeurs par défaut. Vérifie dans
`src/engine/config/mod.rs` que les connexions passent bien par serde et, s'il
existe déjà un test de chargement TOML, ajoute-y un cas sans les nouveaux champs.

- [ ] **Step 2 : lancer** `cargo test -q models::connection` → échec de compilation
  (`ssl_mode` inconnu).

- [ ] **Step 3 : implémentation** — dans `connection.rs`, après `DatabaseType` :

```rust
/// TLS policy for PostgreSQL / MySQL (ignored by the other drivers).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum SslMode {
    Disable,
    /// TLS when the server offers it (psql's default).
    #[default]
    Prefer,
    /// Encrypted, certificate not verified.
    Require,
    /// Encrypted, certificate signed by a trusted CA.
    VerifyCa,
    /// `VerifyCa` + the certificate matches the host name.
    VerifyFull,
}

impl SslMode {
    pub const ALL: [SslMode; 5] = [
        SslMode::Disable,
        SslMode::Prefer,
        SslMode::Require,
        SslMode::VerifyCa,
        SslMode::VerifyFull,
    ];
}

impl std::fmt::Display for SslMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SslMode::Disable => "Désactivé",
            SslMode::Prefer => "Préféré",
            SslMode::Require => "Obligatoire",
            SslMode::VerifyCa => "Vérifier l'autorité (CA)",
            SslMode::VerifyFull => "Vérification complète",
        })
    }
}

/// A PostgreSQL- or MySQL-compatible product. Display and dialog defaults
/// only: the driver is still `db_type`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum Flavor {
    MariaDb,
    PlanetScale,
    CockroachDb,
    TimescaleDb,
    Supabase,
    Neon,
    Redshift,
}

impl Flavor {
    pub const ALL: [Flavor; 7] = [
        Flavor::MariaDb,
        Flavor::PlanetScale,
        Flavor::CockroachDb,
        Flavor::TimescaleDb,
        Flavor::Supabase,
        Flavor::Neon,
        Flavor::Redshift,
    ];

    /// The driver this product speaks.
    pub fn driver(self) -> DatabaseType {
        match self {
            Flavor::MariaDb | Flavor::PlanetScale => DatabaseType::MySQL,
            _ => DatabaseType::Postgres,
        }
    }
}

impl std::fmt::Display for Flavor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Flavor::MariaDb => "MariaDB",
            Flavor::PlanetScale => "PlanetScale",
            Flavor::CockroachDb => "CockroachDB",
            Flavor::TimescaleDb => "TimescaleDB",
            Flavor::Supabase => "Supabase",
            Flavor::Neon => "Neon",
            Flavor::Redshift => "Redshift",
        })
    }
}
```

Dans `ConnectionConfig`, après `color` :

```rust
    /// PostgreSQL / MySQL only; `None` = `SslMode::Prefer`.
    #[serde(default)]
    pub ssl_mode: Option<SslMode>,
    /// Extra trusted CA certificate (PEM).
    #[serde(default)]
    pub ssl_ca: Option<std::path::PathBuf>,
    /// Compatible product shown instead of the bare engine.
    #[serde(default)]
    pub flavor: Option<Flavor>,
```

Dans `impl ConnectionConfig` :

```rust
    pub fn effective_ssl_mode(&self) -> SslMode {
        self.ssl_mode.unwrap_or_default()
    }

    /// The flavor when it matches the driver, else the engine name.
    pub fn display_type(&self) -> String {
        match self.flavor {
            Some(f) if f.driver() == self.db_type => f.to_string(),
            _ => self.db_type.to_string(),
        }
    }
```

`to_connection_string` : retirer le mot de passe des branches Postgres et MySQL
(`postgres://{user}@{host}:{port}/{db}`, `mysql://{user}@{host}:{port}/{db}`) et
mettre à jour la doc-comment : « For display and logs only; never contains the
password. » Les branches SQLite / SQL Server / Azure ne changent pas.

`Default` : `ssl_mode: None, ssl_ca: None, flavor: None`.

Ajouter les trois champs (à `None`) dans chaque littéral signalé par le
compilateur.

- [ ] **Step 4 : lancer** `cargo test -q` → tout passe (le SQLite `connect`
  utilise encore `to_connection_string` : il ne contient pas de mot de passe, rien
  ne change).

- [ ] **Step 5 : commit** `feat(engine): SSL mode, CA and flavor on connections`.

---

### Task 2 : `engine::presets` — table des préréglages et `Flavor::from_host`

**Files:**
- Create: `src/engine/presets.rs`
- Modify: `src/engine/mod.rs` (`pub mod presets;`)

- [ ] **Step 1 : tests qui échouent** (dans `presets.rs`) :

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_flavor_has_a_preset_for_its_driver() {
        for f in Flavor::ALL {
            let p = preset(f);
            assert_eq!(p.flavor, f);
            assert_eq!(p.db_type, f.driver());
        }
        assert_eq!(preset(Flavor::CockroachDb).port, 26257);
        assert_eq!(preset(Flavor::Redshift).port, 5439);
        assert_eq!(preset(Flavor::Supabase).ssl_mode, SslMode::Require);
        assert_eq!(preset(Flavor::PlanetScale).ssl_mode, SslMode::VerifyFull);
        assert_eq!(preset(Flavor::MariaDb).ssl_mode, SslMode::Prefer);
        assert_eq!(preset(Flavor::CockroachDb).username, "root");
        assert_eq!(preset(Flavor::Neon).username, "postgres");
        assert_eq!(preset(Flavor::MariaDb).username, "root");
    }

    #[test]
    fn flavor_from_host() {
        assert_eq!(Flavor::from_host("db.abc.supabase.co"), Some(Flavor::Supabase));
        assert_eq!(
            Flavor::from_host("aws-0-eu.pooler.supabase.com"),
            Some(Flavor::Supabase)
        );
        assert_eq!(Flavor::from_host("ep-x-1.eu-central-1.aws.neon.tech"), Some(Flavor::Neon));
        assert_eq!(Flavor::from_host("aws.connect.psdb.cloud"), Some(Flavor::PlanetScale));
        assert_eq!(
            Flavor::from_host("free-tier.gcp-us-central1.cockroachlabs.cloud"),
            Some(Flavor::CockroachDb)
        );
        assert_eq!(
            Flavor::from_host("c.abc.eu-west-1.redshift.amazonaws.com"),
            Some(Flavor::Redshift)
        );
        assert_eq!(Flavor::from_host("DB.ABC.SUPABASE.CO"), Some(Flavor::Supabase));
        assert_eq!(Flavor::from_host("localhost"), None);
        assert_eq!(Flavor::from_host("notsupabase.co"), None);
    }
}
```

- [ ] **Step 2 : lancer** `cargo test -q presets` → échec de compilation.

- [ ] **Step 3 : implémentation** :

```rust
//! PostgreSQL / MySQL-compatible products: dialog defaults, host detection
//! and provider URL parsing. Pure data and functions, shared by the GUI and
//! the TUI.

use crate::engine::models::{DatabaseType, Flavor, SslMode};

/// What choosing a product fills in the connection dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preset {
    pub flavor: Flavor,
    pub db_type: DatabaseType,
    pub port: u16,
    pub ssl_mode: SslMode,
    /// Default user, set only when the field is empty.
    pub username: &'static str,
    /// Shown as the host field's hint; never written into the field.
    pub host_hint: &'static str,
}

pub fn preset(flavor: Flavor) -> Preset {
    let (port, ssl_mode, host_hint) = match flavor {
        Flavor::MariaDb => (3306, SslMode::Prefer, "localhost"),
        Flavor::PlanetScale => (3306, SslMode::VerifyFull, "aws.connect.psdb.cloud"),
        Flavor::CockroachDb => (26257, SslMode::Require, "localhost ou *.cockroachlabs.cloud"),
        Flavor::TimescaleDb => (5432, SslMode::Prefer, "localhost"),
        Flavor::Supabase => (5432, SslMode::Require, "db.<projet>.supabase.co"),
        Flavor::Neon => (5432, SslMode::Require, "ep-….neon.tech"),
        Flavor::Redshift => (5439, SslMode::Require, "….redshift.amazonaws.com"),
    };
    let db_type = flavor.driver();
    let username = match (flavor, &db_type) {
        (Flavor::CockroachDb, _) => "root",
        (_, DatabaseType::MySQL) => "root",
        _ => "postgres",
    };
    Preset {
        flavor,
        db_type,
        port,
        ssl_mode,
        username,
        host_hint,
    }
}

impl Flavor {
    /// The hosted product serving `host`, from its domain.
    pub fn from_host(host: &str) -> Option<Flavor> {
        let host = host.to_ascii_lowercase();
        let on = |domain: &str| host == domain || host.ends_with(&format!(".{domain}"));
        if on("supabase.co") || on("supabase.com") {
            Some(Flavor::Supabase)
        } else if on("neon.tech") {
            Some(Flavor::Neon)
        } else if on("psdb.cloud") {
            Some(Flavor::PlanetScale)
        } else if on("cockroachlabs.cloud") {
            Some(Flavor::CockroachDb)
        } else if on("redshift.amazonaws.com") {
            Some(Flavor::Redshift)
        } else {
            None
        }
    }
}
```

`src/engine/mod.rs` : ajouter `pub mod presets;` (ordre alphabétique, entre
`ops` et `services`).

- [ ] **Step 4 : lancer** `cargo test -q presets` → PASS.
- [ ] **Step 5 : commit** `feat(engine): presets for PG/MySQL-compatible products`.

---

### Task 3 : `parse_url`

**Files:**
- Modify: `Cargo.toml` (`url = "2"`, `percent-encoding = "2"` dans `# Misc`)
- Modify: `src/engine/presets.rs`

- [ ] **Step 1 : tests qui échouent** (ajouter au `mod tests` de `presets.rs`) :

```rust
    #[test]
    fn parses_a_supabase_url() {
        let p = parse_url(
            "postgresql://postgres:p%40ss%3Aw%2Fd@db.abc.supabase.co:5432/postgres?sslmode=require",
        )
        .unwrap();
        assert_eq!(p.db_type, DatabaseType::Postgres);
        assert_eq!(p.host.as_deref(), Some("db.abc.supabase.co"));
        assert_eq!(p.port, Some(5432));
        assert_eq!(p.username.as_deref(), Some("postgres"));
        assert_eq!(p.password.as_deref(), Some("p@ss:w/d"));
        assert_eq!(p.database.as_deref(), Some("postgres"));
        assert_eq!(p.ssl_mode, Some(SslMode::Require));
        assert_eq!(p.flavor, Some(Flavor::Supabase));
    }

    #[test]
    fn parses_mysql_and_mariadb_schemes() {
        let p = parse_url("mysql://u@aws.connect.psdb.cloud/app?ssl-mode=VERIFY_IDENTITY").unwrap();
        assert_eq!(p.db_type, DatabaseType::MySQL);
        assert_eq!(p.port, None);
        assert_eq!(p.password, None);
        assert_eq!(p.ssl_mode, Some(SslMode::VerifyFull));
        assert_eq!(p.flavor, Some(Flavor::PlanetScale));

        let p = parse_url("mariadb://root:x@localhost:3307/shop").unwrap();
        assert_eq!(p.db_type, DatabaseType::MySQL);
        assert_eq!(p.flavor, Some(Flavor::MariaDb));
        assert_eq!(p.port, Some(3307));
    }

    #[test]
    fn every_ssl_mode_spelling() {
        let pg = |m: &str| parse_url(&format!("postgres://h/d?sslmode={m}")).unwrap().ssl_mode;
        assert_eq!(pg("disable"), Some(SslMode::Disable));
        assert_eq!(pg("allow"), Some(SslMode::Prefer));
        assert_eq!(pg("prefer"), Some(SslMode::Prefer));
        assert_eq!(pg("require"), Some(SslMode::Require));
        assert_eq!(pg("verify-ca"), Some(SslMode::VerifyCa));
        assert_eq!(pg("verify-full"), Some(SslMode::VerifyFull));
        let my = |m: &str| parse_url(&format!("mysql://h/d?ssl-mode={m}")).unwrap().ssl_mode;
        assert_eq!(my("DISABLED"), Some(SslMode::Disable));
        assert_eq!(my("PREFERRED"), Some(SslMode::Prefer));
        assert_eq!(my("REQUIRED"), Some(SslMode::Require));
        assert_eq!(my("VERIFY_CA"), Some(SslMode::VerifyCa));
        assert_eq!(my("VERIFY_IDENTITY"), Some(SslMode::VerifyFull));
        assert_eq!(
            parse_url("mysql://h/d?sslmode=REQUIRED").unwrap().ssl_mode,
            Some(SslMode::Require)
        );
        assert_eq!(parse_url("postgres://h/d?sslmode=bogus").unwrap().ssl_mode, None);
    }

    #[test]
    fn ipv6_root_cert_and_minimal_urls() {
        let p = parse_url("postgres://[::1]:6543/d?sslrootcert=C%3A%2Fca.pem&application_name=x")
            .unwrap();
        assert_eq!(p.host.as_deref(), Some("::1"));
        assert_eq!(p.port, Some(6543));
        assert_eq!(p.ssl_ca, Some(std::path::PathBuf::from("C:/ca.pem")));

        let p = parse_url("  postgres://localhost  ").unwrap();
        assert_eq!(p.host.as_deref(), Some("localhost"));
        assert_eq!(p.database, None);
        assert_eq!(p.username, None);
    }

    #[test]
    fn rejects_unknown_or_broken_urls() {
        let e = parse_url("redis://localhost").unwrap_err();
        assert!(e.contains("postgres://"), "{e}");
        assert!(parse_url("postgres://").is_err());
        assert!(parse_url("not a url").is_err());
    }
```

- [ ] **Step 2 : lancer** `cargo test -q presets` → échec de compilation.

- [ ] **Step 3 : implémentation** (dans `presets.rs`) :

```rust
/// The parts of a provider connection URL; `None` = absent from the URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedUrl {
    pub db_type: DatabaseType,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub database: Option<String>,
    pub ssl_mode: Option<SslMode>,
    pub ssl_ca: Option<std::path::PathBuf>,
    pub flavor: Option<Flavor>,
}

const URL_ERROR: &str = "URL non reconnue : schéma attendu postgres://, postgresql://, mysql:// ou mariadb://";

/// Parse `postgres(ql)://`, `mysql://` or `mariadb://` URLs as providers
/// give them (`user:pass@host:port/db?sslmode=require`).
pub fn parse_url(input: &str) -> Result<ParsedUrl, String> {
    let url = url::Url::parse(input.trim()).map_err(|_| URL_ERROR.to_string())?;
    let (db_type, scheme_flavor) = match url.scheme() {
        "postgres" | "postgresql" => (DatabaseType::Postgres, None),
        "mysql" => (DatabaseType::MySQL, None),
        "mariadb" => (DatabaseType::MySQL, Some(Flavor::MariaDb)),
        _ => return Err(URL_ERROR.into()),
    };
    let host = match url.host() {
        Some(url::Host::Ipv6(ip)) => ip.to_string(),
        Some(h) => decode(&h.to_string()),
        None => return Err("URL sans hôte".into()),
    };
    if host.is_empty() {
        return Err("URL sans hôte".into());
    }
    let username = Some(decode(url.username())).filter(|u| !u.is_empty());
    let password = url.password().map(decode);
    let database = Some(decode(url.path().trim_start_matches('/'))).filter(|d| !d.is_empty());
    let mut ssl_mode = None;
    let mut ssl_ca = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "sslmode" | "ssl-mode" | "ssl_mode" => ssl_mode = parse_ssl_mode(&value),
            "sslrootcert" | "ssl-ca" => ssl_ca = Some(std::path::PathBuf::from(value.as_ref())),
            _ => {}
        }
    }
    let flavor = scheme_flavor.or_else(|| Flavor::from_host(&host));
    Ok(ParsedUrl {
        db_type,
        host: Some(host),
        port: url.port(),
        username,
        password,
        database,
        ssl_mode,
        ssl_ca,
        flavor,
    })
}

fn decode(s: &str) -> String {
    percent_encoding::percent_decode_str(s)
        .decode_utf8_lossy()
        .into_owned()
}

/// libpq (`verify-full`) and MySQL (`VERIFY_IDENTITY`) spellings.
fn parse_ssl_mode(value: &str) -> Option<SslMode> {
    match value.to_ascii_lowercase().replace('_', "-").as_str() {
        "disable" | "disabled" => Some(SslMode::Disable),
        "allow" | "prefer" | "preferred" => Some(SslMode::Prefer),
        "require" | "required" => Some(SslMode::Require),
        "verify-ca" => Some(SslMode::VerifyCa),
        "verify-full" | "verify-identity" => Some(SslMode::VerifyFull),
        _ => None,
    }
}
```

`url::Url::parse("not a url")` échoue (pas de schéma) ; `postgres://` donne un
hôte vide → « URL sans hôte ». `url` décode déjà la query (`query_pairs`) ; le
chemin, l'utilisateur et le mot de passe restent encodés, d'où `decode`.

- [ ] **Step 4 : lancer** `cargo test -q presets` → PASS.
- [ ] **Step 5 : commit** `feat(engine): parse provider connection URLs`.

---

### Task 4 : TLS sqlx et connexion par options

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/engine/db/postgres.rs`, `src/engine/db/mysql.rs`, `src/engine/db/connector.rs`

- [ ] **Step 1 : Cargo.toml** — ajouter la feature à sqlx :

```toml
sqlx = { version = "0.8", features = [
  "runtime-tokio",
  # rustls with the system's certificates (corporate proxies, Keychain…);
  # no OpenSSL (scripts/check-linked-libs.sh).
  "tls-rustls-ring-native-roots",
  "postgres",
  …
```

- [ ] **Step 2 : tests qui échouent** — dans `postgres.rs` (créer
  `#[cfg(test)] mod tests` en bas s'il n'existe pas) :

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::models::{ConnectionConfig, SslMode};
    use sqlx::postgres::PgSslMode;

    #[test]
    fn options_come_from_the_fields() {
        let c = ConnectionConfig {
            host: Some("db.example.com".into()),
            port: Some(6543),
            username: Some("alice".into()),
            password: Some("a@b:c/d#e?f%g".into()),
            database: "app".into(),
            ssl_mode: Some(SslMode::VerifyFull),
            ..Default::default()
        };
        let o = connect_options(&c);
        assert_eq!(o.get_host(), "db.example.com");
        assert_eq!(o.get_port(), 6543);
        assert_eq!(o.get_username(), "alice");
        assert_eq!(o.get_database(), Some("app"));
        assert!(matches!(o.get_ssl_mode(), PgSslMode::VerifyFull));
    }

    #[test]
    fn defaults_and_ssl_mapping() {
        let o = connect_options(&ConnectionConfig {
            host: None,
            port: None,
            username: None,
            ..Default::default()
        });
        assert_eq!(o.get_host(), "localhost");
        assert_eq!(o.get_port(), 5432);
        assert_eq!(o.get_username(), "postgres");
        assert!(matches!(o.get_ssl_mode(), PgSslMode::Prefer));
        for (mode, want) in [
            (SslMode::Disable, PgSslMode::Disable),
            (SslMode::Require, PgSslMode::Require),
            (SslMode::VerifyCa, PgSslMode::VerifyCa),
        ] {
            let c = ConnectionConfig { ssl_mode: Some(mode), ..Default::default() };
            assert_eq!(
                std::mem::discriminant(&connect_options(&c).get_ssl_mode()),
                std::mem::discriminant(&want)
            );
        }
    }
}
```

Le mot de passe n'a pas d'accesseur dans sqlx : il est vérifié de bout en bout
par le test d'intégration (Task 10).

Dans `mysql.rs`, même chose avec `MySqlSslMode` : défauts `localhost`, `3306`,
`root` ; `VerifyFull` → `MySqlSslMode::VerifyIdentity`, `Prefer` → `Preferred`,
`Disable` → `Disabled`, `Require` → `Required`, `VerifyCa` → `VerifyCa`.

- [ ] **Step 3 : lancer** `cargo test -q db::postgres db::mysql` → échec de compilation.

- [ ] **Step 4 : implémentation** — `postgres.rs`, remplacer `connect` :

```rust
use sqlx::postgres::{PgConnectOptions, PgSslMode};

use crate::engine::models::{ConnectionConfig, SslMode};

/// Options built field by field: no URL, so any character works in the
/// password.
pub fn connect_options(config: &ConnectionConfig) -> PgConnectOptions {
    let mut o = PgConnectOptions::new_without_pgpass()
        .host(config.host.as_deref().unwrap_or("localhost"))
        .port(config.port.unwrap_or(5432))
        .username(config.username.as_deref().unwrap_or("postgres"))
        .database(&config.database)
        .ssl_mode(match config.effective_ssl_mode() {
            SslMode::Disable => PgSslMode::Disable,
            SslMode::Prefer => PgSslMode::Prefer,
            SslMode::Require => PgSslMode::Require,
            SslMode::VerifyCa => PgSslMode::VerifyCa,
            SslMode::VerifyFull => PgSslMode::VerifyFull,
        });
    if let Some(p) = &config.password {
        o = o.password(p);
    }
    if let Some(ca) = &config.ssl_ca {
        o = o.ssl_root_cert(ca);
    }
    o
}

/// Connect to PostgreSQL
pub async fn connect(config: &ConnectionConfig) -> Result<PgPool> {
    Ok(PgPool::connect_with(connect_options(config)).await?)
}
```

`mysql.rs`, idem :

```rust
use sqlx::mysql::{MySqlConnectOptions, MySqlSslMode};

use crate::engine::models::{ConnectionConfig, SslMode};

/// Options built field by field: no URL, so any character works in the
/// password.
pub fn connect_options(config: &ConnectionConfig) -> MySqlConnectOptions {
    let mut o = MySqlConnectOptions::new()
        .host(config.host.as_deref().unwrap_or("localhost"))
        .port(config.port.unwrap_or(3306))
        .username(config.username.as_deref().unwrap_or("root"))
        .database(&config.database)
        .ssl_mode(match config.effective_ssl_mode() {
            SslMode::Disable => MySqlSslMode::Disabled,
            SslMode::Prefer => MySqlSslMode::Preferred,
            SslMode::Require => MySqlSslMode::Required,
            SslMode::VerifyCa => MySqlSslMode::VerifyCa,
            SslMode::VerifyFull => MySqlSslMode::VerifyIdentity,
        });
    if let Some(p) = &config.password {
        o = o.password(p);
    }
    if let Some(ca) = &config.ssl_ca {
        o = o.ssl_ca(ca);
    }
    o
}

/// Connect to MySQL
pub async fn connect(config: &ConnectionConfig) -> Result<MySqlPool> {
    Ok(MySqlPool::connect_with(connect_options(config)).await?)
}
```

`connector.rs` : `postgres::connect(config)`, `mysql::connect(config)` ;
`conn_str` ne sert plus qu'à SQLite : le déplacer dans la branche SQLite
(`sqlite::connect(&config.to_connection_string())`).

Chercher les autres appelants : `rg "postgres::connect|mysql::connect" src` et
les adapter.

- [ ] **Step 5 : lancer** `cargo test -q` → PASS. Puis
  `cargo tree -i openssl-sys --target aarch64-apple-darwin -e normal` →
  « nothing to print » (pas d'OpenSSL ajouté sur macOS).
- [ ] **Step 6 : commit** `feat(engine): TLS for PostgreSQL/MySQL; connect without a URL`.

---

### Task 5 : messages d'erreur TLS

**Files:**
- Create: `src/engine/db/tls.rs` (+ `mod tls;` dans `src/engine/db/mod.rs`)
- Modify: `src/engine/db/connector.rs`

- [ ] **Step 1 : tests qui échouent** (`tls.rs`) :

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_tls_failures_get_a_french_hint() {
        let h = |m: &str| explain(m, "db.example.com");
        assert!(h("error returned from database: no pg_hba.conf entry for host \"1.2.3.4\", user \"u\", database \"d\", no encryption")
            .unwrap().contains("passe le mode SSL à Obligatoire"));
        assert!(h("error returned from database: 3159 (HY000): Connections using insecure transport are prohibited while --require_secure_transport=ON.")
            .unwrap().contains("exige une connexion chiffrée"));
        assert!(h("error communicating with database: invalid peer certificate: UnknownIssuer")
            .unwrap().contains("Certificat du serveur non reconnu"));
        assert!(h("invalid peer certificate: NotValidForName")
            .unwrap().contains("ne correspond pas à db.example.com"));
        assert!(h("error occurred while attempting to establish a TLS connection: server does not support TLS")
            .unwrap().contains("ne propose pas de connexion chiffrée"));
        assert_eq!(h("password authentication failed for user \"u\""), None);
    }

    #[test]
    fn unreadable_ca_file_is_reported() {
        let e = check_ca(Some(std::path::Path::new("Z:/does/not/exist.pem"))).unwrap_err();
        assert!(e.starts_with("Certificat CA illisible : Z:/does/not/exist.pem"), "{e}");
        assert!(check_ca(None).is_ok());
    }
}
```

- [ ] **Step 2 : lancer** `cargo test -q db::tls` → échec de compilation.

- [ ] **Step 3 : implémentation** :

```rust
//! French explanations for TLS connection failures (PostgreSQL / MySQL).

use std::path::Path;

/// A hint replacing the driver's message for a known TLS failure.
pub fn explain(message: &str, host: &str) -> Option<String> {
    let m = message.to_ascii_lowercase();
    if (m.contains("pg_hba.conf") && m.contains("no encryption"))
        || m.contains("require_secure_transport")
    {
        Some("Le serveur exige une connexion chiffrée : passe le mode SSL à Obligatoire.".into())
    } else if m.contains("notvalidforname") {
        Some(format!("Le certificat ne correspond pas à {host}."))
    } else if m.contains("unknownissuer") || m.contains("invalid peer certificate") {
        Some(
            "Certificat du serveur non reconnu : indique son certificat CA, ou passe le \
             mode SSL à Obligatoire (chiffré, sans vérification)."
                .into(),
        )
    } else if m.contains("does not support tls") || m.contains("doesn't support tls") {
        Some("Le serveur ne propose pas de connexion chiffrée.".into())
    } else {
        None
    }
}

/// Fail early, in French, when the CA file can't be read.
pub fn check_ca(path: Option<&Path>) -> Result<(), String> {
    match path {
        Some(p) => std::fs::read(p)
            .map(|_| ())
            .map_err(|e| format!("Certificat CA illisible : {} ({e})", p.display())),
        None => Ok(()),
    }
}
```

`connector.rs`, dans `connect`, pour les branches Postgres et MySQL :

```rust
            DatabaseType::Postgres | DatabaseType::MySQL => {
                tls::check_ca(config.ssl_ca.as_deref()).map_err(anyhow::Error::msg)?;
                let host = config.host.as_deref().unwrap_or("localhost");
                let explain = |e: anyhow::Error| match tls::explain(&format!("{e:#}"), host) {
                    Some(hint) => anyhow::anyhow!("{hint}\n({e})"),
                    None => e,
                };
                if config.db_type == DatabaseType::Postgres {
                    Ok(DatabaseConnection::Postgres(postgres::connect(config).await.map_err(explain)?))
                } else {
                    Ok(DatabaseConnection::MySQL(mysql::connect(config).await.map_err(explain)?))
                }
            }
```

(le message d'origine reste entre parenthèses, utile pour un diagnostic.)
Vérifie avec `rg "invalid peer certificate|UnknownIssuer" ~/.cargo/registry/src/*/rustls-0.23*/src/error*`
la casse exacte des messages rustls ; la comparaison est faite en minuscules.

- [ ] **Step 4 : lancer** `cargo test -q` → PASS.
- [ ] **Step 5 : commit** `feat(engine): explain TLS connection failures in French`.

---

### Task 6 : métadonnées secondaires qui dégradent

**Files:**
- Modify: `src/engine/models/connection.rs` (`TableDetails`)
- Modify: `src/engine/ops/schema.rs` (`table_details`, et le littéral ligne ~302)
- Modify: `src/gui/explorer/tree.rs`, `src/gui/explorer/mod.rs`

- [ ] **Step 1 : test qui échoue** — dans les tests de `schema.rs` (il existe un
  test qui construit `TableDetails` vers la ligne 302 ; regarde comment il obtient
  une connexion SQLite en mémoire et réutilise ce montage). Sur SQLite,
  `get_indexes` d'une table inexistante échoue-t-il ? Si oui :

```rust
    #[tokio::test]
    async fn details_survive_failing_index_and_key_queries() {
        // A table that doesn't exist: columns come from the cache seeded
        // below, the index / foreign key queries fail.
        let conn = /* même montage SQLite que les autres tests du fichier */;
        let cache = TableCache::default();
        cache.insert_columns("ghost", vec![/* une colonne `id` */]);
        let d = table_details(&conn, &cache, "ghost").await.unwrap();
        assert_eq!(d.columns.len(), 1);
        assert!(d.indexes.is_empty() && d.foreign_keys.is_empty());
        assert!(d.indexes_error.is_some() || d.foreign_keys_error.is_some());
    }
```

Si SQLite renvoie une liste vide au lieu d'une erreur, teste la fonction pure
`degrade` décrite au step 3 à la place :

```rust
    #[test]
    fn degrade_keeps_the_error_text() {
        let (v, e) = degrade::<u8>(Err(anyhow::anyhow!("relation pg_index does not exist")));
        assert!(v.is_empty());
        assert_eq!(e.as_deref(), Some("relation pg_index does not exist"));
        let (v, e) = degrade(Ok(vec![1u8]));
        assert_eq!((v, e), (vec![1], None));
    }
```

(Adapte les noms `TableCache::insert_columns` à l'API réelle du cache :
`rg "pub fn" src/engine/services/table_cache.rs`.)

- [ ] **Step 2 : lancer** → échec de compilation.

- [ ] **Step 3 : implémentation** — `TableDetails` :

```rust
pub struct TableDetails {
    pub columns: Vec<Column>,
    pub indexes: Vec<IndexInfo>,
    pub foreign_keys: Vec<ForeignKeyInfo>,
    /// Why `indexes` is empty when the server couldn't list them
    /// (CockroachDB, Redshift…).
    pub indexes_error: Option<String>,
    pub foreign_keys_error: Option<String>,
}
```

(ajouter `..` / les champs `None` aux littéraux existants, y compris
`gui/explorer/tree.rs:610`.)

`schema.rs` :

```rust
/// An optional metadata list: an error becomes an empty list plus its text.
fn degrade<T>(r: anyhow::Result<Vec<T>>) -> (Vec<T>, Option<String>) {
    match r {
        Ok(v) => (v, None),
        Err(e) => (Vec::new(), Some(format!("{e:#}"))),
    }
}

pub async fn table_details(
    conn: &DatabaseConnection,
    cache: &TableCache,
    table: &str,
) -> Result<TableDetails> {
    let (columns, indexes, foreign_keys) = tokio::join!(
        cached_columns(conn, cache, table),
        conn.get_indexes(table),
        conn.get_foreign_keys(table),
    );
    let (indexes, indexes_error) = degrade(indexes);
    let (foreign_keys, foreign_keys_error) = degrade(foreign_keys);
    Ok(TableDetails {
        columns: columns?,
        indexes,
        foreign_keys,
        indexes_error,
        foreign_keys_error,
    })
}
```

Explorateur : dans `tree.rs`, ajouter une variante

```rust
    /// `group` couldn't be loaded on this server.
    GroupError { at: TableAt, group: Group },
```

et, dans la construction des groupes (autour de la ligne 290), quand le groupe
est ouvert, `count == 0` et que l'erreur correspondante est `Some` (Keys →
`foreign_keys_error` sauf s'il y a une clé primaire ; Indexes →
`indexes_error`), pousser `self.push(4, RowKind::GroupError { at, group })`.
Dans `mod.rs`, dessiner cette ligne comme les `ConnError` existantes, en texte
faible : « Non disponible sur ce serveur », avec l'erreur complète en
info-bulle (`.on_hover_text(err)`). Ajouter un test dans `tree.rs` sur le modèle
du test existant qui construit `TableDetails` (ligne ~610) : un groupe Indexes
ouvert, vide, avec `indexes_error` → une ligne `GroupError`.

- [ ] **Step 4 : lancer** `cargo test -q` → PASS.
- [ ] **Step 5 : commit** `feat: index / foreign key lists degrade on servers that lack them`.

---

### Task 7 : type affiché partout

**Files:**
- Modify: `src/gui/explorer/mod.rs:412`, `src/tui/ui/widgets.rs:186` et `:1197`

- [ ] **Step 1** — remplacer `config.db_type.to_string()` / `conn.db_type` affichés
  par `config.display_type()` (`rg "db_type\)|db_type\.to_string\(\)" src/gui src/tui`
  pour vérifier qu'il n'en reste pas d'autre à l'affichage ; les usages qui
  choisissent un comportement restent sur `db_type`).
- [ ] **Step 2** — `cargo test -q` → PASS ; commit `feat: show the product (Supabase, MariaDB…) instead of the engine`.

---

### Task 8 : dialogue de connexion GUI

**Files:**
- Modify: `src/gui/dialogs/connection.rs`

- [ ] **Step 1 : tests qui échouent** (dans le `mod tests` existant) :

```rust
    #[test]
    fn preset_fills_driver_port_ssl_and_keeps_typed_values() {
        let mut f = ConnectionForm::new(None);
        f.host = "mine.example".into();
        f.username = String::new();
        f.apply_preset(Some(Flavor::CockroachDb));
        assert_eq!(f.db_type, DatabaseType::Postgres);
        assert_eq!(f.port, "26257");
        assert_eq!(f.ssl_mode, SslMode::Require);
        assert_eq!(f.host, "mine.example");
        assert_eq!(f.username, "root");
        f.apply_preset(None);
        assert_eq!(f.flavor, None);
    }

    #[test]
    fn pasted_url_fills_every_field() {
        let mut f = ConnectionForm::new(None);
        f.url = "postgresql://postgres:p%40ss@db.abc.supabase.co:5432/postgres?sslmode=require".into();
        f.apply_url().unwrap();
        assert_eq!(f.host, "db.abc.supabase.co");
        assert_eq!(f.password, "p@ss");
        assert_eq!(f.ssl_mode, SslMode::Require);
        assert_eq!(f.flavor, Some(Flavor::Supabase));
        assert!(f.url.is_empty());

        f.url = "redis://x".into();
        assert!(f.apply_url().is_err());
        assert_eq!(f.host, "db.abc.supabase.co"); // untouched
    }

    #[test]
    fn validate_keeps_ssl_and_flavor_for_pg_mysql_only() {
        let mut f = ConnectionForm::new(None);
        f.name = "x".into();
        f.ssl_mode = SslMode::VerifyFull;
        f.ssl_ca = "C:/ca.pem".into();
        f.flavor = Some(Flavor::Neon);
        let c = f.validate(&[]).unwrap();
        assert_eq!(c.ssl_mode, Some(SslMode::VerifyFull));
        assert_eq!(c.ssl_ca, Some("C:/ca.pem".into()));
        assert_eq!(c.flavor, Some(Flavor::Neon));
        f.set_db_type(DatabaseType::SQLServer);
        let c = f.validate(&[]).unwrap();
        assert_eq!((c.ssl_mode, c.ssl_ca, c.flavor), (None, None, None));
    }

    #[test]
    fn ssl_settings_are_part_of_the_target() {
        let a = ConnectionConfig::default();
        let b = ConnectionConfig { ssl_mode: Some(SslMode::Require), ..a.clone() };
        assert!(!same_target(&a, &b));
    }
```

- [ ] **Step 2 : lancer** `cargo test -q dialogs::connection` → échec de compilation.

- [ ] **Step 3 : implémentation**
  - Champs de `ConnectionForm` : `pub flavor: Option<Flavor>`, `pub ssl_mode: SslMode`,
    `pub ssl_ca: String`, `pub url: String`, `pub url_error: Option<String>` ;
    `new` les remplit depuis la config (`c.ssl_mode.unwrap_or_default()`,
    `c.ssl_ca.map(|p| p.display().to_string()).unwrap_or_default()`).
  - `fn uses_tls(&self) -> bool { matches!(self.db_type, DatabaseType::Postgres | DatabaseType::MySQL) }`.
  - `set_db_type` : si la variante ne correspond plus au pilote, `self.flavor = None`.
  - Méthodes :

```rust
    /// Choose a product (None = bare engine): driver, port and SSL mode;
    /// the user only when empty; the host is never overwritten.
    pub fn apply_preset(&mut self, flavor: Option<Flavor>) {
        self.flavor = flavor;
        let Some(f) = flavor else { return };
        let p = crate::engine::presets::preset(f);
        self.set_db_type(p.db_type);
        self.port = p.port.to_string();
        self.ssl_mode = p.ssl_mode;
        if self.username.trim().is_empty() || self.username == "postgres" || self.username == "root" {
            self.username = p.username.to_string();
        }
    }

    /// Fill the form from `self.url`; on error nothing changes.
    pub fn apply_url(&mut self) -> Result<(), String> {
        let p = crate::engine::presets::parse_url(&self.url)?;
        self.set_db_type(p.db_type);
        self.flavor = p.flavor;
        if let Some(h) = p.host { self.host = h; }
        self.port = p.port.map(|n| n.to_string()).unwrap_or_else(|| default_port(&self.db_type).to_string());
        if let Some(u) = p.username { self.username = u; }
        if let Some(pw) = p.password { self.password = pw; }
        if let Some(d) = p.database { self.database = d; }
        if let Some(m) = p.ssl_mode { self.ssl_mode = m; }
        if let Some(ca) = p.ssl_ca { self.ssl_ca = ca.display().to_string(); }
        self.url.clear();
        Ok(())
    }
```

  Note : `apply_preset` remplace un utilisateur resté à une valeur par défaut
  (`postgres` / `root`) ; un utilisateur saisi est conservé. Le test l'exige avec
  un champ vide.
  - `validate` : `ssl_mode: self.uses_tls().then_some(self.ssl_mode)`,
    `ssl_ca: if self.uses_tls() { opt(&self.ssl_ca).map(Into::into) } else { None }`,
    `flavor: self.flavor.filter(|f| f.driver() == self.db_type)`.
  - `same_target` : ajouter `&& a.ssl_mode == b.ssl_mode && a.ssl_ca == b.ssl_ca`.
  - `ui` :
    - avant la ligne « Type », une ligne « Modèle » : `ComboBox` « Aucun » +
      `Flavor::ALL` ; à la sélection, `apply_preset`.
    - avant « Modèle », une ligne « Coller une URL » : `TextEdit` (hint
      `postgresql://user:pass@host:5432/db?sslmode=require`) + bouton « Remplir »
      qui appelle `apply_url` et met l'erreur dans `url_error` (affichée en
      `ERROR` sous la grille, comme `self.error`).
    - dans `server_fields`, champ Hôte : `TextEdit::singleline(&mut self.host).hint_text(preset.host_hint)` quand `flavor` est `Some`.
    - après « Base », si `uses_tls()` : ligne « SSL » (`ComboBox` sur
      `SslMode::ALL`) et ligne « Certificat CA » (`TextEdit` + bouton
      `FOLDER_OPEN` ouvrant `rfd::FileDialog::new().add_filter("Certificat", &["pem", "crt", "cer"])`).

- [ ] **Step 4 : lancer** `cargo test -q` → PASS.
- [ ] **Step 5 : vérification visuelle** — `cargo run`, « Nouvelle connexion » :
  choisir Supabase, coller l'URL du test, vérifier les champs ; passer en SQLite :
  la section SSL disparaît. Capture d'écran dans le scratchpad si la session est
  déverrouillée ; sinon le noter dans le rapport.
- [ ] **Step 6 : commit** `feat(gui): presets, URL paste and SSL settings in the connection dialog`.

---

### Task 9 : TUI

**Files:**
- Modify: `src/tui/app_state.rs`, `src/tui/mod.rs` (~l. 680-700), `src/tui/ui/modals/new_connection.rs`

- [ ] **Step 1 : tests qui échouent** (dans `app_state.rs`, `mod tests` existant ou nouveau) :

```rust
    #[test]
    fn tui_form_cycles_presets_and_ssl_and_keeps_them() {
        let mut nc = NewConnectionState::default();
        nc.cycle_flavor(); // Aucun -> MariaDB
        assert_eq!(nc.flavor, Some(Flavor::MariaDb));
        assert_eq!(nc.db_type, DatabaseType::MySQL);
        assert_eq!(nc.port, "3306");
        nc.cycle_ssl_mode();
        let c = nc.to_config();
        assert_eq!(c.flavor, Some(Flavor::MariaDb));
        assert_eq!(c.ssl_mode, Some(SslMode::Require)); // Prefer -> Require
    }

    #[test]
    fn tui_url_field_fills_the_form() {
        let mut nc = NewConnectionState::default();
        nc.url = "mysql://u:p%23w@aws.connect.psdb.cloud/app?ssl-mode=VERIFY_IDENTITY".into();
        nc.apply_url().unwrap();
        assert_eq!(nc.password, "p#w");
        assert_eq!(nc.flavor, Some(Flavor::PlanetScale));
        assert_eq!(nc.ssl_mode, SslMode::VerifyFull);
    }

    #[test]
    fn ssl_fields_are_skipped_for_other_engines() {
        let t = DatabaseType::SQLServer;
        let a = AzureAuthMethod::Credentials;
        assert_ne!(ConnectionField::Database.next_for(&t, &a), ConnectionField::SslMode);
    }
```

- [ ] **Step 2 : lancer** → échec de compilation.

- [ ] **Step 3 : implémentation**
  - `ConnectionField` : ajouter `Url` (après `Name`), `Flavor` (avant `DbType`),
    `SslMode` et `SslCa` (après `Database`) ; mettre à jour `next` / `prev`
    (chaîne : Name → Url → Flavor → DbType → AzureAuth → TenantId → Host → Port →
    Username → Password → Database → SslMode → SslCa → Name) ; `should_skip` :
    `SslMode | SslCa` sautés si `db_type` n'est ni Postgres ni MySQL.
  - `NewConnectionState` : `url: String`, `flavor: Option<Flavor>`,
    `ssl_mode: SslMode`, `ssl_ca: String`, `color: Option<[u8; 3]>` (pour ne plus
    perdre la couleur en éditant). `Default` : vides / `Prefer` / `None`.
    `open_edit_connection_dialog` les remplit depuis la config ; `to_config`
    les reporte avec les mêmes règles que le GUI (Task 8) et `color: self.color`.
  - `get_active_field_value` / `get_active_field_mut` : `Url` → `url`,
    `SslCa` → `ssl_ca` ; `Flavor` et `SslMode` → `""` / `None` (cyclés).
  - `cycle_flavor` : Aucun → `Flavor::ALL[0]` → … → dernier → Aucun ; applique le
    préréglage comme `ConnectionForm::apply_preset` (port, pilote via
    `cycle_db_type` remplacé par une affectation directe + mêmes valeurs par
    défaut que `cycle_db_type`, `ssl_mode`, utilisateur s'il vaut une valeur par
    défaut). Factoriser : extraire de `cycle_db_type` une méthode
    `set_db_type(&mut self, t)` qui applique port/hôte/utilisateur par défaut, et
    l'appeler depuis `cycle_db_type` et `cycle_flavor`.
  - `cycle_ssl_mode` : suivant dans `SslMode::ALL`, en boucle.
  - `apply_url` : comme le GUI, à partir de `parse_url`.
  - `tui/mod.rs` : `Left`/`Right` sur `Flavor` → `cycle_flavor`, sur `SslMode` →
    `cycle_ssl_mode` ; `Enter` sur le champ `Url` (non vide) → `apply_url`, erreur
    dans le message de statut existant (regarde comment les erreurs de
    `Enter`/sauvegarde sont affichées dans ce handler et fais pareil) au lieu
    d'enregistrer.
  - `new_connection.rs` : rendre les quatre champs avec `render_field` /
    le même rendu « cycle » que `DbType` (`◀ valeur ▶`) ; libellés « URL »,
    « Modèle », « SSL », « Certificat CA » ; agrandir la hauteur de la fenêtre
    et le tableau `chunks` en conséquence.

- [ ] **Step 4 : lancer** `cargo test -q` → PASS ; `cargo run -- tui`, créer une
  connexion avec un modèle et un mode SSL, la rééditer : valeurs conservées.
- [ ] **Step 5 : commit** `feat(tui): presets, URL paste and SSL settings`.

---

### Task 10 : tests d'intégration

**Files:**
- Modify: `src/engine/ops/integration_tests.rs`

- [ ] **Step 1 : doc du module** — ajouter aux variables listées :

```text
//! - `SU_IT_MARIADB=mysql://user:pw@host:port/db`
//! - `SU_IT_COCKROACH=postgres://root@host:port/defaultdb` (insecure mode)
//! - `SU_IT_PG_TLS=host:port` + `SU_IT_PG_TLS_CA=<ca.pem>`: a PostgreSQL that
//!   only accepts TLS (`hostssl` + `hostnossl … reject`), user `postgres`,
//!   password `a@b:c/d#e?f%g`, certificate for `localhost` signed by the CA.
```

et, en commentaire du module, les commandes Docker (à vérifier en les lançant) :

```text
//! docker run -d --name su-mariadb -p 43307:3306 -e MARIADB_ROOT_PASSWORD=pw -e MARIADB_DATABASE=it mariadb:11
//! docker run -d --name su-crdb -p 46257:26257 cockroachdb/cockroach:latest start-single-node --insecure
//! scripts/it-pg-tls.sh   # generates the CA + server cert, starts su-pg-tls on 45433
```

- [ ] **Step 2 : script `scripts/it-pg-tls.sh`** :

```bash
#!/usr/bin/env bash
# Start a TLS-only PostgreSQL for the integration tests (SU_IT_PG_TLS).
# Usage: scripts/it-pg-tls.sh [dir]   (certificates go to dir, default ./target/it-tls)
set -euo pipefail
dir="${1:-target/it-tls}"
mkdir -p "$dir"
cd "$dir"
openssl req -x509 -newkey rsa:2048 -nodes -days 30 -subj "/CN=su-test-ca" \
  -keyout ca.key -out ca.pem
openssl req -newkey rsa:2048 -nodes -subj "/CN=localhost" -keyout server.key -out server.csr
printf "subjectAltName=DNS:localhost" > san.ext
openssl x509 -req -in server.csr -CA ca.pem -CAkey ca.key -CAcreateserial -days 30 \
  -extfile san.ext -out server.crt
chmod 600 server.key
printf "hostssl all all all scram-sha-256\nhostnossl all all all reject\nlocal all all trust\n" > pg_hba.conf
docker rm -f su-pg-tls >/dev/null 2>&1 || true
docker run -d --name su-pg-tls -p 45433:5432 \
  -e POSTGRES_PASSWORD='a@b:c/d#e?f%g' \
  -v "$PWD:/tls:ro" postgres:16-alpine \
  -c ssl=on -c ssl_cert_file=/tls/server.crt -c ssl_key_file=/tls/server.key \
  -c hba_file=/tls/pg_hba.conf
echo "SU_IT_PG_TLS=localhost:45433 SU_IT_PG_TLS_CA=$PWD/ca.pem"
```

(le serveur lit la clé avec l'utilisateur `postgres` du conteneur : si
`chmod 600` sur un volume Windows pose problème, copier les fichiers dans le
conteneur avec un `--entrypoint` qui fait `cp /tls/* /var/lib/postgresql/ && chown postgres`;
documenter ce qui marche.)

- [ ] **Step 3 : tests** :

```rust
fn host_port(spec: &str) -> (String, u16) {
    let (h, p) = spec.rsplit_once(':').expect("host:port");
    (h.to_string(), p.parse().expect("port"))
}

#[tokio::test]
#[ignore]
async fn integration_pg_tls() {
    let (Some(spec), Some(ca)) = (env("SU_IT_PG_TLS"), env("SU_IT_PG_TLS_CA")) else {
        eprintln!("SU_IT_PG_TLS / SU_IT_PG_TLS_CA not set, skipping");
        return;
    };
    let (host, port) = host_port(&spec);
    let config = |mode: SslMode, ca: Option<&str>| ConnectionConfig {
        name: "tls".into(),
        db_type: DatabaseType::Postgres,
        host: Some(host.clone()),
        port: Some(port),
        username: Some("postgres".into()),
        password: Some("a@b:c/d#e?f%g".into()),
        database: "postgres".into(),
        ssl_mode: Some(mode),
        ssl_ca: ca.map(Into::into),
        ..Default::default()
    };
    let err = |c| async move { DatabaseConnection::connect(&c).await.err().map(|e| format!("{e:#}")) };

    let e = err(config(SslMode::Disable, None)).await.expect("plain text must be refused");
    assert!(e.contains("exige une connexion chiffrée"), "{e}");

    let conn = DatabaseConnection::connect(&config(SslMode::Require, None)).await.unwrap();
    assert_eq!(scalar(&conn, "SELECT 1").await, "1");
    // The special-character password went through untouched.

    let e = err(config(SslMode::VerifyFull, None)).await.expect("unknown CA must fail");
    assert!(e.contains("Certificat du serveur non reconnu"), "{e}");

    let conn = DatabaseConnection::connect(&config(SslMode::VerifyFull, Some(&ca))).await.unwrap();
    assert_eq!(scalar(&conn, "SHOW ssl").await, "on");
}

#[tokio::test]
#[ignore]
async fn integration_mariadb() {
    let Some(dsn) = env("SU_IT_MARIADB") else {
        eprintln!("SU_IT_MARIADB not set, skipping");
        return;
    };
    // Same suite as MySQL: MariaDB speaks its protocol.
    run_mysql_suite(&dsn).await;
}

#[tokio::test]
#[ignore]
async fn integration_cockroach() {
    let Some(dsn) = env("SU_IT_COCKROACH") else {
        eprintln!("SU_IT_COCKROACH not set, skipping");
        return;
    };
    let conn = DatabaseConnection::Postgres(sqlx::PgPool::connect(&dsn).await.unwrap());
    exec(&conn, "DROP TABLE IF EXISTS su_crdb").await;
    exec(&conn, "CREATE TABLE su_crdb (id INT PRIMARY KEY, name STRING)").await;
    exec(&conn, "INSERT INTO su_crdb VALUES (1, 'a')").await;
    let schemas = conn.get_schemas().await.unwrap();
    assert!(schemas.iter().any(|s| s.tables.iter().any(|t| t.contains("su_crdb"))));
    let cache = TableCache::default();
    // Details must not fail even where Cockroach lacks catalog pieces.
    let d = table_details(&conn, &cache, "public.su_crdb").await.unwrap();
    assert!(d.columns.iter().any(|c| c.name == "name"));
    assert_eq!(scalar(&conn, "SELECT name FROM su_crdb WHERE id = 1").await, "a");
}
```

`run_mysql_suite` : extraire le corps de `integration_mysql` (après la lecture de
la variable) dans `async fn run_mysql_suite(dsn: &str)`, et l'appeler depuis
`integration_mysql` et `integration_mariadb`. Si une assertion de la suite MySQL
échoue sur MariaDB pour une différence attendue (ex. décodage JSON en
`LONGTEXT`), ajouter un paramètre `mariadb: bool` qui adapte uniquement ce cas et
le commenter. Adapter `get_schemas` / `SchemaInfo` aux noms réels
(`rg "pub async fn get_schemas|pub struct SchemaInfo" src/engine`).

Ajouter aussi, dans `integration_mysql` existant, un utilisateur avec un mot de
passe spécial :

```rust
    exec(&conn, "DROP USER IF EXISTS 'su_special'@'%'").await;
    exec(&conn, "CREATE USER 'su_special'@'%' IDENTIFIED BY 'a@b:c/d#e?f%g'").await;
    let url = url::Url::parse(&dsn).unwrap();
    let special = ConnectionConfig {
        db_type: DatabaseType::MySQL,
        host: url.host_str().map(Into::into),
        port: url.port(),
        username: Some("su_special".into()),
        password: Some("a@b:c/d#e?f%g".into()),
        database: String::new(),
        ..Default::default()
    };
    let c = DatabaseConnection::connect(&special).await.unwrap();
    assert_eq!(scalar(&c, "SELECT 1").await, "1");
```

(`database` vide : vérifier que `MySqlConnectOptions::database("")` ne casse
pas ; sinon n'appeler `.database()` que si non vide, dans `connect_options`.)

- [ ] **Step 4 : lancer les serveurs et les tests** — avec un `DOCKER_CONFIG`
  temporaire vide (comme pour les tests existants) :

```bash
cargo test -q -- --ignored integration
```

avec toutes les variables (`SU_IT_PG`, `SU_IT_MYSQL`, `SU_IT_MSSQL`,
`SU_IT_MARIADB`, `SU_IT_COCKROACH`, `SU_IT_PG_TLS`, `SU_IT_PG_TLS_CA`). Tout
doit passer, y compris les anciens.
- [ ] **Step 5 : commit** `test: MariaDB, CockroachDB, TLS-only PostgreSQL and special passwords`.

---

### Task 11 : documentation

**Files:**
- Modify: `CHANGELOG.md`, `README.md`

- [ ] **Step 1** — `CHANGELOG.md`, nouvelle section en tête (au-dessus de
  `## [0.9.1]`) :

```markdown
## [Non publié]

### Nouveautés

- **Connexions chiffrées (SSL/TLS)** pour PostgreSQL et MySQL : mode SSL
  (désactivé, préféré, obligatoire, vérification de l'autorité ou complète) et
  certificat CA optionnel. Les certificats du système sont reconnus.
- **Modèles de connexion** : MariaDB, PlanetScale, CockroachDB, TimescaleDB,
  Supabase, Neon et Redshift, avec le bon port et le bon mode SSL ; le produit
  s'affiche dans l'explorateur.
- **Coller une URL** (`postgresql://…?sslmode=require`) remplit le formulaire de
  connexion.

### Corrections

- Les mots de passe contenant `@`, `:`, `/`, `#`, `?` ou `%` fonctionnent.
- Les index et clés étrangères absents d'un serveur (CockroachDB, Redshift)
  n'empêchent plus d'afficher la table.
```

Vérifier que `python scripts/changelog.py CHANGELOG.md v0.9.1` sort toujours la
section 0.9.1 (la section « Non publié » ne doit pas la perturber).

- [ ] **Step 2** — `README.md` : dans la liste des bases supportées, ajouter les
  produits compatibles et une phrase sur le SSL et le collage d'URL (suivre le
  style de la section existante : `rg -n "SQLite|SQL Server" README.md`).
- [ ] **Step 3** — `cargo test -q` → PASS ; `cargo clippy -q 2>&1 | grep -c warning`
  → 27 au plus ; commit `docs: TLS, presets and URL paste`.

---

### Task 12 : PR

- [ ] `git push -u origin feat/tls-presets`, ouvrir la PR vers `master` (titre
  français, corps : résumé, tests unitaires et d'intégration lancés, points non
  testés : Supabase/Neon/PlanetScale/Redshift hébergés). Attendre la CI verte
  sur les trois plateformes.
