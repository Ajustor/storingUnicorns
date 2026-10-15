# Azure AD interactif + relance après mise à jour — plan d'implémentation

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Connexion Azure SQL avec le compte Entra ID de l'utilisateur via le
navigateur (auth code + PKCE, jeton de renouvellement au trousseau), et relance
manuelle après une mise à jour.

**Architecture:** Un module moteur `engine::db::entra_interactive` (parties
pures testables + flux réseau injectable), un module `engine::secrets`
(trousseau derrière un trait), branchés dans `azure::open`. Les UIs reçoivent le
lien de connexion par un gestionnaire d'interaction global. La relance après
mise à jour ne touche que `updater::background` et `gui`.

**Tech Stack:** Rust, tokio, ureq 3, sha2, base64 0.22, getrandom, webbrowser
(déjà dans l'arbre via eframe), keyring 3, tiberius (AADToken), egui, ratatui.

**Spec:** `docs/superpowers/specs/2026-10-15-aad-interactive-design.md`.

## Conventions du dépôt

- Build/tests sous Windows : `RUSTC_WRAPPER="C:/Users/alexa/scoop/apps/sccache/current/sccache.exe" cargo test -q`
  (sans cette variable : « sccache not found »). Outil Bash (Git Bash).
- Libellés et messages en français ; commentaires et commits en anglais ;
  commits terminés par une ligne vide puis
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Ne formater (rustfmt) que son propre code dans les fichiers qui ne sont pas
  déjà propres.
- Relever avant de commencer le nombre de tests et d'avertissements clippy
  (`cargo clippy -q --all-targets 2>&1 | grep -c "^warning"`) ; ne pas en ajouter.
- TDD : test qui échoue, implémentation, test qui passe, commit.
- Les parties réseau sont injectables (autorité, ouverture du navigateur,
  stockage) pour être testées sans Azure.

## Fichiers

| Fichier | Rôle |
|---|---|
| `src/engine/models/connection.rs` | `AzureAuthMethod::InteractiveBrowser`, libellés, `azure_client_id` |
| `src/engine/secrets.rs` (nouveau) | `SecretStore`, `KeyringStore`, `MemoryStore`, store par défaut |
| `src/engine/db/entra_interactive.rs` (nouveau) | PKCE, URL, retour, jetons, cache, flux, erreurs |
| `src/engine/db/azure.rs` | branchement de la méthode, astuce 18456 |
| `src/gui/dialogs/connection.rs`, `src/gui/status.rs`, `src/gui/app/*`, `src/gui/explorer/mod.rs` | champs, bandeau d'attente, « Oublier… », relance |
| `src/tui/app_state.rs`, `src/tui/mod.rs`, `src/tui/ui/modals/new_connection.rs`, `src/tui/ui/widgets.rs` | champs, lien dans le statut |
| `src/updater/background.rs` | « Plus tard », relance retirée à l'annulation |
| `README.md`, `CHANGELOG.md` | documentation |

---

### Task 1 : modèle

- [ ] Tests (dans `connection.rs` et/ou `gui/dialogs/connection.rs` selon où
  vit la validation) :
  - `InteractiveBrowser` sérialise en `"InteractiveBrowser"` ; une config sans
    `azure_client_id` se charge (`None`) ; aller-retour TOML avec la nouvelle
    méthode et l'ID client.
  - `AzureAuthMethod::Interactive.to_string() == "Azure CLI"`,
    `InteractiveBrowser.to_string() == "Azure AD interactif (navigateur)"`.
  - `validate_interactive(tenant, client_id)` (fonction pure, dans
    `entra_interactive.rs` ou `azure.rs`) : tenant vide → « Le tenant est
    obligatoire » ; `common` / `organizations` (insensible à la casse) →
    « Indique le tenant de ton organisation (GUID ou domaine), pas
    « common » » ; client ID vide ou non-GUID → « L'ID client de l'application
    doit être un GUID ». Réutiliser `azure::is_guid` (le rendre `pub(crate)`).
- [ ] Implémentation ; commit `feat(engine): Azure AD interactive auth method and app client ID`.

### Task 2 : secrets

- [ ] `Cargo.toml` :
  ```toml
  # OS credential store for Entra ID refresh tokens. Linux: Secret Service in
  # pure Rust with a vendored libdbus (no OpenSSL, no system library needed).
  keyring = { version = "3", features = ["apple-native", "windows-native", "sync-secret-service", "crypto-rust", "vendored"] }
  ```
  Vérifier dans la doc du crate (registry) les noms exacts des features de la
  version résolue et ajuster.
- [ ] `src/engine/secrets.rs` :
  ```rust
  pub trait SecretStore: Send + Sync {
      fn get(&self, key: &str) -> Result<Option<String>, String>;
      fn set(&self, key: &str, value: &str) -> Result<(), String>;
      fn delete(&self, key: &str) -> Result<(), String>;
  }
  pub struct KeyringStore;   // service "storingUnicorns"
  #[derive(Default)] pub struct MemoryStore(Mutex<HashMap<String, String>>);
  /// The process-wide store: the OS keyring, or memory when it isn't usable
  /// (`available()` false, warned once).
  pub fn store() -> &'static dyn SecretStore;
  pub fn available() -> bool;
  ```
  `get` d'une entrée absente → `Ok(None)` (mapper `keyring::Error::NoEntry`).
  Détection : au premier appel, un `set`/`delete` d'une clé sonde
  `storingUnicorns:probe` ; en cas d'erreur, bascule mémoire.
  Les tests unitaires n'utilisent que `MemoryStore` (jamais le vrai trousseau).
- [ ] Vérifier sous Linux (Docker `rust:1-bookworm`, voir comment la CI installe
  ses dépendances dans `.github/workflows/ci.yml`) que `cargo build` passe et que
  `bash scripts/check-linked-libs.sh target/debug/storingUnicorns` est vert. Si
  `vendored` n'est pas disponible ou casse le build, le signaler (BLOCKED) plutôt
  que de lier libdbus dynamiquement.
- [ ] Commit `feat(engine): OS credential store behind a SecretStore trait`.

### Task 3 : parties pures d'`entra_interactive`

Tout est testé sans réseau.

- [ ] PKCE : `pub fn pkce_challenge(verifier: &str) -> String` (SHA-256,
  base64url sans padding) ; test RFC 7636 : verifier
  `dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk` → challenge
  `E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGF8cOM2cM`.
  `fn random_urlsafe(bytes: usize) -> String` (getrandom + base64url).
- [ ] `pub struct Target { pub authority: String /* https://login.microsoftonline.com */, pub tenant: String, pub client_id: String, pub scope: String /* "<resource>/.default offline_access openid profile" */, pub login_hint: Option<String> }`
  avec `Target::for_config(&ConnectionConfig) -> Result<Target, String>`
  (cloud par `AzureCloud::from_host`, validation de la Task 1).
- [ ] `pub fn authorize_url(t: &Target, redirect_uri: &str, challenge: &str, state: &str) -> String`
  (`url::Url` + `query_pairs_mut`) ; tests : chaque paramètre présent et
  encodé, `login_hint` seulement s'il est saisi, autorité US Gov / Chine.
- [ ] `pub fn parse_callback(request_target: &str, expected_state: &str) -> Result<String /* code */, AuthError>`
  sur la ligne `GET /?…` ; tests : code OK, `state` différent → erreur,
  `error=access_denied&error_description=…` → `AuthError::Denied`, autre
  erreur → `AuthError::Server { code, description }`, requête sans code
  (ex. `/favicon.ico`) → `AuthError::NotACallback` (ignorée par la boucle).
- [ ] `pub struct Tokens { pub access: String, pub refresh: Option<String>, pub expires_at: SystemTime }`
  et `pub fn parse_token_response(status: u16, body: &str, now: SystemTime) -> Result<Tokens, AuthError>`
  (`expires_in` → `expires_at`, erreur JSON `error` / `error_description` /
  `error_codes`).
- [ ] `pub fn explain(err: &AuthError) -> String` : table de la spec (AADSTS
  repérés dans `error_description` ou `error_codes`), message d'origine entre
  parenthèses.
- [ ] Clé de cache / trousseau : `pub fn secret_key(t: &Target) -> String` =
  `entra:<tenant en minuscules>:<client_id en minuscules>:<login_hint en minuscules ou "-">`.
- [ ] Décision : `pub enum Next { UseCached, Refresh(String), Interactive }` et
  `pub fn next_step(cached: Option<&Tokens>, refresh: Option<&str>, now) -> Next`
  (jeton valide ≥ 5 min → `UseCached`).
- [ ] Commit `feat(engine): Entra ID interactive sign-in building blocks`.

### Task 4 : flux

- [ ] Gestionnaire d'interaction :
  ```rust
  pub enum Interaction { Waiting { connection: String, url: String }, Done { connection: String } }
  pub fn set_interaction_handler(f: impl Fn(Interaction) + Send + Sync + 'static);
  ```
  (un `OnceLock`/`RwLock` global ; sans gestionnaire, rien n'est émis.)
- [ ] `pub async fn token(config: &ConnectionConfig) -> anyhow::Result<String>` :
  cache mémoire global (`Mutex<HashMap<key, Tokens>>`), `next_step`, puis :
  - `Refresh` → POST `<authority>/<tenant>/oauth2/v2.0/token`
    (`grant_type=refresh_token`, `client_id`, `refresh_token`, `scope`) via ureq
    dans `spawn_blocking` ; nouveau refresh → mémoire + `secrets::store()` ;
    `invalid_grant` → effacer le secret et passer en `Interactive`.
  - `Interactive` → `tokio::net::TcpListener::bind("127.0.0.1:0")`,
    `redirect_uri = http://localhost:<port>`, URL, `Interaction::Waiting`,
    `webbrowser::open` (échec ignoré : l'UI montre le lien), boucle `accept`
    jusqu'à un vrai retour (ignorer `NotACallback`, répondre 404), réponse HTML
    UTF-8 en français, puis `Interaction::Done` (toujours, y compris erreur /
    annulation : garde `Drop`), échange du code
    (`grant_type=authorization_code`, `code`, `redirect_uri`, `code_verifier`,
    `client_id`, `scope`). `tokio::time::timeout(5 min)`.
  - Une seule connexion interactive à la fois par clé (un `tokio::sync::Mutex`
    par clé) pour éviter deux onglets si deux reconnexions arrivent ensemble.
- [ ] Injection pour les tests : une variante interne
  `token_with(config, deps: &Deps)` où `Deps { authority_override: Option<String>, open_browser: fn/closure, store: &dyn SecretStore, now: fn() -> SystemTime }`.
- [ ] Test de bout en bout (tokio) : un faux serveur d'autorisation HTTP local
  (`tokio::net::TcpListener`, réponses JSON écrites à la main) sert
  `/<tenant>/oauth2/v2.0/token` ; `open_browser` simule le navigateur en lisant
  `redirect_uri` et `state` dans l'URL et en faisant `GET <redirect_uri>/?code=abc&state=<state>`
  (ureq) ; vérifier : l'échange envoie `code_verifier` correspondant au
  `code_challenge`, le jeton est retourné, le refresh est dans le `MemoryStore` ;
  second appel après expiration simulée → renouvellement sans navigateur ;
  `invalid_grant` → retour au navigateur ; `state` incorrect → erreur ; timeout
  (avec un délai de test court, rendre le délai injectable) → message.
- [ ] Commit `feat(engine): Entra ID interactive sign-in with silent refresh`.

### Task 5 : branchement Azure

- [ ] `azure::open` : `AzureAuthMethod::InteractiveBrowser` →
  `entra_interactive::token(config)` puis `connect_with_aad_token`.
  Erreur de login SQL 18456 avec un jeton → ajouter l'astuce
  « `CREATE USER [compte] FROM EXTERNAL PROVIDER` » (fonction pure testée sur le
  texte d'erreur tiberius).
- [ ] `pub fn forget(config: &ConnectionConfig)` dans `entra_interactive`
  (cache mémoire + secret). Appelée quand une connexion Azure interactive est
  supprimée, ou modifiée avec un tenant / ID client / compte différent
  (GUI `delete_connection` / sauvegarde du formulaire, TUI équivalent). Test :
  fonction pure `needs_forget(old, new) -> bool`.
- [ ] Commit `feat(azure): connect with the user's own Entra ID account`.

### Task 6 : GUI

- [ ] Dialogue (`gui/dialogs/connection.rs`) : `InteractiveBrowser` dans
  `AZURE_METHODS` ; champs « Tenant (ID ou domaine) », « ID client de
  l'application », « Compte (facultatif) » ; validation Task 1 dans `validate` ;
  `same_target` inclut `azure_client_id` ; ligne d'aide faible
  « Application à enregistrer dans Entra ID : voir le README ». Tests : champs
  visibles par méthode, validation, aller-retour d'édition.
- [ ] Bandeau d'attente : au démarrage de l'app, `set_interaction_handler`
  envoie l'interaction au thread UI (canal + `request_repaint`) ; `App` garde
  `waiting_sign_in: Option<(String, String)>` ; bandeau en haut
  « Terminez la connexion à <nom> dans votre navigateur… » + « Copier le lien »
  (`ctx.copy_text`) + « Annuler » (annule la connexion en cours comme le fait
  déjà l'annulation existante de `connecting`, à trouver dans `sessions` /
  `worker`). Test sur l'état (`Waiting` → bandeau, `Done` → plus de bandeau).
- [ ] Menu contextuel de la connexion dans l'explorateur : « Oublier la
  connexion Azure AD » si la méthode est `InteractiveBrowser` → `forget`.
- [ ] Vérification visuelle si la session est déverrouillée (config jetable via
  `STORINGUNICORNS_CONFIG_DIR`) : dialogue avec la nouvelle méthode. Capture
  dans le scratchpad.
- [ ] Commit `feat(gui): Azure AD interactive sign-in`.

### Task 7 : TUI

- [ ] Champs : nouveau `ConnectionField::ClientId` (après `TenantId`), visible
  seulement pour `InteractiveBrowser` ; `TenantId` visible pour `Interactive`
  et `InteractiveBrowser` ; `Username` (libellé « Compte ») visible pour
  `InteractiveBrowser`, `Password` masqué. `to_config` / `from_config` /
  `check()` (validation Task 1). Tests sur la navigation et l'aller-retour.
- [ ] Gestionnaire d'interaction : le lien s'affiche dans la ligne de statut
  (« Connexion Azure AD : ouvre <url> ») tant que `Waiting`.
- [ ] Commit `feat(tui): Azure AD interactive sign-in`.

### Task 8 : relance manuelle après mise à jour

- [ ] `updater/background.rs` :
  - `pub fn later(&mut self)` : état `Ready` → `dismissed = true` et
    `schedule_on_quit()`.
  - `pub fn show_ready(&mut self)` : remet `dismissed = false` si `Ready`.
  - `pub fn cancel_restart(&mut self)` : `Relaunch` → `None`,
    `RunMsiThenRelaunch(m)` → `RunMsi(m)`, sinon inchangé.
  - Tests unitaires pour chacune.
- [ ] `gui/status.rs` : bandeau `Ready` affiché seulement si `!dismissed`, avec
  « Redémarrer maintenant » et « Plus tard » ; la version de la barre d'état,
  quand `Ready`, s'affiche « v<actuelle> → <nouvelle> » en couleur d'accent,
  info-bulle « Mise à jour prête : cliquer pour redémarrer » ; clic →
  `show_ready()` (au lieu de relancer une vérification).
- [ ] Fermeture (`gui/app/mod.rs`, dialogue `ConfirmQuit`) : si l'utilisateur
  annule la confirmation, appeler `updater.cancel_restart()`.
- [ ] TUI : rien (il n'a pas de bandeau de mise à jour).
- [ ] Commit `feat(updater): restart later or manually once an update is ready`.

### Task 9 : documentation

- [ ] README : section « Azure AD interactif » : enregistrement de
  l'application (étapes de la spec), saisie dans storingUnicorns, création de
  l'utilisateur dans la base, où sont gardés les jetons et comment les oublier.
- [ ] CHANGELOG : section `## [Non publié]` (Nouveautés : Azure AD interactif,
  relance après mise à jour ; Modifications : « Azure AD » renommé
  « Azure CLI »).
- [ ] `cargo test -q`, clippy ; commit `docs: Azure AD interactive sign-in and restart after update`.
