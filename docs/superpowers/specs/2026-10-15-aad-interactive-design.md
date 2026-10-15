# Azure AD interactif (navigateur) et relance manuelle après mise à jour

Date : 2026-10-15. Branche `feat/aad-interactive`, depuis master (v0.13.0).

## Partie 1 — Azure AD interactif pour Azure SQL

### Objectif

Permettre à chaque utilisateur de se connecter à Azure SQL avec **son propre
compte Entra ID** (MFA comprise) en passant par le navigateur, comme le mode
*Active Directory Interactive* de SSMS, sans installer le CLI `az`.

Aujourd'hui les méthodes Azure sont : identifiants SQL, « Azure AD » (jeton du
CLI `az`) et identité managée.

### Modèle

- Nouvelle variante `AzureAuthMethod::InteractiveBrowser`, affichée
  « Azure AD interactif (navigateur) ».
- La variante `Interactive` (jeton du CLI) garde son nom sérialisé mais
  s'affiche désormais « Azure CLI ».
- Champs de la méthode :
  - **Tenant** (`tenant_id`, obligatoire) : GUID ou domaine (`contoso.com`) ;
    `common` / `organizations` refusés (une application d'organisation est
    liée à son tenant).
  - **ID client de l'application** (nouveau champ `azure_client_id:
    Option<String>`, `#[serde(default)]`, obligatoire pour cette méthode,
    GUID) : l'application enregistrée par l'organisation. Pas de valeur par
    défaut.
  - **Compte** (`username`, facultatif) : `prenom.nom@contoso.com`, transmis
    comme `login_hint`.

### Flux (module `src/engine/db/entra_interactive.rs`)

Obtention d'un jeton pour `(cloud, tenant, client_id, compte, ressource)` :

1. **Cache mémoire** : jeton d'accès encore valide au moins 5 minutes → réutilisé.
2. **Jeton de renouvellement** (mémoire, sinon trousseau) → requête
   `grant_type=refresh_token` ; le nouveau jeton de renouvellement remplace
   l'ancien (mémoire + trousseau). Échec `invalid_grant` → secret effacé, on
   passe à l'étape 3.
