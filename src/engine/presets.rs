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
        Flavor::CockroachDb => (
            26257,
            SslMode::Require,
            "localhost ou *.cockroachlabs.cloud",
        ),
        Flavor::TimescaleDb => (5432, SslMode::Prefer, "localhost"),
        Flavor::Supabase => (5432, SslMode::Require, "db.<projet>.supabase.co"),
        Flavor::Neon => (5432, SslMode::Require, "ep-….neon.tech"),
        Flavor::Redshift => (5439, SslMode::Require, "….redshift.amazonaws.com"),
    };
    let db_type = flavor.driver();
    let username = match (flavor, &db_type) {
        (Flavor::CockroachDb, _) | (_, DatabaseType::MySQL) => "root",
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
    /// MongoDB: the whole connection string without its password (hosts,
    /// options), saved as `ConnectionConfig::uri`.
    pub uri: Option<String>,
}

const URL_ERROR: &str = "URL non reconnue : schéma attendu postgres://, postgresql://, \
     mysql://, mariadb://, mongodb://, mongodb+srv://, redis:// ou rediss://";

/// Parse `postgres(ql)://`, `mysql://` or `mariadb://` URLs as providers
/// give them (`user:pass@host:port/db?sslmode=require`), `redis(s)://` URLs
/// and MongoDB connection strings (`parse_mongo_uri`).
pub fn parse_url(input: &str) -> Result<ParsedUrl, String> {
    let input = input.trim();
    let scheme = input
        .split_once("://")
        .map(|(s, _)| s.to_ascii_lowercase())
        .unwrap_or_default();
    match scheme.as_str() {
        "mongodb" | "mongodb+srv" => return parse_mongo_uri(input),
        "redis" | "rediss" => return parse_redis_url(input),
        _ => {}
    }
    let url = url::Url::parse(input.trim()).map_err(|_| URL_ERROR.to_string())?;
    let (db_type, scheme_flavor) = match url.scheme() {
        "postgres" | "postgresql" => (DatabaseType::Postgres, None),
        "mysql" => (DatabaseType::MySQL, None),
        "mariadb" => (DatabaseType::MySQL, Some(Flavor::MariaDb)),
        _ => return Err(URL_ERROR.into()),
    };
    // These schemes aren't "special" to the URL standard: the host is
    // opaque and stays percent-encoded.
    let host = match url.host() {
        Some(url::Host::Ipv6(ip)) => ip.to_string(),
        Some(h) => decode(&h.to_string()),
        None => String::new(),
    };
    if host.is_empty() {
        return Err("URL sans hôte".into());
    }
    let username = Some(decode(url.username())).filter(|u| !u.is_empty());
    let password = url
        .password()
        .map(decode)
        .or_else(|| has_empty_password(input.trim()).then(String::new));
    let database = Some(decode(url.path().trim_start_matches('/'))).filter(|d| !d.is_empty());
    let mut ssl_mode = None;
    let mut ssl_ca = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "sslmode" | "ssl-mode" | "ssl_mode" => ssl_mode = parse_ssl_mode(&value),
            // `system` (psql >= 16): the system roots, which are always used.
            "sslrootcert" | "ssl-ca" if value == "system" => ssl_ca = None,
            "sslrootcert" | "ssl-ca" => ssl_ca = Some(std::path::PathBuf::from(value.as_ref())),
            _ => {}
        }
    }
    // libpq: `require` plus a root certificate behaves as `verify-ca`.
    // sqlx only reads the CA when verifying, so say so explicitly rather
    // than leave the user believing the server is pinned.
    if ssl_mode == Some(SslMode::Require) && ssl_ca.is_some() {
        ssl_mode = Some(SslMode::VerifyCa);
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
        uri: None,
    })
}

/// `redis://[user[:password]@]host[:port][/db]`, `rediss://` = TLS verified
/// (`#insecure`: not verified). The database is an index from 0 to 15.
fn parse_redis_url(input: &str) -> Result<ParsedUrl, String> {
    let url = url::Url::parse(input).map_err(|_| URL_ERROR.to_string())?;
    let tls = url.scheme() == "rediss";
    let host = match url.host() {
        Some(url::Host::Ipv6(ip)) => ip.to_string(),
        Some(h) => decode(&h.to_string()),
        None => String::new(),
    };
    if host.is_empty() {
        return Err("URL sans hôte".into());
    }
    let username = Some(decode(url.username())).filter(|u| !u.is_empty());
    let password = url.password().map(decode);
    let database = Some(decode(url.path().trim_start_matches('/'))).filter(|d| !d.is_empty());
    if let Some(db) = &database {
        if !db.parse::<u8>().is_ok_and(|n| n <= 15) {
            return Err(format!("Base Redis invalide : « {db} » (0 à 15)"));
        }
    }
    let ssl_mode = match (tls, url.fragment() == Some("insecure")) {
        (false, _) => None,
        (true, true) => Some(SslMode::Require),
        (true, false) => Some(SslMode::VerifyFull),
    };
    Ok(ParsedUrl {
        db_type: DatabaseType::Redis,
        host: Some(host),
        port: url.port(),
        username,
        password,
        database,
        ssl_mode,
        ssl_ca: None,
        flavor: None,
        uri: None,
    })
}

