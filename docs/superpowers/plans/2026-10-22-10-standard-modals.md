# Modales standard — spec et plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal :** toutes les modales de la GUI ont la même structure, la même taille
selon leur rôle, les mêmes boutons et les mêmes touches, pour ne plus avoir de
problèmes de largeur, de boutons hors de vue ou de comportement incohérent.

**Aujourd'hui :** `gui/dialogs/mod.rs` ouvre un seul `egui::Modal` (largeur
mini 420) et chaque dialogue dessine son titre (`ui.heading`), ses boutons et
gère Entrée / Échap à sa façon. Dialogues (`enum Dialog`) : `Connection`,
`ConfirmDeleteConnection`, `History`, `TableSearch`, `DiscardEdits`,
`CloseTab`, `ConfirmRerun`, `ConfirmQuit`, `EndTx`, `Structure`,
`ConfirmTruncate`, `Batch`. Utilitaires de largeur : `gui/popup.rs`.

Branche : `feat/standard-modals` depuis master (0.16.0).

## Le standard (contrat `src/gui/modal.rs`)

```rust
pub enum ModalSize { Small /* 440 */, Medium /* 640 */, Large /* popup::modal_width */ }
pub enum ButtonKind { Primary, Secondary, Danger }
pub struct ModalButton<T> { pub label: String, pub kind: ButtonKind, pub action: T,
                            pub enabled: bool, pub hint: Option<String> }
pub struct ModalSpec<'a> { pub title: &'a str, pub icon: Option<&'a str>, pub size: ModalSize }
pub enum ModalResponse<T> { None, Button(T), Cancel }
/// Draw one standard modal body inside the shared `egui::Modal`:
/// header (icon + title + × close button), separator, scrollable body
/// (max height 75 % of the window), optional message line (error / info),
/// separator, footer with buttons right-aligned in the given order
/// (Secondary/Cancel on the left of Primary, Danger in red, Primary filled
/// with the accent colour). Enter triggers the Primary button unless a
/// multiline text edit or a popup (completion, combo) has focus; Escape and
/// the × give `Cancel`. Width is fixed by `size` (bounded by the window).
pub fn show<T: Clone>(ui: &mut egui::Ui, spec: ModalSpec, body: impl FnOnce(&mut egui::Ui),
                      message: Option<(MessageKind, &str)>, buttons: &[ModalButton<T>]) -> ModalResponse<T>;
pub enum MessageKind { Error, Info, Success }
/// Standard confirmation: `text` (and optional bullet list), Cancel + one
/// confirm button (Primary or Danger).
pub fn confirm<T: Clone>(ui: &mut egui::Ui, title: &str, text: &str, items: &[String],
                         confirm: ModalButton<T>) -> ModalResponse<T>;
```

Règles :
- Tailles : confirmations = `Small` ; connexion, structure, export/import =
  `Medium` ; historique, recherche de table = `Large`.
- Libellés de boutons : verbe à l'infinitif (« Enregistrer », « Supprimer »,
  « Vider », « Quitter quand même »), « Annuler » toujours présent sauf modale
  d'information ; destructif en `Danger`.
- Échap et clic hors de la modale = Annuler, partout (déjà géré par
  `modal.should_close()` dans `dialogs/mod.rs` : le garder cohérent avec
  `ModalResponse::Cancel`, y compris les effets de bord existants —
  `DiscardEdits` → `answer_discard(Cancel)`, `ConfirmQuit` →
  `updater.cancel_restart()`, `Connection` → abandon du test et oubli des
  connexions Azure testées, etc.).
- Le contenu long défile dans le corps ; l'en-tête et les boutons restent visibles.
- Aucune régression de comportement : les tests existants des dialogues doivent
  passer (adapter seulement leur forme).

## Conventions
- `RUSTC_WRAPPER="C:/Users/alexa/scoop/apps/sccache/current/sccache.exe" cargo test -q` (Bash). Base : 671 / 12 ; clippy 26.
- UI en français ; commentaires/commits en anglais terminés par une ligne vide
  puis `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Vérification visuelle **dans l'appli réelle** si la session est déverrouillée
  (config jetable `STORINGUNICORNS_CONFIG_DIR` dans le scratchpad, jamais la
  vraie) ; sinon rendu hors écran du vrai code dans un test jetable non commité.
  Captures avant/après dans `scratchpad\shots\` (noms nouveaux).

### Task 1 : composant + confirmations
- [ ] `src/gui/modal.rs` selon le contrat ; tests sur la logique pure (Enter →
  Primary sauf focus multiligne / popup, Échap → Cancel, ordre et types des
  boutons, taille bornée par la fenêtre).
- [ ] Migrer `ConfirmDeleteConnection`, `DiscardEdits`, `CloseTab`,
  `ConfirmRerun`, `ConfirmQuit`, `EndTx`, `ConfirmTruncate` vers `confirm` /
  `show`, en gardant leurs effets.
- [ ] Commit `feat(gui): one standard modal; confirmations use it`.

### Task 2 : formulaires (en parallèle, après Task 1)
- [ ] `Connection` (Medium ; boutons Enregistrer / Tester / Annuler ; la ligne
  d'attente navigateur et les erreurs dans la zone de message) et `Structure`.
- [ ] Commit `feat(gui): connection and structure dialogs use the standard modal`.

### Task 3 : listes et export/import (en parallèle, après Task 1)
- [ ] `History`, `TableSearch` (Large) et `Batch` (Medium).
- [ ] Commit `feat(gui): history, table search and batch dialogs use the standard modal`.

### Task 4 : CHANGELOG `## [Non publié]` (Modifications : modales homogènes).
