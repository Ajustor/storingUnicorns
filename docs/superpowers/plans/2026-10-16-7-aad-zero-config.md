# Azure AD interactif sans configuration (comme DataGrip) — spec et plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** « Azure AD interactif (navigateur) » fonctionne sans tenant ni ID
d'application, comme DataGrip (driver JDBC Microsoft, `ActiveDirectoryInteractive`).

**Constat (source mssql-jdbc) :**
- ID client par défaut : `7f98cb04-cd1e-40df-9140-3bf7e2cea4db`
  (`ActiveDirectoryAuthentication.JDBC_FEDAUTH_CLIENT_ID`), redirection
  `http://localhost`.
- L'autorité (tenant) vient du serveur : le driver envoie un `LOGIN7` avec
  l'extension FEDAUTH en mode bibliothèque ADAL ; Azure SQL répond par un
  jeton TDS `FEDAUTHINFO` (0xEE) contenant `STSURL` (ID 0x01, ex.
  `https://login.windows.net/<tenant-guid>`) et `SPN` (ID 0x02, ex.
  `https://database.windows.net/`). MSAL utilise `authority = STSURL`.
- Constantes MS-TDS : feature ext `FEDAUTH = 0x02` ; bibliothèque
  `SECURITYTOKEN = 0x01`, `ADAL = 0x02` (octet d'options = `library << 1 |
  fFedAuthEcho`) ; workflow ADAL interactif = `0x03` (octet suivant) ; jeton
  `FEDAUTHINFO = 0xEE` : `TokenLength: u32`, `CountOfInfoIDs: u32`, puis
  `CountOfInfoIDs` × (`FedAuthInfoID: u8`, `FedAuthInfoDataLen: u32`,
  `FedAuthInfoDataOffset: u32` — offset depuis le début des données après
  `TokenLength`), données en UTF-16LE.
- tiberius ne gère que la bibliothèque SECURITYTOKEN (jeton connu d'avance) :
  la découverte se fait par une sonde TDS à nous, puis la vraie connexion reste
  tiberius + `AuthMethod::AADToken`.

**Base :** master v0.14.0 (`engine::db::entra_interactive`, `azure.rs`).
Branche : `feat/aad-zero-config`.

## Conventions du dépôt

- Build/tests sous Windows : `RUSTC_WRAPPER="C:/Users/alexa/scoop/apps/sccache/current/sccache.exe" cargo test -q` (outil Bash).
- Libellés en français ; commentaires et commits en anglais ; commits terminés
  par une ligne vide puis `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Relever tests et clippy (`cargo clippy -q --all-targets 2>&1 | grep -c "^warning"`) au départ ; ne pas ajouter d'avertissement.
- TDD, un commit par tâche. rustfmt seulement sur son propre code dans les
  fichiers qui ne sont pas déjà propres.

---

### Task 1 : sonde FEDAUTHINFO (`src/engine/db/fedauth_probe.rs`)

API :
```rust
pub struct FedAuthInfo { pub sts_url: String, pub spn: String }
/// Ask the server where its Entra ID users sign in (MS-TDS FEDAUTHINFO).
/// Ok(None): the server doesn't do Entra ID federated auth (on-prem SQL Server…).
pub async fn probe(host: &str, port: u16) -> anyhow::Result<Option<FedAuthInfo>>;
```

- [ ] **Parties pures, testées sur des octets construits d'après MS-TDS** :
  - en-tête de paquet TDS (type, status EOM, longueur big-endian, SPID, packet id, window) ;
  - PRELOGIN : VERSION, ENCRYPTION (`ENCRYPT_ON`), INSTOPT, THREADID,
    MARS, FEDAUTHREQUIRED (0x06, valeur 0x01) + terminateur 0xFF ; décodage
    de la réponse (chiffrement, FEDAUTHREQUIRED) ;
  - LOGIN7 minimal TDS 7.4 : longueurs/offsets, `HostName`, `AppName`
    (`storingUnicorns`), `ServerName` = host, base vide, aucun mot de passe,
    `OptionFlags3` avec `fExtension`, bloc FeatureExt : `0x02`, longueur `2`,
    options `(0x02 << 1)` (ADAL, sans echo), workflow `0x03`, terminateur `0xFF`
    (vérifier la disposition exacte dans tiberius `src/tds/codec/login.rs`, qui
    encode la variante SECURITYTOKEN, et dans mssql-jdbc
    `SQLServerConnection.writeFedAuthFeatureRequest`) ;
  - lecture des jetons de la réponse jusqu'à `FEDAUTHINFO` (0xEE) — ignorer
    proprement `ENVCHANGE` (0xE3), `INFO` (0xAB), `LOGINACK` (0xAD),
    `FEATUREEXTACK` (0xAE) ; `ERROR` (0xAA) → `Ok(None)` si aucun FEDAUTHINFO
    n'est venu avant ; `DONE` (0xFD) → fin.
  - Tests : un FEDAUTHINFO à deux entrées (STSURL + SPN) dans une réponse avec
    d'autres jetons avant ; entrées dans l'ordre inverse ; offset hors limites
    → erreur, pas de panique ; réponse sans FEDAUTHINFO → `None`.
- [ ] **Transport** : TCP (timeout de connexion 10 s, global 15 s), PRELOGIN en
  clair, puis poignée de main TLS **encapsulée dans des paquets PRELOGIN
  (type 0x12)** comme le fait tiberius (`TlsPreloginWrapper`), puis LOGIN7 et la
  suite directement dans le flux TLS. rustls avec les racines du système
  (comme le reste), nom de serveur = host, **vérification du certificat**.
  Fermer la connexion juste après `FEDAUTHINFO` (ne jamais envoyer de jeton).
- [ ] **Intégration réelle** : un test `#[ignore]` (variable `SU_IT_MSSQL`,
  conteneur SQL Server existant, voir `src/engine/ops/integration_tests.rs`)
  vérifie que la sonde fait la poignée de main TLS et le LOGIN7 et renvoie
  `Ok(None)` (SQL Server local : pas d'Entra ID). Le certificat du conteneur
  est auto-signé : prévoir une option interne de test pour ne pas vérifier le
  certificat, **jamais** utilisée hors tests. Docker : `export DOCKER_CONFIG=$(mktemp -d)` pour tirer les images ;
  conteneur `su-it-mssql` (port 41433, `sa` / `Su_it_Passw0rd!`) à relancer s'il est arrêté.
- [ ] Commit `feat(engine): ask Azure SQL where its Entra ID users sign in (FEDAUTHINFO probe)`.

### Task 2 : valeurs par défaut sans configuration

- [ ] `entra_interactive` :
  - `pub const DEFAULT_CLIENT_ID: &str = "7f98cb04-cd1e-40df-9140-3bf7e2cea4db";`
    (commentaire : ID public des drivers SQL Server de Microsoft, celui
    qu'utilisent le driver JDBC et donc DataGrip ; préautorisé pour Azure SQL).
  - Tenant et ID client deviennent **facultatifs** : `validate_interactive`
    accepte vides ; refuse un ID client non-GUID s'il est saisi ; un tenant
    saisi `common` reste refusé mais `organizations` est accepté.
  - Résolution de l'autorité, dans cet ordre : tenant saisi → sonde FEDAUTHINFO
    (`sts_url`, en extraire l'hôte et le tenant ; ne garder que des hôtes
    d'autorité connus : `login.windows.net`, `login.microsoftonline.com`,
    `login.microsoftonline.us`, `login.chinacloudapi.cn`, `login.partner.microsoftonline.cn`
    — sinon ignorer la réponse) → `organizations` sur l'autorité du cloud
    déduit de l'hôte. `login.windows.net` est réécrit en
    `login.microsoftonline.com`. Le résultat de la sonde est mis en cache en
    mémoire par `host:port` (y compris un échec, pour 10 minutes).
  - Le `scope` reste `AzureCloud::sql_scope()` (la SPN sert seulement de
    contrôle : si elle ne correspond pas à la ressource du cloud, utiliser la SPN
    + `.default`).
  - La clé de trousseau utilise le tenant **effectif** et l'ID client
    **effectif** : `entra:<tenant ou organizations>:<client>:<compte ou ->`.
    `same_sign_in` / `forget_unless_used` / `orphaned` raisonnent sur les
    valeurs effectives connues sans réseau : tenant saisi sinon `auto:<host>`.
    Documenter ce choix dans le code et le tester.
  - Reconnexion non interactive : même résolution (cache de la sonde).
  - Les connexions 0.14.0 (tenant + ID client saisis) gardent exactement leur
    comportement et leur clé.
- [ ] Tests : `validate_interactive` (vides acceptés) ; résolution de
  l'autorité (saisi > sonde > organizations) avec une sonde injectée ; hôte
  d'autorité inconnu ignoré ; clés de trousseau avant/après ; bout en bout avec
  le faux serveur d'autorisation et une sonde injectée qui renvoie
  `https://login.windows.net/<guid>` (le flux doit appeler `<authority>/<guid>/oauth2/v2.0/...`
  avec `client_id=7f98cb04-…`).
- [ ] Commit `feat(azure): Azure AD interactive works without a tenant or an app, like DataGrip`.

### Task 3 : interface

- [ ] GUI (`gui/dialogs/connection.rs`) : pour cette méthode, seul « Compte
  (facultatif) » est visible ; une section repliable « Options avancées »
  contient « Tenant (facultatif) » et « ID client de l'application
  (facultatif) », ouverte d'office si l'un des deux est rempli. Texte d'aide :
  « Le tenant est demandé au serveur ; l'application par défaut est celle des
  drivers Microsoft. » Tests sur la visibilité et la validation.
- [ ] TUI : champs Tenant et ID app facultatifs (libellés « Tenant (auto) »,
  « ID app (auto) »), même validation.
- [ ] README : la section « Azure AD interactif » devient « aucun réglage
  nécessaire » ; l'enregistrement d'une application devient une option
  avancée (organisations qui bloquent les applications tierces ou qui veulent
  la leur) ; garder `CREATE USER … FROM EXTERNAL PROVIDER`. CHANGELOG
  `## [Non publié]`.
- [ ] Commit `feat: Azure AD interactive asks only for the account`.
