// SPDX-License-Identifier: GPL-3.0-or-later
//
// Mail mutations go through this service instead of being implemented by UI
// callbacks.  The local reader is intentionally read-only with regard to
// Evolution's files; only client-owned read/flag metadata is writable until a
// Camel/EDS backend is available.

use std::fmt;
use std::fs;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::camel_mail_service::{self, CamelMailOperation};
use super::mail_store_service::{MailBackendLocation, MailStoreService};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailOperationKind {
    SetMessageState,
    Delete,
    Move,
    Archive,
    MarkJunk,
    MarkNotJunk,
    MarkImportant,
    MarkNotImportant,
}

impl fmt::Display for MailOperationKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::SetMessageState => "Lesestatus/Markierung",
            Self::Delete => "Löschen",
            Self::Move => "Verschieben",
            Self::Archive => "Archivieren",
            Self::MarkJunk => "Als Spam markieren",
            Self::MarkNotJunk => "Als Nicht-Spam markieren",
            Self::MarkImportant => "Als wichtig markieren",
            Self::MarkNotImportant => "Wichtig-Markierung entfernen",
        };
        formatter.write_str(label)
    }
}

#[derive(Debug)]
pub enum MailOperationError {
    Io(io::Error),
    Backend {
        operation: MailOperationKind,
        message: String,
    },
    Conflict {
        operation: MailOperationKind,
        message: String,
    },
    Unsupported {
        operation: MailOperationKind,
        backend: &'static str,
    },
}

impl fmt::Display for MailOperationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Backend { operation, message } => {
                write!(formatter, "{operation} ist fehlgeschlagen: {message}")
            }
            Self::Conflict { operation, message } => {
                write!(formatter, "Konflikt bei {operation}: {message}")
            }
            Self::Unsupported { operation, backend } => write!(
                formatter,
                "{operation} ist mit dem Backend {backend} noch nicht verfügbar"
            ),
        }
    }
}

impl std::error::Error for MailOperationError {}

impl From<io::Error> for MailOperationError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum MailOperation {
    SetMessageState {
        id: String,
        location: Option<MailBackendLocation>,
        is_unread: bool,
        is_flagged: bool,
    },
    Delete {
        id: String,
        location: Option<MailBackendLocation>,
    },
    Move {
        id: String,
        location: Option<MailBackendLocation>,
        destination: String,
    },
    Archive {
        id: String,
        location: Option<MailBackendLocation>,
    },
    SetJunk {
        id: String,
        location: Option<MailBackendLocation>,
        is_junk: bool,
    },
    SetImportant {
        id: String,
        location: Option<MailBackendLocation>,
        is_important: bool,
    },
}

/// A reversible user action.  The backend mutation is executed before a
/// command enters the history, so failed or conflicted operations can never
/// leave a misleading undo entry behind.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct MailCommand {
    forward: MailOperation,
    inverse: MailOperation,
    description: String,
}

impl MailCommand {
    pub(crate) fn set_message_state(
        id: impl Into<String>,
        location: Option<MailBackendLocation>,
        before_unread: bool,
        before_flagged: bool,
        after_unread: bool,
        after_flagged: bool,
    ) -> Self {
        let id = id.into();
        Self {
            forward: MailOperation::SetMessageState {
                id: id.clone(),
                location: location.clone(),
                is_unread: after_unread,
                is_flagged: after_flagged,
            },
            inverse: MailOperation::SetMessageState {
                id,
                location,
                is_unread: before_unread,
                is_flagged: before_flagged,
            },
            description: "Lesestatus/Markierung".to_string(),
        }
    }

    pub(crate) fn move_to(
        id: impl Into<String>,
        location: MailBackendLocation,
        destination: impl Into<String>,
    ) -> Self {
        let id = id.into();
        let destination = destination.into();
        let inverse_location = MailBackendLocation {
            source_uid: location.source_uid.clone(),
            folder_name: destination.clone(),
            message_uid: location.message_uid.clone(),
        };
        Self {
            forward: MailOperation::Move {
                id: id.clone(),
                location: Some(location.clone()),
                destination: destination.clone(),
            },
            inverse: MailOperation::Move {
                id,
                location: Some(inverse_location),
                destination: location.folder_name,
            },
            description: format!("Nachricht nach {destination} verschieben"),
        }
    }