/// A MongoDB connection string (`mongodb://` or `mongodb+srv://`, possibly
/// several hosts): kept whole in `uri` without its password, which goes to
/// `password`; `host` / `port` are the first host's, `database` the path.
fn parse_mongo_uri(input: &str) -> Result<ParsedUrl, String> {
    let (uri, password) = split_uri_password(input);
    let rest = uri.split_once("://").map_or("", |(_, r)| r);
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    let (userinfo, hosts) = authority.rsplit_once('@').unwrap_or(("", authority));
    let first = hosts.split(',').next().unwrap_or("");
    if first.is_empty() {
        return Err("URL sans hôte".into());
    }
    let (host, port) = if let Some(v6) = first.strip_prefix('[') {
        let (ip, after) = v6.split_once(']').unwrap_or((v6, ""));
        (ip.to_string(), after.strip_prefix(':'))
    } else {
        match first.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), Some(p)),
            None => (first.to_string(), None),
        }
    };
    let port = match port {
        Some(p) => Some(
            p.parse::<u16>()
                .map_err(|_| format!("Port invalide : « {p} »"))?,
        ),
        None => None,
    };
    let username = Some(decode(userinfo)).filter(|u| !u.is_empty());
    let path = tail.strip_prefix('/').unwrap_or("");
    let path = path.split(['?', '#']).next().unwrap_or("");
    let database = Some(decode(path)).filter(|d| !d.is_empty());
    Ok(ParsedUrl {
        db_type: DatabaseType::MongoDB,
        host: Some(decode(&host)),
        port,
        username,
        password,
        database,
        ssl_mode: None,
        ssl_ca: None,
        flavor: None,
        uri: Some(uri),
    })
}

/// A SQL driver's default user (PostgreSQL, MySQL, SQL Server), cleared
/// when the form switches to an engine without one (MongoDB, Redis).
pub fn is_default_user(user: &str) -> bool {
    matches!(user.trim(), "postgres" | "root" | "sa")
}

