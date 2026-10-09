# Journal des modifications

Les changements notables de storingUnicorns, version par version. Chaque section
est aussi affichée dans l'application lors d'une mise à jour et sur la
[page de téléchargement](https://ajustor.github.io/storingUnicorns/).

Le format suit [Keep a Changelog](https://keepachangelog.com/fr/1.1.0/) et les
numéros de version suivent le [versionnage sémantique](https://semver.org/lang/fr/).

## [Non publié]

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