    pub(crate) fn set_junk(
        id: impl Into<String>,
        location: MailBackendLocation,
        before: bool,
        after: bool,
    ) -> Self {
        let id = id.into();
        Self {
            forward: MailOperation::SetJunk {
                id: id.clone(),
                location: Some(location.clone()),
                is_junk: after,
            },
            inverse: MailOperation::SetJunk {
                id,
                location: Some(location),
                is_junk: before,
            },
            description: if after {
                "Als Spam markieren".to_string()
            } else {
                "Spam-Markierung entfernen".to_string()
            },
        }
    }

    pub(crate) fn set_important(
        id: impl Into<String>,
        location: MailBackendLocation,
        before: bool,
        after: bool,
    ) -> Self {
        let id = id.into();
        Self {
            forward: MailOperation::SetImportant {
                id: id.clone(),
                location: Some(location.clone()),
                is_important: after,
            },
            inverse: MailOperation::SetImportant {
                id,
                location: Some(location),
                is_important: before,
            },
            description: if after {
                "Als wichtig markieren".to_string()
            } else {
                "Wichtig-Markierung entfernen".to_string()
            },
        }
    }

    pub(crate) fn description(&self) -> &str {
        &self.description
    }
}

const COMMAND_HISTORY_VERSION: u32 = 1;
const COMMAND_HISTORY_LIMIT: usize = 100;

#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct MailCommandStack {
    undo: Vec<MailCommand>,
    redo: Vec<MailCommand>,
    #[serde(skip)]
    path: Option<PathBuf>,
}

