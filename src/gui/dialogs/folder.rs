//! The folder name prompt of the explorer: new folder, rename, or a new
//! folder for a connection being moved.

use egui_phosphor::regular as icon;

use crate::engine::config::folders;
use crate::engine::config::AppConfig;
use crate::gui::explorer::tree::join_folder;
use crate::gui::modal::{self, MessageKind, ModalButton, ModalResponse, ModalSize, ModalSpec};

/// What the typed name is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FolderGoal {
    /// A new folder in `parent` (`""`: the root).
    Create { parent: String },
    /// Rename folder `from` (it stays in its parent).
    Rename { from: String },
    /// Move `connection` into a new folder (a full path, from the root).
    MoveConnection { connection: String },
}

pub struct FolderPrompt {
    pub goal: FolderGoal,
    pub name: String,
    pub error: Option<String>,
    /// The field takes the focus on the first frame.
    focus: bool,
}

impl FolderPrompt {
    pub fn new(goal: FolderGoal) -> Self {
        let name = match &goal {
            FolderGoal::Rename { from } => folders::name(from).to_string(),
            _ => String::new(),
        };
        Self {
            goal,
            name,
            error: None,
            focus: true,
        }
    }

    /// The folder's full path once named (normalized), or why not.
    pub fn target(&self) -> Result<String, String> {
        let name = folders::normalize(&self.name)?;
        Ok(match &self.goal {
            FolderGoal::Create { parent } => join_folder(parent, &name),
            FolderGoal::Rename { from } => join_folder(folders::parent(from), &name),
            FolderGoal::MoveConnection { .. } => name,
        })
    }

    /// Apply the prompt to `config`: the folder's new path, or why not.
    pub fn apply(&self, config: &mut AppConfig) -> Result<String, String> {
        let target = self.target()?;
        match &self.goal {
            FolderGoal::Create { .. } => config.create_folder(&target),
            FolderGoal::Rename { from } => {
                config.rename_folder(from, &target)?;
                Ok(target)
            }
            FolderGoal::MoveConnection { connection } => {
                // An existing folder is fine: the connection just goes there.
                if !config.folder_exists(&target) {
                    config.create_folder(&target)?;
                }
                config.move_connection(connection, Some(&target))?;
                Ok(target)
            }
        }
    }

    fn title(&self) -> &'static str {
        match self.goal {
            FolderGoal::Create { .. } | FolderGoal::MoveConnection { .. } => "Nouveau dossier",
            FolderGoal::Rename { .. } => "Renommer le dossier",
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> ModalResponse<()> {
        let hint = match &self.goal {
            FolderGoal::Create { parent } if !parent.is_empty() => {
                format!("Dans « {parent} ». « / » crée des sous-dossiers.")
            }
            FolderGoal::Rename { from } if !folders::parent(from).is_empty() => {
                format!("Dans « {} ».", folders::parent(from))
            }
            _ => "« / » sépare les sous-dossiers (ex. Clients/ACME).".to_string(),
        };
        let confirm = match self.goal {
            FolderGoal::Rename { .. } => "Renommer",
            FolderGoal::Create { .. } => "Créer",
            FolderGoal::MoveConnection { .. } => "Créer et déplacer",
        };
        let error = self.error.clone();
        let focus = std::mem::take(&mut self.focus);
        let title = self.title();
        let name = &mut self.name;
        modal::show(
            ui,
            ModalSpec::new(title, ModalSize::Small).icon(icon::FOLDER_PLUS),
            |ui| {
                ui.label("Nom");
                let field = ui.add(
                    egui::TextEdit::singleline(name)
                        .hint_text("Nom du dossier")
                        .desired_width(f32::INFINITY),
                );
                if focus {
                    field.request_focus();
                }
                ui.label(egui::RichText::new(hint).weak().small());
            },
            error.as_deref().map(|e| (MessageKind::Error, e)),
            &[ModalButton::primary(confirm, ())],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::models::ConnectionConfig;

    fn prompt(goal: FolderGoal, name: &str) -> FolderPrompt {
        FolderPrompt {
            name: name.into(),
            ..FolderPrompt::new(goal)
        }
    }

    #[test]
    fn names_are_normalized_and_placed_in_their_parent() {
        let create = |parent: &str, name| {
            prompt(
                FolderGoal::Create {
                    parent: parent.into(),
                },
                name,
            )
            .target()
        };
        assert_eq!(create("", " Clients / ACME "), Ok("Clients/ACME".into()));
        assert_eq!(create("Clients", "Prod"), Ok("Clients/Prod".into()));
        assert!(create("", "  ").is_err());
        assert!(create("", "..").is_err());
        let rename = prompt(
            FolderGoal::Rename {
                from: "Clients/ACME".into(),
            },
            "Acme",
        );
        assert_eq!(rename.target(), Ok("Clients/Acme".into()));
        assert_eq!(
            FolderPrompt::new(FolderGoal::Rename {
                from: "Clients/ACME".into()
            })
            .name,
            "ACME",
            "starts from the current name"
        );
    }

    #[test]
    fn applying_creates_renames_or_moves() {
        let mut config = AppConfig {
            connections: vec![ConnectionConfig {
                name: "prod".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let create = prompt(
            FolderGoal::Create {
                parent: String::new(),
            },
            "A",
        );
        assert_eq!(create.apply(&mut config), Ok("A".into()));
        assert!(create.apply(&mut config).is_err(), "exists already");
        let rename = prompt(FolderGoal::Rename { from: "A".into() }, "B");
        assert_eq!(rename.apply(&mut config), Ok("B".into()));
        assert_eq!(config.folders, ["B"]);
        let move_to = |name| {
            prompt(
                FolderGoal::MoveConnection {
                    connection: "prod".into(),
                },
                name,
            )
        };
        assert_eq!(move_to("B").apply(&mut config), Ok("B".into()), "existing");
        assert_eq!(move_to("C/D").apply(&mut config), Ok("C/D".into()));
        assert_eq!(config.connections[0].folder.as_deref(), Some("C/D"));
    }
}
