# Authentification par connexion, indépendante du moteur

Date : 2026-10-09. Sous-projet « Authentification », avant le sous-projet B
(Oracle, DuckDB, ClickHouse) qui s'appuiera dessus.

## Objectif

Choisir la méthode d'authentification d'une connexion indépendamment du moteur,
ne montrer que les champs utiles, ne plus stocker de mot de passe en clair, et
gérer les méthodes des clouds (Entra ID, AWS IAM, GCP IAM) ainsi que
l'authentification Windows intégrée.

Aujourd'hui :

- seules les connexions « Azure SQL » ont des méthodes (`AzureAuthMethod` :
  identifiants SQL, CLI `az`, identité managée via `DefaultAzureCredential`) ;
- la ressource du jeton est toujours `https://database.windows.net`, quel que
  soit le cloud de l'hôte ; on ne peut pas choisir une identité managée
  attribuée par l'utilisateur ;
- les connexions par jeton désactivent la vérification du certificat
  (`trust_cert()`) alors qu'un jeton porteur est envoyé ;
- les mots de passe sont en clair dans `config.toml`.

La branche `fix/azure-identity` (déduction du cloud depuis l'hôte, identité
managée choisie par ID client, vérification du certificat) est intégrée à ce
sous-projet et en constitue la base Entra ID.

## 1. Modèle

```rust
pub enum AuthMethod {
    /// Utilisateur + mot de passe, stocké selon `secret_storage`.
    Password,
    /// Jeton Entra ID ; cloud et ressource déduits de l'hôte.
    EntraCli { tenant: Option<String> },
    EntraManagedIdentity { client_id: Option<String> },   // None = identité système
    EntraServicePrincipal { tenant: String, client_id: String }, // secret au trousseau
    EntraDeviceCode { tenant: Option<String> },
    AwsIam { profile: Option<String>, region: Option<String> }, // region None = déduite de l'hôte
    GcpIam,
    WindowsIntegrated,
}

pub enum SecretStorage { Keyring, Prompt, Plaintext }
```

Nouveaux champs de `ConnectionConfig` (`#[serde(default)]`) :
`auth: Option<AuthMethod>` (None = `Password`) et
`secret_storage: Option<SecretStorage>` (None = `Keyring` pour les nouvelles
connexions ; voir migration). `password` ne reste rempli que pour `Plaintext`.

### Compatibilité par moteur et système

| Méthode | PostgreSQL | MySQL | SQL Server | SQLite | Systèmes |
|---|---|---|---|---|---|
| Password | oui | oui | oui | — | tous |
| Entra (4 variantes) | oui | oui | oui | — | tous |
| AWS IAM | oui | oui | — | — | tous |
| GCP IAM | oui | oui | — | — | tous |
| Windows intégrée | — | — | oui | — | Windows |

Fonction pure `AuthMethod::supported(db_type, os) -> bool` ; les UIs ne
proposent que les méthodes supportées.

### Migration de la configuration (au chargement)

- `db_type = Azure` → `SQLServer`, `flavor = Some(Flavor::AzureSql)` (nouvelle
  variante, détectée aussi d'après l'hôte `*.database.windows.net` & co).
- `azure_auth_method` : `Credentials` → `Password` ; `Interactive` →
  `EntraCli { tenant }` (depuis `tenant_id`, `common` → None) ;
  `ManagedIdentity` → `EntraManagedIdentity { client_id: None }`.
- Mot de passe en clair existant : déplacé dans le trousseau si disponible
  (`secret_storage = Keyring`, `password = None`), sinon conservé avec
  `secret_storage = Plaintext`. Le fichier est réécrit après migration.
- Les anciens champs (`azure_auth_method`, `tenant_id`) sont lus mais plus
  écrits.

## 2. Secrets

- Crate `keyring` (v3) : Windows Credential Manager, macOS Keychain, Linux
  Secret Service en Rust pur (`crypto-rust`, pas d'OpenSSL ;
  `scripts/check-linked-libs.sh` doit rester vert). Service
  `storingUnicorns`, compte `<nom de la connexion>` ; renommer ou supprimer une
  connexion déplace ou supprime le secret.
- Trait `SecretStore` (get / set / delete) avec une implémentation mémoire pour
  les tests ; l'app utilise le trousseau, et bascule sur « indisponible » si le
  trousseau ne répond pas (Linux sans session graphique).
- `Prompt` : le mot de passe est demandé à la connexion (dialogue GUI, champ
  masqué dans le TUI) et gardé en mémoire pour la session uniquement.
- Le secret du principal de service suit les mêmes règles.
- Trousseau indisponible : message « Trousseau du système indisponible : choisis
  “Demander à chaque connexion” ou “En clair dans le fichier”. »

## 3. Fournisseurs de jetons

Trait `TokenProvider` : `async fn token(&self) -> Result<Token>` avec
`Token { secret, expires_at }`.

