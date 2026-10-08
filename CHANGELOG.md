# Journal des modifications

Les changements notables de storingUnicorns, version par version. Chaque section
est aussi affichée dans l'application lors d'une mise à jour et sur la
[page de téléchargement](https://ajustor.github.io/storingUnicorns/).

Le format suit [Keep a Changelog](https://keepachangelog.com/fr/1.1.0/) et les
numéros de version suivent le [versionnage sémantique](https://semver.org/lang/fr/).

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
