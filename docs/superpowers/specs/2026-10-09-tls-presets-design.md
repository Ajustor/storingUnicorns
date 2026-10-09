# Connexions chiffrées et moteurs compatibles PG/MySQL (sous-projet A)

Date : 2026-10-09. Premier des sous-projets « plus de moteurs » :

- A (ce document) : TLS et préréglages pour les moteurs compatibles PostgreSQL / MySQL ;
- B : Oracle, DuckDB, ClickHouse, avec une interface « dialecte » commune ;
- C : MongoDB, Redis ;
- D : association des fichiers de base de données (`.sqlite`, `.db`, `.sql`…).

## Objectif

Se connecter aux bases hébergées les plus courantes (Supabase, Neon, PlanetScale,
CockroachDB Cloud, Redshift) et aux variantes auto-hébergées (MariaDB, CockroachDB,
TimescaleDB) sans rien configurer à la main, en chiffrant la connexion.

Aujourd'hui sqlx est compilé sans TLS : toute base qui exige SSL est inaccessible.
De plus, le mot de passe est inséré tel quel dans l'URL de connexion : un mot de
passe contenant `@`, `:`, `/`, `#`, `?` ou `%` fait échouer la connexion.

## 1. Modèle de connexion

Nouveaux champs de `ConnectionConfig`, tous `#[serde(default)]` pour que les
fichiers de configuration existants restent valides :

```rust
pub ssl_mode: Option<SslMode>,   // None = SslMode::Prefer
pub ssl_ca: Option<PathBuf>,     // certificat CA (PEM) optionnel
pub flavor: Option<Flavor>,      // variante affichée ; None = moteur nu
```

```rust
pub enum SslMode { Disable, Prefer, Require, VerifyCa, VerifyFull }
pub enum Flavor { MariaDb, PlanetScale, CockroachDb, TimescaleDb, Supabase, Neon, Redshift }
```

- `SslMode` s'applique uniquement à `Postgres` et `MySQL` ; il est ignoré (et
  masqué dans l'interface) pour SQLite, SQL Server et Azure.
- Correspondance MySQL : `Disable` → `Disabled`, `Prefer` → `Preferred`,
  `Require` → `Required`, `VerifyCa` → `VerifyCa`, `VerifyFull` → `VerifyIdentity`.
- `Flavor` ne change que l'affichage et les valeurs par défaut du dialogue ; le
  pilote reste celui de `db_type`. Chaque variante déclare le `db_type` qu'elle
  exige (`Flavor::driver()`), et une config incohérente (ex. `Supabase` + `MySQL`)
  affiche le moteur nu.

### Connexion sans chaîne

`postgres::connect` et `mysql::connect` prennent la `ConnectionConfig` et
construisent `PgConnectOptions` / `MySqlConnectOptions` champ par champ
(`host`, `port`, `username`, `password`, `database`, `ssl_mode`, `ssl_root_cert` /
`ssl_ca`). Plus aucun mot de passe ne passe dans une URL.

`to_connection_string` reste, pour l'affichage et les journaux uniquement ; elle
n'inclut plus le mot de passe (`postgres://user@host:port/db`).

## 2. TLS

- Feature sqlx `tls-rustls-ring-native-roots` : rustls (déjà présent pour tiberius
  et ureq) avec les certificats du système, comme la mise à jour automatique.
  Pas d'OpenSSL ajouté ; `scripts/check-linked-libs.sh` le vérifie en CI.
- `ssl_ca` ajoute un certificat de confiance (RDS, Supabase…). Un fichier illisible
  ou invalide donne l'erreur « Certificat CA illisible : <chemin> (<raison>) ».
- Messages d'erreur traduits à partir des erreurs sqlx :
  - serveur qui refuse le clair (`no pg_hba.conf entry … no encryption`,
    MySQL `--require_secure_transport`) → « Le serveur exige une connexion
    chiffrée : passe le mode SSL à Obligatoire. » ;
  - certificat non reconnu (`UnknownIssuer`, `invalid peer certificate`) →
    « Certificat du serveur non reconnu : indique son certificat CA, ou passe le
    mode SSL à Obligatoire (chiffré, sans vérification). » ;
  - nom d'hôte non couvert (`NotValidForName`) → « Le certificat ne correspond
    pas à <hôte>. » ;
  - serveur sans TLS en mode `Require` ou plus → « Le serveur ne propose pas de
    connexion chiffrée. ».
  Les autres erreurs restent inchangées.

## 3. Préréglages

Module `engine::presets` : une table de données pure.

| Préréglage | Pilote | Port | SSL par défaut | Indication d'hôte |
|---|---|---|---|---|
| MariaDB | MySQL | 3306 | Préféré | `localhost` |
| PlanetScale | MySQL | 3306 | Vérification complète | `aws.connect.psdb.cloud` |
| CockroachDB | PG | 26257 | Obligatoire | `localhost` ou `*.cockroachlabs.cloud` |
| TimescaleDB | PG | 5432 | Préféré | `localhost` |
| Supabase | PG | 5432 | Obligatoire | `db.<projet>.supabase.co` |
| Neon | PG | 5432 | Obligatoire | `ep-….neon.tech` |
| Redshift | PG | 5439 | Obligatoire | `….redshift.amazonaws.com` |

