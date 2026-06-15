# Design — Exécution de transactions SQL

Date : 2026-06-15
Statut : approuvé (en attente de revue du spec)

## Objectif

Permettre à l'utilisateur de lancer une transaction SQL complète depuis l'éditeur
de requêtes. Quand le curseur est dans un bloc `BEGIN … COMMIT/ROLLBACK`,
`Ctrl+Enter` exécute **tout le bloc** atomiquement sur une seule connexion dédiée.

## Modèle retenu

- **Unité d'exécution = le bloc de transaction entier.** Le système détecte les
  bornes (`BEGIN` → premier `COMMIT`/`ROLLBACK`/`END` correspondant) et exécute
  tout le contenu en une fois.
- **Transaction éphémère.** La connexion n'est tenue que le temps de l'exécution
  du bloc, puis relâchée. Il n'y a pas de transaction qui reste ouverte entre deux
  exécutions → la notion de portée « par onglet vs globale » ne s'applique pas.
- **Déclencheur inchangé :** `Ctrl+Enter`. La détection du bloc est automatique ;
  hors transaction, le comportement actuel (une seule instruction au curseur)
  reste identique.
- **SGBD cibles :** Postgres, MySQL, SQLite, SQL Server, Azure.

## Approche

Exécuter le bloc `BEGIN…COMMIT` **verbatim** sur une connexion dédiée, instruction
par instruction. Si une instruction échoue, émettre `ROLLBACK` sur cette même
connexion et renvoyer l'erreur. Cette approche :

- respecte l'intention de l'utilisateur (un `ROLLBACK` écrit est exécuté tel quel) ;
- est uniforme sur les 5 SGBD (pas de dépendance à l'API transaction native de sqlx,
  inexistante pour tiberius) ;
- permet une attribution claire des erreurs (on sait quelle instruction a échoué).

## Composants

### 1. Détection du bloc (parsing) — `src/main.rs`

- Améliorer le découpage en instructions pour respecter les `;` situés dans des
  chaînes (`'…'`, `"…"`) et des commentaires (`-- …`, `/* … */`). Le splitter
  actuel (`get_query_at_cursor`) est naïf et coupe sur tout `;`.
- Classifier la première instruction au curseur. Si elle ouvre une transaction
  (`BEGIN`, `BEGIN TRANSACTION`, `BEGIN TRAN`, `START TRANSACTION`), remonter au
  `BEGIN` englobant et descendre jusqu'au premier terminateur correspondant
  (`COMMIT`, `COMMIT TRAN[SACTION]`, `ROLLBACK`, `ROLLBACK TRAN[SACTION]`,
  `END`/`END TRANSACTION` pour SQLite/Postgres).
- Le bloc résultant inclut le `BEGIN` et le terminateur.
- **Imbrication hors périmètre :** le premier terminateur ferme le bloc (pas de
  gestion de savepoints / `BEGIN` imbriqués).
- Nouvelle fonction renvoyant une unité d'exécution :
  - soit une instruction unique (comportement actuel),
  - soit un bloc de transaction (liste ordonnée d'instructions).

### 2. Exécution — `src/db/connector.rs` + backends

- Nouvelle méthode `DatabaseConnection::execute_transaction(&self, statements: &[String]) -> Result<QueryResult>`.
- **sqlx (Postgres / MySQL / SQLite)** : `pool.acquire()` pour obtenir une
  connexion dédiée (`PoolConnection`, propriétaire). Exécuter chaque instruction
  dans l'ordre sur cette connexion. En cas d'échec → exécuter `ROLLBACK` sur la
  même connexion, puis renvoyer l'erreur.
- **SQL Server / Azure (tiberius)** : `client.lock()` pour toute la durée du bloc
  (garantit l'atomicité et évite l'interleaving avec d'autres requêtes). Même
  logique d'exécution séquentielle et de `ROLLBACK` sur erreur.

### 3. Résultat affiché

- Afficher le dernier jeu de lignes renvoyé par le bloc (ex. un `SELECT` final).
- Si aucune instruction ne renvoie de lignes : statut
  « Transaction validée : N instructions, M lignes affectées ».
- En cas d'échec : statut « Transaction annulée : \<erreur\> ».

### 4. Cas du `BEGIN` sans terminateur

Si un bloc commence par `BEGIN` mais qu'aucun `COMMIT`/`ROLLBACK`/`END` n'est
trouvé dans l'éditeur : **ne pas exécuter** et afficher l'erreur
« Transaction non terminée : ajoute COMMIT ou ROLLBACK ». Comportement prévisible,
sans commit implicite.

## Gestion des erreurs

- Instruction échoue en cours de bloc → `ROLLBACK` automatique + remontée de l'erreur.
- Bloc sans terminateur → erreur, aucune exécution.
- Échec de l'acquisition d'une connexion (pool épuisé) → erreur explicite.

## Tests

- **Parser** (unitaires, sans DB) :
  - détection d'un bloc `BEGIN…COMMIT` simple ;
  - variantes de mots-clés multi-dialectes (`START TRANSACTION`, `BEGIN TRAN`,
    `END`, `ROLLBACK`) ;
  - `;` à l'intérieur de chaînes et de commentaires (ne doit pas couper) ;
  - curseur sur une instruction interne du bloc → le bloc entier est sélectionné ;
  - `BEGIN` sans terminateur → signalé comme non terminé ;
  - hors transaction → une seule instruction (non-régression).
- **Exécution** (SQLite en mémoire) :
  - transaction qui commit → données persistées ;
  - transaction dont une instruction échoue → `ROLLBACK`, données non persistées,
    erreur renvoyée.

## Hors périmètre

- Transactions persistantes/interactives restant ouvertes entre deux exécutions.
- Savepoints et transactions imbriquées.
- Indicateur d'état de transaction permanent dans l'UI (inutile en modèle éphémère).