3. **Navigateur** — OAuth 2.0 *authorization code* + PKCE (S256) :
   - écoute sur `127.0.0.1:<port libre>` ; `redirect_uri=http://localhost:<port>` ;
   - URL `https://<autorité du cloud>/<tenant>/oauth2/v2.0/authorize` avec
     `client_id`, `response_type=code`, `redirect_uri`,
     `scope=<ressource>/.default offline_access openid profile`,
     `code_challenge`, `code_challenge_method=S256`, `state` aléatoire,
     `prompt=select_account`, `login_hint` si un compte est saisi ;
   - ouverture du navigateur par défaut (crate `webbrowser`, déjà dans les
     dépendances) ; l'URL est aussi transmise à l'interface (lien à copier) ;
   - le retour `GET /?code=…&state=…` (ou `error=…&error_description=…`) reçoit
     une page HTML en français (« Connexion réussie, vous pouvez fermer cet
     onglet » / message d'erreur) ; un `state` différent est rejeté ;
   - échange du code (`grant_type=authorization_code` + `code_verifier`) ;
   - délai maximal 5 minutes ; annuler la connexion dans l'app arrête l'écoute.

Autorité et ressource viennent de `AzureCloud::from_host` (public, US
Government, Chine). Les requêtes HTTP passent par `ureq` (certificats du
système). Aucun jeton n'est journalisé.

Erreurs traduites (le message d'origine reste entre parenthèses) :

| Code / cas | Message |
|---|---|
| AADSTS65001, `consent_required` | L'application n'a pas l'autorisation d'accéder à Azure SQL : un administrateur doit accorder la permission déléguée « Azure SQL Database / user_impersonation ». |
| AADSTS50011 | L'URI de redirection `http://localhost` n'est pas déclarée : ajoute-la dans la plateforme « Applications mobiles et de bureau » de l'application. |
| AADSTS7000218 | Active « Autoriser les flux clients publics » dans l'application. |
| AADSTS700016 | Application introuvable dans ce tenant : vérifie l'ID client et le tenant. |
| AADSTS90002 | Tenant introuvable : vérifie le tenant. |
| `access_denied` (annulé dans le navigateur) | Connexion annulée dans le navigateur. |
| délai dépassé | Connexion non terminée dans le navigateur après 5 minutes. |
| navigateur non lancé | (pas d'erreur) l'interface affiche le lien à ouvrir. |
| login SQL refusé (erreur 18456) après jeton | Le compte est authentifié mais n'a pas accès à la base : `CREATE USER [compte] FROM EXTERNAL PROVIDER`. |

### Trousseau

- Crate `keyring` (v3) : Windows Credential Manager, macOS Keychain, Linux
  Secret Service en Rust pur + libdbus intégrée (pas d'OpenSSL ni de
  bibliothèque système requise ; `scripts/check-linked-libs.sh` reste vert).
- Service `storingUnicorns`, compte `entra:<tenant>:<client_id>:<compte ou
  "-">` ; valeur : le jeton de renouvellement.
- Trait `SecretStore` (get / set / delete), implémentation mémoire pour les
  tests.
- Trousseau indisponible : session seulement, avertissement une fois
  (« Trousseau du système indisponible : il faudra se reconnecter au prochain
  lancement. »).
- Supprimer la connexion, ou changer son tenant / ID client / compte, efface le
  secret correspondant. Action « Oublier la connexion Azure AD » dans le menu
  contextuel de la connexion (GUI).

### Interface

- **GUI** : la méthode s'ajoute à la liste Azure ; champs Tenant, ID client de
  l'application, Compte (facultatif), avec validation (GUID, tenant non vide et
  différent de `common` / `organizations`) et une ligne d'aide renvoyant à la
  documentation. Pendant l'attente du navigateur : bandeau « Terminez la
  connexion à <nom> dans votre navigateur… » avec « Copier le lien » et
  « Annuler ».
- **TUI** : mêmes champs (libellés courts) ; le lien s'affiche dans la ligne de
  statut pendant l'attente.
- Le moteur prévient l'interface via un gestionnaire d'interaction global
  (`set_interaction_handler`) qui reçoit `{ connexion, url }` au début et la fin
  de l'attente.

### Reconnexion

Le client SQL Server partagé se reconnecte via `azure::open` après une
interruption : il obtient alors le jeton par le cache ou le renouvellement,
sans rouvrir le navigateur tant que le jeton de renouvellement est valide.

### Documentation

README : enregistrement de l'application (plateforme « Applications mobiles et
de bureau », URI `http://localhost`, flux clients publics, permission déléguée
*Azure SQL Database / user_impersonation* + consentement administrateur), et
création de l'utilisateur côté base. CHANGELOG.

### Tests

- Unitaires : vecteur PKCE de la RFC 7636 ; URL d'autorisation (paramètres,
  encodage, autorité par cloud, `login_hint`) ; analyse du retour (code, `state`
  différent, erreur) ; réponse de jeton et expiration ; décision cache /
  renouvellement / navigateur ; traduction des AADSTS ; `SecretStore` mémoire
  (création, rotation, effacement) ; validation des champs GUI / TUI ;
  aller-retour de la configuration.
- Bout en bout : faux serveur d'autorisation local (autorité surchargeable en
  test) ; le test joue le navigateur en appelant le `redirect_uri`, puis vérifie
  l'échange du code, le stockage du jeton de renouvellement et un second appel
  silencieux par renouvellement.
- Non testable ici : un vrai tenant Entra ID.

### Hors périmètre

Azure Database for PostgreSQL / MySQL (refonte générale de l'authentification),
connexion par code d'appareil.

## Partie 2 — Relance manuelle après une mise à jour

Comme sur codingUnicorns :

- Bandeau « Version x prête » : boutons « Redémarrer maintenant » et
  « Plus tard ». « Plus tard » masque le bandeau ; la mise à jour s'applique à
  la fermeture (déjà le cas : binaire remplacé, ou MSI lancé à la sortie).
- Après « Plus tard », la version de la barre d'état signale la mise à jour
  prête (« v0.13.0 → 0.14.0 », info-bulle « Redémarrer pour passer à … ») ; un
  clic réaffiche le bandeau avec « Redémarrer maintenant ».
- « Redémarrer maintenant » passe par la fermeture normale : si la confirmation
  de fermeture (modifications ou transactions en attente) est annulée, la
  relance est retirée (une fermeture ultérieure n'applique que le MSI, sans
  relancer).
- Tests : état du bandeau après « Plus tard », réaffichage par la version,
  annulation de la fermeture qui retire la relance.