Choisir un préréglage remplit `db_type`, `port`, `ssl_mode`, `flavor`, et met
l'indication d'hôte en texte d'aide du champ (sans écraser un hôte déjà saisi).
L'utilisateur par défaut suit le pilote (`postgres` pour PG, `root` pour MySQL),
sauf CockroachDB qui utilise `root`. Un utilisateur déjà saisi n'est pas écrasé.

## 4. Coller une URL

Fonction pure `engine::presets::parse_url(&str) -> Result<ParsedUrl, String>` :

- schémas : `postgres://`, `postgresql://`, `mysql://`, `mariadb://` ;
- `user:pass@host:port/db`, avec décodage pourcentage de chaque partie, hôte IPv6
  entre crochets, port et base optionnels ;
- paramètres : `sslmode` (`disable|allow|prefer|require|verify-ca|verify-full`,
  `allow` → `Prefer`) et `ssl-mode` / `sslmode` MySQL
  (`DISABLED|PREFERRED|REQUIRED|VERIFY_CA|VERIFY_IDENTITY`), `sslrootcert`
  (→ `ssl_ca`) ; les autres paramètres sont ignorés ;
- variante déduite de l'hôte : `supabase.co` / `supabase.com` → Supabase,
  `neon.tech` → Neon, `psdb.cloud` → PlanetScale, `cockroachlabs.cloud` →
  CockroachDB, `redshift.amazonaws.com` → Redshift ; schéma `mariadb://` →
  MariaDB ;
- une URL invalide renvoie une erreur lisible (« URL non reconnue : schéma
  attendu postgres:// ou mysql:// »), sans modifier les champs.

## 5. Interface

### GUI (`gui/dialogs/connection.rs`)

- Liste « Modèle » au-dessus du type : « Aucun » puis les sept préréglages.
- Champ « Coller une URL » avec bouton « Remplir » ; le résultat remplit tous les
  champs, y compris le mot de passe.
- Section « SSL » (PG/MySQL seulement) : liste du mode, chemin du certificat CA
  avec bouton « Parcourir » (rfd).
- La variante s'affiche à la place du moteur partout où le type de la connexion
  est affiché (explorateur, barre d'état et liste des connexions du TUI).

### TUI (`tui/ui/modals/new_connection.rs`)

- Mêmes champs : modèle (sélection au clavier), URL à coller, mode SSL, chemin CA.
- La liste des connexions affiche la variante.

### CLI

Pas de changement : la CLI utilise les connexions enregistrées.

## 6. Particularités des moteurs compatibles

- CockroachDB et Redshift n'implémentent pas tout le catalogue PostgreSQL. Les
  chargements de métadonnées secondaires (index, clés étrangères, DDL) qui
  échouent n'interrompent plus la connexion : la section concernée reste vide
  avec la note « Non disponible sur ce serveur : <erreur> ». La liste des
  schémas, tables et colonnes doit fonctionner ; si elle échoue, l'erreur est
  affichée comme aujourd'hui.
- MariaDB : le type `JSON` est un alias de `LONGTEXT` ; les valeurs s'affichent
  en texte, ce qui est acceptable.
- PlanetScale (Vitess) : pas de clés étrangères selon la configuration ; la
  section reste vide, sans erreur.

## 7. Tests

### Unitaires

- `parse_url` : schémas, encodage pourcentage (`p%40ss` → `p@ss`), IPv6, port et
  base absents, chaque valeur de `sslmode` / `ssl-mode`, `sslrootcert`, détection
  de la variante, erreurs.
- Préréglages : chaque variante donne le bon pilote, port et mode SSL ; un hôte
  saisi n'est pas écrasé.
- Construction des options : un mot de passe `a@b:c/d#e?f%g` arrive intact dans
  `PgConnectOptions` / `MySqlConnectOptions` ; le mode SSL et le CA sont transmis.
- `to_connection_string` ne contient jamais le mot de passe.
- Sérialisation : un fichier de connexions sans les nouveaux champs se charge, et
  un aller-retour conserve `ssl_mode`, `ssl_ca` et `flavor`.
- Traduction des erreurs TLS : chaque message source donne le bon message.

### Intégration (Docker, `#[ignore]`, comme les tests existants)

Nouvelles variables : `SU_IT_MARIADB`, `SU_IT_COCKROACH`, `SU_IT_PG_TLS`.

- MariaDB 11 : connexion, liste des tables, édition d'une ligne, DDL.
- CockroachDB (`cockroachdb/cockroach`, mode insecure) : connexion, tables,
  colonnes, requête ; les métadonnées absentes dégradent sans erreur.
- PostgreSQL avec TLS obligatoire et un certificat auto-signé généré par le test :
  `Disable` est refusé avec le message « exige une connexion chiffrée »,
  `Require` réussit, `VerifyFull` sans CA échoue avec « certificat non reconnu »,
  `VerifyFull` avec le CA réussit.
- Mot de passe avec caractères spéciaux sur PostgreSQL et MySQL.

Non testés en local : Redshift, PlanetScale, Supabase, Neon (services hébergés).

## Hors périmètre

- SQL Server et Azure : chiffrement déjà géré par tiberius.
- Certificats client (mTLS) et tunnel SSH.
- Nouveaux pilotes (sous-projets B et C).
