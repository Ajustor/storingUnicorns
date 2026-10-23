# Journal des modifications

Les changements notables de storingUnicorns, version par version. Chaque section
est aussi affichée dans l'application lors d'une mise à jour et sur la
[page de téléchargement](https://ajustor.github.io/storingUnicorns/).

Le format suit [Keep a Changelog](https://keepachangelog.com/fr/1.1.0/) et les
numéros de version suivent le [versionnage sémantique](https://semver.org/lang/fr/).

## [0.18.3] - 2026-10-23

### Améliorations

- **Fenêtre « Mise à jour »** : un clic sur le numéro de version, en bas à
  gauche, ouvre une fenêtre où se déroule toute la mise à jour : recherche,
  « Vous utilisez la dernière version », ou la version disponible avec ses
  notes de version, **Installer**, **Ignorer cette version** et **Plus tard** ;
  puis le téléchargement (qui peut **Continuer en arrière-plan**) et
  **Redémarrer maintenant**. En cas d'échec, **Réessayer**. Quand une mise à
  jour est disponible ou installée, un bouton bien visible à côté de la version
  (« 0.18.2 disponible », « Redémarrer pour 0.18.2 ») rouvre cette fenêtre ;
  pendant le téléchargement, une roue de progression s'y affiche. Le bandeau
  du haut, affiché quand une mise à jour est trouvée au démarrage, propose
  **Voir la mise à jour** et ne double plus la fenêtre quand elle est ouverte.

## [0.18.1] - 2026-10-23

### Corrections

- **Scripts SQL Server / Azure** : un script (**Tout**, `F5` ou une sélection)
  s'exécute désormais comme dans SSMS, en lots T-SQL séparés par les lignes
  `GO` (`GO 5` répète le lot). Les variables déclarées dans le script sont
  visibles de toutes ses instructions, y compris un `SELECT` placé après le
  `COMMIT` (« Must declare the scalar variable » ne survient plus). Chaque jeu
  de résultats ouvre son propre onglet, un `SELECT` vide garde ses colonnes, et
  une erreur indique la ligne du script (`… (ligne 37 du script)`). Un lot
  ouvrant une transaction sans régler `XACT_ABORT` lui-même s'exécute avec
  `XACT_ABORT ON` (le réglage de la session est rétabli ensuite) et est annulé
  en cas d'erreur ; une transaction laissée ouverte à la fin du script est
  annulée et signalée. Le mode transaction manuelle suit les mêmes lots. Quand
  le script contient des branchements (`IF`, `TRY … CATCH`…), ses jeux de
  résultats s'affichent sous le texte du lot, en lecture seule.
- **Nombre de lignes affectées** : sous `SET NOCOUNT ON` (ou quand le serveur
  ne donne pas de compte), la console affiche « Exécuté » au lieu d'un
  trompeur « 0 lignes affectées ».

## [0.18.0] - 2026-10-23

### Nouveautés

- **Dossiers de connexions** : les connexions se rangent dans des dossiers
  imbriqués (`Clients/ACME/Prod`) : bouton **Nouveau dossier**, menu du dossier
  (**Nouvelle connexion ici**, **Nouveau sous-dossier…**, **Renommer…**,
  **Supprimer**, le contenu remontant d'un niveau), **Déplacer vers…**,
  glisser-déposer sur un dossier ou sur l'en-tête de l'explorateur (racine) et
  champ **Dossier** du formulaire. Dans l'interface terminal : `f`, `m`, `r` /
  `e`, `d`, `Entrée` / `Espace` et `←` / `→`.
- **Masquer des schémas** : **Masquer le schéma**, **Afficher les schémas
  masqués** et **Schémas affichés** dans le formulaire ; les schémas masqués
  sont exclus de `Ctrl+N` et de la complétion des tables (une table saisie à la
  main fonctionne toujours). Interface terminal : `h` et `H`.
- **Navigation par clés étrangères** : les colonnes FK (avec `→ table.colonne`)
  et les clés primaires sont marquées dans la grille, l'explorateur, la
  structure et la complétion. Dans la grille, cliquer sur le lien d'une cellule
  FK (ou `Alt+Entrée`) affiche la ligne liée dans le panneau de valeur, avec
  fil d'Ariane, **Retour**, ses propres clés étrangères et **Référencée par** ;
  `Ctrl+clic` ou **Ouvrir dans un onglet** ouvre la table liée filtrée. Le menu
  contextuel propose **Voir la ligne liée** et **Enregistrements liés**. La
  structure affiche **Lien**, **Clés étrangères** et **Référencée par**, avec
  des liens cliquables.

### Corrections

- MySQL : une valeur contenant un antislash (`\`) était mal écrite dans les
  requêtes générées (modifications de la grille, suppression, navigation) : elle
  pouvait faire échouer la requête ou, avec certaines valeurs, en modifier le
  sens ; c'est corrigé.
- SQL Server / Azure : les valeurs Unicode (accents hors page de code,
  idéogrammes, emoji) sont comparées correctement.
- PostgreSQL : une table partitionnée n'apparaît qu'une fois parmi les tables
  qui en référencent une autre.
- SQLite : les clés étrangères sans colonne cible explicite pointent vers la
  clé primaire.

## [0.17.0] - 2026-10-22

### Modifications

- **Fenêtres de dialogue homogènes** : toutes les fenêtres (connexion,
  structure, historique, recherche de table, export / import / vidage par lot,
  confirmations) ont la même présentation : le titre avec un bouton **×**, un
  contenu qui défile quand il est long, et les boutons toujours visibles en
  bas, **Annuler** à gauche, l'action principale à droite et en couleur, les
  actions destructives en rouge. Leur largeur dépend de leur rôle et ne dépasse
  jamais la fenêtre. `Entrée` déclenche l'action principale (jamais une action
  destructive, et pas dans un champ multiligne ni une liste déroulante
  ouverte, ni avec la touche `Entrée` qui vient d'ouvrir la fenêtre) ;
  `Échap`, **×** ou un clic hors de la fenêtre annulent, partout (sauf si une
  liste déroulante est ouverte : `Échap` la ferme d'abord). Les touches ne
  partent plus vers la console ou la grille restée derrière la fenêtre.
- Les erreurs, le test de connexion en cours et son résultat s'affichent juste
  au-dessus des boutons (un message long défile).
- Libellés : **Quitter quand même** (au lieu de « Fermer quand même ») pour
  quitter avec des modifications ou des transactions en attente ; pour terminer
  une transaction, **Commit** est l'action principale et **Rollback** est en
  rouge. Les boutons des fenêtres de l'historique (**Insérer**, **Copier**), de
  la recherche de table (**Ouvrir**), de la structure (**Appliquer** /
  **Supprimer**, **Fermer**) et du traitement par lot (**Exporter**,
  **Importer**, **Vider…**) sont dans la barre du bas ; ce qui manque pour
  lancer un traitement par lot est indiqué au-dessus du bouton.
- **Vider les tables** : la confirmation liste maintenant toutes les tables à
  vider (la liste défile ; elle s'arrêtait à 12).

### Corrections

- **Mise à jour sous Windows** : « Redémarrer maintenant » n'appliquait pas la
  mise à jour quand l'app avait été lancée par un double-clic sur l'exe (le
  relancement échouait avec « Descripteur non valide ») ; depuis un terminal il
  fonctionnait déjà.

## [0.16.0] - 2026-10-22

### Nouveautés

- **Complétion contextuelle** : dans les consoles, les barres `WHERE` /
  `ORDER BY` de la vue d'une table et l'éditeur de l'interface terminal, la
  complétion propose les colonnes des tables citées dans l'instruction sous le
  curseur. `alias.` donne les colonnes de cette table, les jointures préfixent
  les noms ambigus, et `INSERT INTO t (…)` et `UPDATE … SET` complètent les
  colonnes de `t`. Le type et la clé primaire sont affichés, les noms sont
  protégés selon le moteur et les colonnes se chargent en arrière-plan.
  `Ctrl+Espace` ouvre la liste, `↑` / `↓` choisissent, `Entrée` ou `Tab`
  valident, `Échap` ferme.
- **Historique des requêtes refait** (`Ctrl+Alt+E`) : un résumé par entrée avec
  la connexion, l'heure et la durée, un aperçu du SQL avec coloration, les
  boutons **Insérer** et **Copier**, et une recherche dans le SQL et le nom de la
  connexion.
- **IAM Google Cloud pour Cloud SQL** : connexion à une instance Cloud SQL
  PostgreSQL ou MySQL avec votre compte Google, sans mot de passe ni proxy.
  Choisissez **Authentification : IAM Google Cloud** dans le formulaire (à côté
  du port dans l'interface terminal) et indiquez votre **Compte Google** ;
  l'app obtient le jeton avec `gcloud` (après `gcloud auth login`), se connecte
  en TLS (au moins *Obligatoire*) et renouvelle le jeton avant son expiration.
  Le formulaire affiche le nom d'utilisateur que la base attend (l'adresse
  complète pour PostgreSQL, la partie avant `@` pour MySQL). Voir
  [IAM Google Cloud](https://github.com/Ajustor/storingUnicorns#iam-google-cloud-cloud-sql).

### Corrections

- **Historique** : un `history.json` enregistré avec un BOM était lu comme vide
  puis écrasé ; il est maintenant lu correctement.
- **Historique** : les doublons qui ne différaient que par les fins de ligne
  (CRLF / LF) sont fusionnés.
- Les caractères emoji et symboles ne s'affichent plus comme des cases.
- Les listes déroulantes et la recherche de la vue d'une table sont assez larges
  pour leur contenu.

## [0.15.1] - 2026-10-16

### Corrections

- **Azure AD interactif** : une note de tenant abîmée dans le trousseau du
  système est ignorée au lieu d'être mal lue (un tenant `../x` était lu comme
  `x`), et son adresse de connexion doit être exactement celle d'un service
  Microsoft connu.
- Une connexion Azure AD sans tenant connu qui est refusée efface aussi la note
  de son tenant.
- Les connexions sans tenant connu des clouds Azure Government et Azure Chine
  sont gardées à part de celles du cloud public.

## [0.15.0] - 2026-10-16

### Nouveautés

- **Azure AD interactif sans configuration, comme DataGrip** : plus besoin de
  tenant ni d'application. Comme les drivers SQL Server de Microsoft, l'app
  demande au serveur Azure SQL le tenant de ses utilisateurs Entra ID et se
  connecte avec l'application de ces drivers, déjà autorisée pour Azure SQL :
  votre organisation n'a plus rien à enregistrer. Seul le compte reste à
  indiquer, et il est facultatif. Voir
  [Azure AD interactif](https://github.com/Ajustor/storingUnicorns#azure-ad-interactif).

### Modifications

- Le tenant et l'ID client de l'application d'une connexion Azure AD
  interactive deviennent facultatifs et passent dans les **Options avancées**
  du formulaire (repliées, sauf s'ils sont renseignés ; `auto` dans l'interface
  terminal), pour une organisation qui bloque l'application des drivers
  Microsoft ou préfère la sienne. Le tenant `organizations` est accepté. Les
  connexions existantes gardent leur tenant et leur application.
- Les messages d'erreur de la connexion Azure AD tiennent compte de
  l'application utilisée : avec celle des drivers Microsoft, un consentement
  refusé invite à demander l'accord d'un administrateur ou à indiquer
  l'application de votre organisation.

## [0.14.0] - 2026-10-15

### Nouveautés

- **Azure SQL : connexion avec votre propre compte Entra ID, par le
  navigateur**, comme le mode *Active Directory Interactive* de SSMS, MFA
  comprise et sans installer Azure CLI. Choisissez l'authentification
  **Azure AD interactif (navigateur)**, puis saisissez le tenant de votre
  organisation (GUID ou domaine), l'ID client de l'application qu'elle a
  enregistrée dans Entra ID et, si vous le souhaitez, votre compte. La page de
  connexion s'ouvre dans le navigateur ; un bandeau permet d'en copier le lien
  (si le navigateur ne s'est pas ouvert) ou d'annuler. Les connexions
  suivantes, même après un redémarrage, se font sans navigateur tant que le
  jeton de renouvellement, gardé dans le trousseau du système, reste valable.
  « Oublier la connexion Azure AD » dans le menu de la connexion l'efface. Voir
  [Azure AD interactif](https://github.com/Ajustor/storingUnicorns#azure-ad-interactif)
  pour enregistrer l'application et créer l'utilisateur dans la base. Dans
  l'interface terminal, le lien s'affiche dans la barre d'état et `Échap`
  annule.

- Les reconnexions automatiques d'une connexion Azure AD interactive (après
  une requête interrompue, ou pour la session d'une transaction manuelle)
  n'ouvrent jamais le navigateur : quand le jeton ne peut plus être renouvelé,
  elles échouent avec « Session Azure AD expirée » jusqu'à ce que vous vous
  reconnectiez.

- **Redémarrer plus tard après une mise à jour.** Le bandeau « Version x
  prête » propose **Redémarrer maintenant** et **Plus tard** : la mise à jour
  s'applique alors à la fermeture. En attendant, la version de la barre d'état
  affiche `v0.13.0 → x` ; un clic réaffiche le bandeau. Annuler la fermeture
  (modifications ou transaction en attente) annule aussi le redémarrage.

### Modifications

- L'authentification Azure « Azure AD », qui utilise le jeton d'Azure CLI,
  s'appelle désormais **Azure CLI**. Les connexions existantes ne changent pas.
- Interface terminal : une connexion s'ouvre en arrière-plan, l'interface reste
  utilisable pendant ce temps et `Échap` l'annule.

## [0.13.0] - 2026-10-14

### Nouveautés

- **Mode de transaction manuel, comme dans DataGrip.** Le sélecteur
  **Tx : Auto / Manuel** de la barre d'outils des consoles et des onglets de
  données s'applique à toute la connexion. En **Auto**, chaque instruction est
  validée tout de suite, comme avant. En **Manuel**, requêtes et Submit
  s'accumulent dans une transaction ouverte sur une session dédiée : les
  grilles montrent déjà vos changements, et vous les validez avec **Commit**
  ou les annulez avec **Rollback**. Le nombre d'instructions en attente reste
  affiché. Un Submit en échec n'annule que lui-même, le reste de la
  transaction est conservé.

- Repasser en Auto avec une transaction en cours demande s'il faut la valider
  ou l'annuler, et quitter l'application prévient qu'une transaction est encore
  ouverte. Se déconnecter annule la transaction en cours.

### Limites connues

- MySQL : une instruction DDL (`CREATE`, `ALTER`…) valide implicitement ce qui
  la précède dans la transaction.
- SQLite : un import ou un vidage lancé depuis l'explorateur pendant une
  transaction manuelle avec écritures peut échouer avec « database is locked ».
- Pas encore disponible dans l'interface terminal.

## [0.12.0] - 2026-10-13

### Nouveautés

- **macOS : installation en une ligne dans le Terminal**, désormais la méthode
  recommandée sur Mac :
  `curl -fsSL https://ajustor.github.io/storingUnicorns/install-macos.sh | bash`.
  Le script télécharge la dernière version, vérifie son SHA-256, installe
  **storingUnicorns.app** dans `~/Apps` (où Spotlight la trouve) et ajoute la
  commande `storingUnicorns` au `PATH`. Installée ainsi, l'app n'affiche plus
  d'avertissement « app dangereuse » et n'est pas bloquée par macOS 26. Relancez
  la commande pour réinstaller ; l'app continue ensuite de se mettre à jour
  toute seule.

- **Version en cours dans la barre d'état**, tout à gauche : `v0.x.y` dans
  l'interface graphique (un clic vérifie les mises à jour, comme avant) et dans
  l'interface terminal.

### Modifications

- La page de téléchargement présente d'abord l'installation macOS par le
  Terminal. Avec le `.dmg`, glissez l'app ailleurs que dans un dossier
  « Applications » : macOS 26 y bloque les apps non notarisées.

## [0.11.0] - 2026-10-09

### Nouveautés

- **macOS : une vraie application.** `storingUnicorns-macos-arm64.dmg` contient
  **storingUnicorns.app**, à glisser dans Applications. Lancée depuis le Finder
  ou le Dock, elle reprend le `PATH` de votre shell de connexion : Azure CLI
  (`az`) est trouvé pour l'authentification Azure AD.
- **Linux : une AppImage.** `storingUnicorns-linux-x64.AppImage`, avec son icône
  et son entrée de menu, se met à jour sur place.
- **Commande `storingUnicorns` dans le `PATH`.** L'app macOS et l'AppImage
  créent au lancement un lien dans `/usr/local/bin` ou `~/.local/bin` (ajouté si
  besoin au fichier de démarrage du shell) : `storingUnicorns tui` marche depuis
  un nouveau terminal.
- **Azure SQL : identité managée affectée par l'utilisateur.** Le champ
  « ID client de l'identité » choisit l'identité à utiliser (vide = identité
  système). Sur App Service, Functions et Container Apps, seule l'identité
  système est prise en charge pour l'instant.
- **Azure SQL : cloud déduit de l'hôte.** Les serveurs Azure Government
  (`*.database.usgovcloudapi.net`) et Azure Chine (`*.database.chinacloudapi.cn`)
  obtiennent un jeton pour le bon cloud ; le cloud détecté s'affiche à côté de
  l'hôte. Un nom personnalisé (alias DNS) est traité comme le cloud public. Si
  Azure CLI est réglé sur un autre cloud, le message d'erreur indique la
  commande `az cloud set --name …` à lancer.

### Corrections

- **Azure SQL : l'authentification « Managed Identity » utilisait en réalité
  Azure CLI.** Elle interroge désormais le point de terminaison d'identité
  managée de la ressource Azure, avec un message clair quand la machine n'en a
  pas.
- **Azure SQL : le certificat du serveur est vérifié.** Les connexions Azure
  (jeton Entra ID comme authentification SQL) acceptaient n'importe quel
  certificat alors qu'un jeton ou un mot de passe est envoyé au serveur. Les
  connexions SQL Server ne changent pas.

## [0.10.0] - 2026-10-09

### Nouveautés

- **Connexions chiffrées (SSL/TLS)** pour PostgreSQL et MySQL : mode SSL
  (désactivé, préféré, obligatoire, vérification de l'autorité ou complète) et
  certificat CA optionnel. Les certificats du système sont reconnus. Le mode
  « vérification de l'autorité » contrôle encore le nom du serveur (limite de
  la bibliothèque sqlx) : le mode obligatoire chiffre sans vérifier.
- **Modèles de connexion** : MariaDB, PlanetScale, CockroachDB, TimescaleDB,
  Supabase, Neon et Redshift, avec le bon port et le bon mode SSL ; le produit
  s'affiche dans l'explorateur.
- **Coller une URL** (`postgresql://…?sslmode=require`) remplit le formulaire de
  connexion.

### Corrections

- Les mots de passe contenant `@`, `:`, `/`, `#`, `?` ou `%` fonctionnent. Un
  mot de passe saisi encodé pour contourner le problème (par exemple `p%40ss`
  pour `p@ss`) doit désormais être saisi tel quel.
- Les index et clés étrangères absents d'un serveur (CockroachDB, Redshift)
  n'empêchent plus d'afficher la table.

## [0.9.1] - 2026-10-09

Première version publiée de la 0.9 : la publication de la 0.9.0 avait échoué
sous Linux et macOS.

### Corrections

- **Mise à jour automatique derrière un proxy d'entreprise ou un antivirus** :
  elle fait confiance aux certificats du système (trousseau macOS, magasin
  Windows, certificats du système sous Linux) au lieu des seuls certificats
  intégrés au binaire.
- **Linux** : OpenSSL est intégré au binaire, qui ne dépend plus de la version
  installée sur la distribution.
- Les tests passent sous Linux et macOS (un test de l'installeur Windows
  utilisait des chemins Windows).

### Interne

- Les tests et le build tournent sur Linux, macOS et Windows à chaque pull
  request, avec une vérification que le binaire ne dépend d'aucune bibliothèque
  absente d'un système nu (Homebrew sous macOS, OpenSSL sous Linux).

## [0.9.0] - 2026-10-08

### Nouveautés

- **Interface graphique** inspirée de DataGrip, ouverte par défaut.
- **Explorateur** : plusieurs connexions ouvertes en même temps, chacune avec sa
  couleur ; schémas, tables, colonnes, clés et index.
- **Consoles SQL** : un onglet de résultat par instruction (épinglable), onglet
  « Sortie » avec le journal d'exécution, historique des requêtes (`Ctrl+Alt+E`),
  formatage (`Ctrl+Alt+L`), autocomplétion ; affichage limité à 1000 lignes.
  `Échap` annule une requête en cours sans figer l'interface.
- **Éditeur de données** : pagination, filtre `WHERE` et tri `ORDER BY`, tri par
  clic sur l'en-tête, édition directe des cellules, ajout et suppression de lignes ;
  `Submit` applique tout en une seule transaction, `Revert` annule.
- Panneau de valeur (JSON mis en forme), onglet DDL, recherche de table (`Ctrl+N`).
- Fenêtres de structure de table, d'import, d'export et de vidage, avec progression
  ligne par ligne pendant les imports.
- **Mises à jour automatiques** : l'application propose les nouvelles versions et
  s'installe toute seule ; `storingUnicorns update` fait de même en ligne de commande.
- Installation en une ligne (`curl … | sh` ou `irm … | iex`) et installateur Windows `.msi`.
- Le TUI reste disponible : `storingUnicorns tui`.

### Changements

- `storingUnicorns` sans argument ouvre désormais l'interface graphique.
  `--debug` et `--no-animations` seuls continuent de lancer le TUI.

### Corrections

- Le nombre de lignes modifiées est désormais exact : l'import CSV ne duplique
  plus les lignes déjà présentes.
- MySQL : transactions et métadonnées (clés, index) fiables.
- SQL Server : les transactions sont atomiques.
- PostgreSQL et SQL Server : décodage correct de types auparavant mal affichés.
- `NULL` est affiché comme tel, quel que soit le type de la colonne.
- Le mode debug du TUI n'exécute plus les `UPDATE` et `INSERT`.