/// `uri` without the password of its userinfo, and that password decoded
/// (`mongodb://u:p%40ss@h/` → (`mongodb://u@h/`, `p@ss`)).
pub fn split_uri_password(uri: &str) -> (String, Option<String>) {
    let uri = uri.trim();
    let Some((scheme, rest)) = uri.split_once("://") else {
        return (uri.to_string(), None);
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    let Some((userinfo, hosts)) = authority.rsplit_once('@') else {
        return (uri.to_string(), None);
    };
    match userinfo.split_once(':') {
        Some((user, password)) => (
            format!("{scheme}://{user}@{hosts}{tail}"),
            Some(decode(password)),
        ),
        None => (uri.to_string(), None),
    }
}

/// `uri` (saved without a password) with `password` put back, percent-encoded,
/// after its username; unchanged when it has no username.
#[allow(dead_code)] // used by the MongoDB engine (plan 13, Task 2)
pub fn uri_with_password(uri: &str, password: &str) -> String {
    let (uri, _) = split_uri_password(uri);
    let Some((scheme, rest)) = uri.split_once("://") else {
        return uri;
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    match authority.rsplit_once('@') {
        Some((user, hosts)) if !user.is_empty() => {
            let encoded =
                percent_encoding::utf8_percent_encode(password, percent_encoding::NON_ALPHANUMERIC);
            format!("{scheme}://{user}:{encoded}@{hosts}{tail}")
        }
        _ => uri,
    }
}

/// Whether the userinfo of `input` ends with `:` (`user:@host`): an
/// explicitly empty password, which `url` reports as no password.
fn has_empty_password(input: &str) -> bool {
    let rest = input.split_once("://").map_or("", |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    authority
        .rsplit_once('@')
        .is_some_and(|(userinfo, _)| userinfo.ends_with(':'))
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
        assert_eq!(
            Flavor::from_host("db.abc.supabase.co"),
            Some(Flavor::Supabase)
        );
        assert_eq!(
            Flavor::from_host("aws-0-eu.pooler.supabase.com"),
            Some(Flavor::Supabase)
        );
        assert_eq!(
            Flavor::from_host("ep-x-1.eu-central-1.aws.neon.tech"),
            Some(Flavor::Neon)
        );
        assert_eq!(
            Flavor::from_host("aws.connect.psdb.cloud"),
            Some(Flavor::PlanetScale)
        );
        assert_eq!(
            Flavor::from_host("free-tier.gcp-us-central1.cockroachlabs.cloud"),
            Some(Flavor::CockroachDb)
        );
        assert_eq!(
            Flavor::from_host("c.abc.eu-west-1.redshift.amazonaws.com"),
            Some(Flavor::Redshift)
        );
        assert_eq!(
            Flavor::from_host("DB.ABC.SUPABASE.CO"),
            Some(Flavor::Supabase)
        );
        assert_eq!(Flavor::from_host("localhost"), None);
        assert_eq!(Flavor::from_host("notsupabase.co"), None);
    }

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
        let pg = |m: &str| {
            parse_url(&format!("postgres://h/d?sslmode={m}"))
                .unwrap()
                .ssl_mode
        };
        assert_eq!(pg("disable"), Some(SslMode::Disable));
        assert_eq!(pg("allow"), Some(SslMode::Prefer));
        assert_eq!(pg("prefer"), Some(SslMode::Prefer));
        assert_eq!(pg("require"), Some(SslMode::Require));
        assert_eq!(pg("verify-ca"), Some(SslMode::VerifyCa));
        assert_eq!(pg("verify-full"), Some(SslMode::VerifyFull));
        let my = |m: &str| {
            parse_url(&format!("mysql://h/d?ssl-mode={m}"))
                .unwrap()
                .ssl_mode
        };
        assert_eq!(my("DISABLED"), Some(SslMode::Disable));
        assert_eq!(my("PREFERRED"), Some(SslMode::Prefer));
        assert_eq!(my("REQUIRED"), Some(SslMode::Require));
        assert_eq!(my("VERIFY_CA"), Some(SslMode::VerifyCa));
        assert_eq!(my("VERIFY_IDENTITY"), Some(SslMode::VerifyFull));
        assert_eq!(
            parse_url("mysql://h/d?sslmode=REQUIRED").unwrap().ssl_mode,
            Some(SslMode::Require)
        );
        assert_eq!(
            parse_url("postgres://h/d?sslmode=bogus").unwrap().ssl_mode,
            None
        );
    }

    #[test]
    fn explicitly_empty_password() {
        let p = parse_url("postgres://u:@localhost/d").unwrap();
        assert_eq!(p.username.as_deref(), Some("u"));
        assert_eq!(p.password, Some(String::new()));
        let p = parse_url("mysql://u:@h:3306/d?x=a:@b").unwrap();
        assert_eq!(p.password, Some(String::new()));
        // No colon: no password at all; `:@` after the host is not userinfo.
        assert_eq!(
            parse_url("postgres://u@localhost/d").unwrap().password,
            None
        );
        assert_eq!(
            parse_url("postgres://localhost/a:@b").unwrap().password,
            None
        );
        assert_eq!(parse_url("postgres://h/d?q=x:@y").unwrap().password, None);
    }

    #[test]
    fn sslrootcert_system_means_the_system_roots() {
        // psql >= 16 (Neon's docs): the system roots, which are always used.
        let p =
            parse_url("postgresql://u:p@ep-x.neon.tech/db?sslmode=verify-full&sslrootcert=system")
                .unwrap();
        assert_eq!(p.ssl_ca, None);
        assert_eq!(p.ssl_mode, Some(SslMode::VerifyFull));
    }

    #[test]
    fn require_with_a_root_cert_verifies_it_as_libpq_does() {
        let p = parse_url("postgresql://h/d?sslmode=require&sslrootcert=ca.pem").unwrap();
        assert_eq!(p.ssl_mode, Some(SslMode::VerifyCa));
        // Order of the parameters doesn't matter; MySQL spelling too.
        let p = parse_url("mysql://h/d?ssl-ca=ca.pem&ssl-mode=REQUIRED").unwrap();
        assert_eq!(p.ssl_mode, Some(SslMode::VerifyCa));
        // `system` pins nothing; other modes are left alone.
        let p = parse_url("postgresql://h/d?sslmode=require&sslrootcert=system").unwrap();
        assert_eq!(p.ssl_mode, Some(SslMode::Require));
        let p = parse_url("postgresql://h/d?sslmode=verify-full&sslrootcert=ca.pem").unwrap();
        assert_eq!(p.ssl_mode, Some(SslMode::VerifyFull));
        let p = parse_url("postgresql://h/d?sslmode=prefer&sslrootcert=ca.pem").unwrap();
        assert_eq!(p.ssl_mode, Some(SslMode::Prefer));
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
        assert_eq!(p.password, None);
        assert_eq!(p.port, None);
        assert_eq!(p.flavor, None);
    }

    #[test]
    fn rejects_unknown_or_broken_urls() {
        let e = parse_url("ftp://localhost").unwrap_err();
        assert!(e.contains("postgres://"), "{e}");
        assert!(parse_url("postgres://").is_err());
        assert!(parse_url("not a url").is_err());
        assert!(parse_url("").is_err());
    }

    #[test]
    fn redis_urls() {
        let p = parse_url("redis://app:p%40ss@cache.local:6380/3").unwrap();
        assert_eq!(p.db_type, DatabaseType::Redis);
        assert_eq!(p.host.as_deref(), Some("cache.local"));
        assert_eq!(p.port, Some(6380));
        assert_eq!(p.username.as_deref(), Some("app"));
        assert_eq!(p.password.as_deref(), Some("p@ss"));
        assert_eq!(p.database.as_deref(), Some("3"));
        assert_eq!(p.ssl_mode, None);
        assert_eq!(p.uri, None);
        // Password only (`AUTH pw`).
        let p = parse_url("redis://:pw@h").unwrap();
        assert_eq!(p.username, None);
        assert_eq!(p.password.as_deref(), Some("pw"));
        assert_eq!(p.port, None);
        assert_eq!(p.database, None);
        let p = parse_url("rediss://h:6380").unwrap();
        assert_eq!(p.ssl_mode, Some(SslMode::VerifyFull));
        let p = parse_url("rediss://h/0#insecure").unwrap();
        assert_eq!(p.ssl_mode, Some(SslMode::Require));
        assert!(parse_url("redis://h/16").is_err());
        assert!(parse_url("redis://h/x").is_err());
        assert!(parse_url("REDIS://h").is_ok());
    }

    #[test]
    fn mongo_uris_keep_everything_but_the_password() {
        let p = parse_url(
            "mongodb+srv://jane:p%40ss%3Aw%2Frd@cluster0.abc.mongodb.net/shop?retryWrites=true&w=majority",
        )
        .unwrap();
        assert_eq!(p.db_type, DatabaseType::MongoDB);
        assert_eq!(p.password.as_deref(), Some("p@ss:w/rd"));
        assert_eq!(p.username.as_deref(), Some("jane"));
        assert_eq!(p.host.as_deref(), Some("cluster0.abc.mongodb.net"));
        assert_eq!(p.port, None);
        assert_eq!(p.database.as_deref(), Some("shop"));
        assert_eq!(
            p.uri.as_deref(),
            Some("mongodb+srv://jane@cluster0.abc.mongodb.net/shop?retryWrites=true&w=majority")
        );
        // Several hosts are kept; the first one fills host / port.
        let p = parse_url("mongodb://u:p@a.local:27018,b.local:27019/?replicaSet=rs0").unwrap();
        assert_eq!(p.host.as_deref(), Some("a.local"));
        assert_eq!(p.port, Some(27018));
        assert_eq!(p.database, None);
        assert_eq!(
            p.uri.as_deref(),
            Some("mongodb://u@a.local:27018,b.local:27019/?replicaSet=rs0")
        );
        let p = parse_url("mongodb://[::1]:27017").unwrap();
        assert_eq!(p.host.as_deref(), Some("::1"));
        assert_eq!(p.port, Some(27017));
        assert_eq!(p.password, None);
        let p = parse_url("mongodb://h").unwrap();
        assert_eq!(p.uri.as_deref(), Some("mongodb://h"));
        assert!(parse_url("mongodb://").is_err());
        assert!(parse_url("mongodb://h:port/").is_err());
    }

    #[test]
    fn uri_passwords_are_split_and_put_back() {
        assert_eq!(
            split_uri_password("mongodb://u:p%40ss@h/db"),
            ("mongodb://u@h/db".to_string(), Some("p@ss".to_string()))
        );
        assert_eq!(
            split_uri_password("mongodb://u@h/"),
            ("mongodb://u@h/".to_string(), None)
        );
        assert_eq!(
            split_uri_password("mongodb://h/?a=b@c"),
            ("mongodb://h/?a=b@c".to_string(), None)
        );
        assert_eq!(
            split_uri_password("not a uri"),
            ("not a uri".to_string(), None)
        );
        let back = uri_with_password("mongodb://u@h/db?x=1", "p@ss:w/rd");
        assert_eq!(back, "mongodb://u:p%40ss%3Aw%2Frd@h/db?x=1");
        assert_eq!(
            split_uri_password(&back),
            (
                "mongodb://u@h/db?x=1".to_string(),
                Some("p@ss:w/rd".to_string())
            )
        );
        // No username: nothing to attach the password to.
        assert_eq!(uri_with_password("mongodb://h/", "x"), "mongodb://h/");
    }

    #[test]
    fn unknown_schemes_name_every_accepted_one() {
        let e = parse_url("ftp://h").unwrap_err();
        assert!(
            e.contains("mongodb+srv://") && e.contains("rediss://"),
            "{e}"
        );
    }
}