impl MailCommandStack {
    pub(crate) fn persistent() -> Self {
        let path = std::env::var_os("EVOLUTION_GTK4_MAIL_UNDO_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| gtk::glib::user_data_dir().join("evolution-gtk4/mail-undo.json"));
        Self::load_from(path)
    }

    fn load_from(path: PathBuf) -> Self {
        let Ok(raw) = fs::read(&path) else {
            return Self {
                path: Some(path),
                ..Self::default()
            };
        };
        let Ok(file) = serde_json::from_slice::<CommandHistoryFile>(&raw) else {
            tracing::warn!(path = %path.display(), "Ignoring invalid mail undo history");
            return Self {
                path: Some(path),
                ..Self::default()
            };
        };
        if file.version != COMMAND_HISTORY_VERSION {
            tracing::warn!(
                path = %path.display(),
                version = file.version,
                expected = COMMAND_HISTORY_VERSION,
                "Ignoring unsupported mail undo history version"
            );
            return Self {
                path: Some(path),
                ..Self::default()
            };
        }
        let mut stack = Self {
            undo: file.undo,
            redo: file.redo,
            path: Some(path),
        };
        stack.trim_history();
        stack
    }

    #[cfg(test)]
    fn with_path(path: PathBuf) -> Self {
        Self::load_from(path)
    }

    fn persist(&self) {
        let Some(path) = self.path.as_ref() else {
            return;
        };
        let file = CommandHistoryFile {
            version: COMMAND_HISTORY_VERSION,
            undo: self.undo.clone(),
            redo: self.redo.clone(),
        };
        let Ok(encoded) = serde_json::to_vec_pretty(&file) else {
            tracing::warn!(path = %path.display(), "Could not serialize mail undo history");
            return;
        };
        if let Some(parent) = path.parent() {
            if let Err(error) = fs::create_dir_all(parent) {
                tracing::warn!(
                    path = %parent.display(),
                    %error,
                    "Could not create mail undo directory"
                );
                return;
            }
        }
        let temporary_path = path.with_extension("json.tmp");
        if let Err(error) =
            fs::write(&temporary_path, encoded).and_then(|()| fs::rename(&temporary_path, path))
        {
            tracing::warn!(path = %path.display(), %error, "Could not persist mail undo history");
        }
    }

    fn trim_history(&mut self) {
        if self.undo.len() > COMMAND_HISTORY_LIMIT {
            let excess = self.undo.len() - COMMAND_HISTORY_LIMIT;
            self.undo.drain(0..excess);
        }
        if self.redo.len() > COMMAND_HISTORY_LIMIT {
            let excess = self.redo.len() - COMMAND_HISTORY_LIMIT;
            self.redo.drain(0..excess);
        }
    }

    pub(crate) fn execute(
        &mut self,
        operations: &MailOperationsService,
        command: MailCommand,
    ) -> Result<(), MailOperationError> {
        operations.apply(command.forward.clone())?;
        self.undo.push(command);
        self.redo.clear();
        self.trim_history();
        self.persist();
        Ok(())
    }

    pub(crate) fn execute_batch(
        &mut self,
        operations: &MailOperationsService,
        commands: Vec<MailCommand>,
    ) -> Result<usize, MailOperationError> {
        let mut applied: Vec<MailCommand> = Vec::with_capacity(commands.len());
        for command in commands {
            if let Err(error) = operations.apply(command.forward.clone()) {
                for previous in applied.iter().rev() {
                    if let Err(rollback_error) = operations.apply(previous.inverse.clone()) {
                        tracing::error!(
                            %rollback_error,
                            "Could not roll back a partially applied mail batch"
                        );
                    }
                }
                return Err(error);
            }
            applied.push(command);
        }

        let applied_count = applied.len();
        if applied_count > 0 {
            self.undo.extend(applied);
            self.redo.clear();
            self.trim_history();
            self.persist();
        }
        Ok(applied_count)
    }

    pub(crate) fn undo(
        &mut self,
        operations: &MailOperationsService,
    ) -> Result<Option<MailCommand>, MailOperationError> {
        let Some(command) = self.undo.pop() else {
            return Ok(None);
        };
        if let Err(error) = operations.apply(command.inverse.clone()) {
            self.undo.push(command);
            return Err(error);
        }
        let result = command.clone();
        self.redo.push(command);
        self.trim_history();
        self.persist();
        Ok(Some(result))
    }

    pub(crate) fn redo(
        &mut self,
        operations: &MailOperationsService,
    ) -> Result<Option<MailCommand>, MailOperationError> {
        let Some(command) = self.redo.pop() else {
            return Ok(None);
        };
        if let Err(error) = operations.apply(command.forward.clone()) {
            self.redo.push(command);
            return Err(error);
        }
        let result = command.clone();
        self.undo.push(command);
        self.trim_history();
        self.persist();
        Ok(Some(result))
    }

    pub(crate) fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub(crate) fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct CommandHistoryFile {
    version: u32,
    #[serde(default)]
    undo: Vec<MailCommand>,
    #[serde(default)]
    redo: Vec<MailCommand>,
}

#[derive(Debug, Clone)]
pub struct MailOperationsService {
    store: MailStoreService,
}

impl MailOperationsService {
    pub fn new() -> Self {
        Self {
            store: MailStoreService::new(),
        }
    }

    #[cfg(test)]
    fn with_store(store: MailStoreService) -> Self {
        Self { store }
    }

    /// Apply a mutation supported by the current backend boundary.
    pub(crate) fn apply(&self, operation: MailOperation) -> Result<(), MailOperationError> {
        match operation {
            MailOperation::SetMessageState {
                id,
                location,
                is_unread,
                is_flagged,
            } => match location {
                Some(location) => self.apply_camel(
                    CamelMailOperation::SetMessageState {
                        location,
                        is_unread,
                        is_flagged,
                    },
                    MailOperationKind::SetMessageState,
                ),
                None => self
                    .store
                    .persist_message_state(&id, is_unread, is_flagged)
                    .map_err(Into::into),
            },
            MailOperation::Delete { location, .. } => match location {
                Some(location) => self.apply_camel(
                    CamelMailOperation::Delete { location },
                    MailOperationKind::Delete,
                ),
                None => Err(Self::unsupported(MailOperationKind::Delete)),
            },
            MailOperation::Move {
                location,
                destination,
                ..
            } => match location {
                Some(location) => self.apply_camel(
                    CamelMailOperation::Move {
                        location,
                        destination,
                    },
                    MailOperationKind::Move,
                ),
                None => Err(Self::unsupported(MailOperationKind::Move)),
            },
            MailOperation::Archive { location, .. } => match location {
                Some(location) => self.apply_camel(
                    CamelMailOperation::Archive { location },
                    MailOperationKind::Archive,
                ),
                None => Err(Self::unsupported(MailOperationKind::Archive)),
            },
            MailOperation::SetJunk {
                location, is_junk, ..
            } => match location {
                Some(location) => self.apply_camel(
                    CamelMailOperation::SetJunk { location, is_junk },
                    if is_junk {
                        MailOperationKind::MarkJunk
                    } else {
                        MailOperationKind::MarkNotJunk
                    },
                ),
                None => Err(Self::unsupported(if is_junk {
                    MailOperationKind::MarkJunk
                } else {
                    MailOperationKind::MarkNotJunk
                })),
            },
            MailOperation::SetImportant {
                location,
                is_important,
                ..
            } => match location {
                Some(location) => self.apply_camel(
                    CamelMailOperation::SetImportant {
                        location,
                        is_important,
                    },
                    if is_important {
                        MailOperationKind::MarkImportant
                    } else {
                        MailOperationKind::MarkNotImportant
                    },
                ),
                None => Err(Self::unsupported(if is_important {
                    MailOperationKind::MarkImportant
                } else {
                    MailOperationKind::MarkNotImportant
                })),
            },
        }
    }

    pub fn set_message_state(
        &self,
        id: &str,
        is_unread: bool,
        is_flagged: bool,
    ) -> Result<(), MailOperationError> {
        self.apply(MailOperation::SetMessageState {
            id: id.to_string(),
            location: None,
            is_unread,
            is_flagged,
        })
    }

    pub(crate) fn set_message_state_for_location(
        &self,
        id: &str,
        location: &MailBackendLocation,
        is_unread: bool,
        is_flagged: bool,
    ) -> Result<(), MailOperationError> {
        self.apply(MailOperation::SetMessageState {
            id: id.to_string(),
            location: Some(location.clone()),
            is_unread,
            is_flagged,
        })
    }

    pub fn mark_read(&self, id: &str, is_flagged: bool) -> Result<(), MailOperationError> {
        self.set_message_state(id, false, is_flagged)
    }

    pub(crate) fn mark_read_for_location(
        &self,
        id: &str,
        location: &MailBackendLocation,
        is_flagged: bool,
    ) -> Result<(), MailOperationError> {
        self.set_message_state_for_location(id, location, false, is_flagged)
    }

    #[allow(dead_code)]
    pub fn mark_unread(&self, id: &str, is_flagged: bool) -> Result<(), MailOperationError> {
        self.set_message_state(id, true, is_flagged)
    }

    pub fn set_flagged(
        &self,
        id: &str,
        is_unread: bool,
        is_flagged: bool,
    ) -> Result<(), MailOperationError> {
        self.set_message_state(id, is_unread, is_flagged)
    }

    pub(crate) fn set_flagged_for_location(
        &self,
        id: &str,
        location: &MailBackendLocation,
        is_unread: bool,
        is_flagged: bool,
    ) -> Result<(), MailOperationError> {
        self.set_message_state_for_location(id, location, is_unread, is_flagged)
    }

    pub fn delete(&self, id: &str) -> Result<(), MailOperationError> {
        self.apply(MailOperation::Delete {
            id: id.to_string(),
            location: None,
        })
    }

    pub(crate) fn delete_for_location(
        &self,
        id: &str,
        location: &MailBackendLocation,
    ) -> Result<(), MailOperationError> {
        self.apply(MailOperation::Delete {
            id: id.to_string(),
            location: Some(location.clone()),
        })
    }

    pub fn archive(&self, id: &str) -> Result<(), MailOperationError> {
        self.apply(MailOperation::Archive {
            id: id.to_string(),
            location: None,
        })
    }

    #[allow(dead_code)]
    pub fn move_to(&self, id: &str, destination: &str) -> Result<(), MailOperationError> {
        self.apply(MailOperation::Move {
            id: id.to_string(),
            location: None,
            destination: destination.to_string(),
        })
    }

    pub(crate) fn move_to_for_location(
        &self,
        id: &str,
        location: &MailBackendLocation,
        destination: &str,
    ) -> Result<(), MailOperationError> {
        self.apply(MailOperation::Move {
            id: id.to_string(),
            location: Some(location.clone()),
            destination: destination.to_string(),
        })
    }

    pub(crate) fn archive_for_location(
        &self,
        id: &str,
        location: &MailBackendLocation,
    ) -> Result<(), MailOperationError> {
        self.apply(MailOperation::Archive {
            id: id.to_string(),
            location: Some(location.clone()),
        })
    }

    pub(crate) fn set_junk_for_location(
        &self,
        id: &str,
        location: &MailBackendLocation,
        is_junk: bool,
    ) -> Result<(), MailOperationError> {
        self.apply(MailOperation::SetJunk {
            id: id.to_string(),
            location: Some(location.clone()),
            is_junk,
        })
    }

    pub fn set_junk(&self, id: &str, is_junk: bool) -> Result<(), MailOperationError> {
        self.apply(MailOperation::SetJunk {
            id: id.to_string(),
            location: None,
            is_junk,
        })
    }

    #[allow(dead_code)]
    pub(crate) fn set_important_for_location(
        &self,
        id: &str,
        location: &MailBackendLocation,
        is_important: bool,
    ) -> Result<(), MailOperationError> {
        self.apply(MailOperation::SetImportant {
            id: id.to_string(),
            location: Some(location.clone()),
            is_important,
        })
    }

    fn apply_camel(
        &self,
        operation: CamelMailOperation,
        kind: MailOperationKind,
    ) -> Result<(), MailOperationError> {
        camel_mail_service::apply_operation(operation).map_err(|message| {
            if message
                .trim_start()
                .to_ascii_lowercase()
                .starts_with("conflict:")
            {
                MailOperationError::Conflict {
                    operation: kind,
                    message: message.trim_start_matches("conflict:").trim().to_string(),
                }
            } else {
                MailOperationError::Backend {
                    operation: kind,
                    message,
                }
            }
        })
    }

    fn unsupported(operation: MailOperationKind) -> MailOperationError {
        MailOperationError::Unsupported {
            operation,
            backend: "lokaler Nur-Lese-Import",
        }
    }
}

impl Default for MailOperationsService {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::mail_metadata_service::{MailMessageState, MailMetadataStore};
    use std::fs;
    use std::path::PathBuf;

    #[test]
    fn read_and_flag_state_uses_the_persistent_sidecar() {
        let root = temporary_directory("operations-state");
        let mail_dir = root.join("mail");
        fs::create_dir_all(&mail_dir).expect("mail directory should be created");
        fs::write(
            mail_dir.join("message.eml"),
            "From: Sender <sender@example.org>\nSubject: Test\n\nBody\n",
        )
        .expect("mail should be written");

        let store =
            MailStoreService::with_mail_dir_and_state_path(&mail_dir, root.join("state.json"));
        let message_id = store
            .load_local_records()
            .first()
            .expect("message should parse")
            .id
            .clone();
        let operations = MailOperationsService::with_store(store.clone());

        operations
            .set_message_state(&message_id, false, true)
            .expect("state should persist");

        let state = MailMetadataStore::new(store.metadata_path()).load();
        assert_eq!(
            state.get(&message_id),
            Some(&MailMessageState {
                is_unread: false,
                is_flagged: true,
            })
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn destructive_operations_are_explicitly_rejected_by_read_only_backend() {
        let root = temporary_directory("operations-unsupported");
        let store = MailStoreService::with_mail_dir_and_state_path(
            root.join("mail"),
            root.join("state.json"),
        );
        let operations = MailOperationsService::with_store(store);

        let error = operations
            .delete("local:message#0")
            .expect_err("delete must not be reported as successful");
        assert!(matches!(
            error,
            MailOperationError::Unsupported {
                operation: MailOperationKind::Delete,
                ..
            }
        ));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn command_stack_only_records_successful_state_changes_and_replays_them() {
        let root = temporary_directory("operations-undo");
        let mail_dir = root.join("mail");
        fs::create_dir_all(&mail_dir).expect("mail directory should be created");
        fs::write(
            mail_dir.join("message.eml"),
            "From: Sender <sender@example.org>\nSubject: Test\n\nBody\n",
        )
        .expect("mail should be written");

        let store =
            MailStoreService::with_mail_dir_and_state_path(&mail_dir, root.join("state.json"));
        let message_id = store
            .load_local_records()
            .first()
            .expect("message should parse")
            .id
            .clone();
        let operations = MailOperationsService::with_store(store.clone());
        let mut history = MailCommandStack::default();

        history
            .execute(
                &operations,
                MailCommand::set_message_state(message_id.clone(), None, true, false, false, true),
            )
            .expect("local state change should succeed");
        assert!(history.can_undo());
        assert!(!history.can_redo());

        let state = MailMetadataStore::new(store.metadata_path()).load();
        assert_eq!(
            state.get(&message_id),
            Some(&MailMessageState {
                is_unread: false,
                is_flagged: true,
            })
        );

        let undone = history
            .undo(&operations)
            .expect("undo should succeed")
            .expect("one command should be undone");
        assert_eq!(undone.description(), "Lesestatus/Markierung");
        assert!(!history.can_undo());
        assert!(history.can_redo());
        let state = MailMetadataStore::new(store.metadata_path()).load();
        assert_eq!(
            state.get(&message_id),
            Some(&MailMessageState {
                is_unread: true,
                is_flagged: false,
            })
        );

        history
            .redo(&operations)
            .expect("redo should succeed")
            .expect("one command should be redone");
        assert!(history.can_undo());
        assert!(!history.can_redo());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn failed_command_is_not_added_to_history() {
        let root = temporary_directory("operations-undo-failed");
        let operations =
            MailOperationsService::with_store(MailStoreService::with_mail_dir(root.join("mail")));
        let mut history = MailCommandStack::default();
        let location = MailBackendLocation {
            source_uid: "source".to_string(),
            folder_name: "INBOX".to_string(),
            message_uid: "uid".to_string(),
        };

        assert!(
            history
                .execute(
                    &operations,
                    MailCommand::move_to("message", location, "Archive"),
                )
                .is_err()
        );
        assert!(!history.can_undo());
        assert!(!history.can_redo());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn failed_batch_rolls_back_already_applied_local_commands() {
        let root = temporary_directory("operations-batch-rollback");
        let mail_dir = root.join("mail");
        fs::create_dir_all(&mail_dir).expect("mail directory should be created");
        fs::write(
            mail_dir.join("message.eml"),
            "From: Sender <sender@example.org>\nSubject: Test\n\nBody\n",
        )
        .expect("mail should be written");
        let store =
            MailStoreService::with_mail_dir_and_state_path(&mail_dir, root.join("state.json"));
        let message_id = store
            .load_local_records()
            .first()
            .expect("message should parse")
            .id
            .clone();
        let operations = MailOperationsService::with_store(store.clone());
        let mut history = MailCommandStack::default();
        let location = MailBackendLocation {
            source_uid: "source".to_string(),
            folder_name: "INBOX".to_string(),
            message_uid: "uid".to_string(),
        };

        assert!(
            history
                .execute_batch(
                    &operations,
                    vec![
                        MailCommand::set_message_state(
                            message_id.clone(),
                            None,
                            true,
                            false,
                            false,
                            true,
                        ),
                        MailCommand::move_to("remote", location, "Archive"),
                    ],
                )
                .is_err()
        );
        assert!(!history.can_undo());
        assert_eq!(
            MailMetadataStore::new(store.metadata_path())
                .load()
                .get(&message_id),
            Some(&MailMessageState {
                is_unread: true,
                is_flagged: false,
            })
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn command_history_roundtrips_with_an_explicit_path() {
        let root = temporary_directory("operations-history");
        let path = root.join("mail-undo.json");
        let mut history = MailCommandStack::with_path(path.clone());
        history.undo.push(MailCommand::set_message_state(
            "message-1",
            None,
            true,
            false,
            false,
            true,
        ));
        history.persist();

        let reloaded = MailCommandStack::with_path(path);
        assert!(reloaded.can_undo());
        assert!(!reloaded.can_redo());

        let _ = fs::remove_dir_all(root);
    }

    fn temporary_directory(label: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("evolution-gtk4-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("temporary directory should be created");
        path
    }
}