- **Entra ID** : cloud déduit de l'hôte (public, US Government, Chine ; hôte
  inconnu → public) pour la ressource (`<resource>/.default`) et l'autorité.
  Ressources : SQL Server → `https://database.windows.net` (ou l'équivalent du
  cloud) ; PostgreSQL / MySQL → `https://ossrdbms-aad.database.windows.net`
  (ou `…usgovcloudapi.net`, `…chinacloudapi.cn`). Hôtes Azure Database :
  `*.postgres.database.azure.com`, `*.mysql.database.azure.com` (+ clouds).
  - CLI : `az account get-access-token --resource … [--tenant …]` ; message si
    le cloud actif du CLI ne correspond pas (`az cloud set --name …`).
  - Identité managée : `ManagedIdentityCredential` (client ID facultatif,
    validé comme GUID).
  - Principal de service : `ClientSecretCredential` (tenant, client ID, secret).
  - Device code : `DeviceCodeCredential` ; le code et l'URL s'affichent dans
    l'app (GUI : dialogue avec bouton « Copier » et « Ouvrir » ; TUI : ligne de
    statut).
  - Pour PostgreSQL / MySQL, l'utilisateur reste requis (nom de l'utilisateur
    ou du groupe Entra) et le jeton est envoyé comme mot de passe ; MySQL exige
    le plugin `mysql_clear_password`, donc TLS obligatoire.
- **AWS IAM** : `aws rds generate-db-auth-token --hostname --port --username
  --region [--profile]`. Région déduite de l'hôte
  (`*.<region>.rds.amazonaws.com`), sinon obligatoire. Jeton valable 15 min.
  TLS obligatoire.
- **GCP IAM** : `gcloud sql generate-login-token` (repli : `gcloud auth
  print-access-token`). TLS obligatoire.
- Les CLI absents donnent « <outil> introuvable : installe-le ou ajoute-le au
  PATH » ; une session expirée donne la commande de connexion à lancer
  (`az login`, `aws sso login --profile …`, `gcloud auth login`).
- Aucun jeton ni secret n'est journalisé.

Les méthodes à jeton forcent au minimum `SslMode::Require` pour PostgreSQL et
MySQL (la valeur affichée est ajustée, avec une note).

## 4. Renouvellement des jetons

- PostgreSQL / MySQL : les options du pool sont mises à jour avec un jeton frais
  (`Pool::set_connect_options`) par une tâche qui se réveille à
  `expires_at - 5 min` (minimum 30 s) ; la tâche s'arrête avec la connexion.
  Une erreur de renouvellement est journalisée et réessayée ; la connexion
  existante continue tant que le serveur l'accepte.
- SQL Server : un jeton frais est obtenu à chaque (re)connexion (`open`).

## 5. Windows intégrée

- tiberius, feature `winauth`, uniquement `cfg(windows)` ;
  `AuthMethod::WindowsIntegrated` n'est proposé que sous Windows.
- Kerberos sur Linux / macOS et pour PostgreSQL : hors périmètre (sqlx ne gère
  pas GSSAPI ; `libgssapi` serait une dépendance système).

## 6. Interface

### GUI (dialogue de connexion)

Trois sections :
1. **Serveur** : nom, couleur, URL à coller, modèle, type, hôte (avec le cloud
   ou la région détectés en texte faible), port, base.
2. **Authentification** : liste des méthodes supportées ; champs selon la
   méthode (utilisateur, mot de passe + stockage, tenant, ID client, secret,
   profil / région AWS) ; texte d'aide d'une ligne par méthode.
3. **SSL** (PG / MySQL).

« Tester » affiche le diagnostic complet (étape en échec + action à faire).
Device code : fenêtre avec le code, l'URL, « Copier » et « Ouvrir ».
Connexion en mode `Prompt` : petite fenêtre « Mot de passe pour <connexion> ».

### TUI

Mêmes champs, avec saut des champs non pertinents ; méthode cyclée au clavier ;
invite de mot de passe masquée pour `Prompt` ; code device affiché dans le
statut et le dialogue.

### CLI

Pas de nouvelle commande ; les connexions enregistrées utilisent leur méthode.

## 7. Tests

- Unitaires :
  - déduction du cloud Entra (SQL / PG / MySQL, trois clouds, casse, hôtes
    piégés comme `x.database.windows.net.evil.com`) et de la région AWS ;
  - `AuthMethod::supported` (moteurs × systèmes) ;
  - migration : chaque ancien `azure_auth_method`, `tenant_id = common`,
    mot de passe en clair avec trousseau disponible / indisponible ;
  - `SecretStore` mémoire : création, renommage, suppression de connexion ;
  - construction des commandes CLI (arguments exacts) et analyse de leurs
    sorties / erreurs (CLI absent, session expirée, mauvais cloud) ;
  - calcul de l'échéance de renouvellement ;
  - GUI / TUI : champs visibles par méthode, aller-retour d'édition.
- Intégration (`#[ignore]`, Docker) : PostgreSQL et MySQL avec un
  `TokenProvider` de test qui renvoie un mot de passe changeant (le serveur
  change le mot de passe de l'utilisateur), pour vérifier que les nouvelles
  connexions du pool utilisent le jeton renouvelé.
- Non testables ici : Entra ID, AWS, GCP réels ; Windows intégrée contre un
  domaine.

## Hors périmètre

- Kerberos / GSSAPI (PostgreSQL, Linux, macOS).
- Wallets Oracle et méthodes propres à Oracle / ClickHouse (sous-projet B).
- Certificats client (mTLS).
