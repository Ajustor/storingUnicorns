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
}
