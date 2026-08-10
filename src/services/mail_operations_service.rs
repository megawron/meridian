// SPDX-License-Identifier: GPL-3.0-or-later
//
// Mail mutations go through this service instead of being implemented by UI
// callbacks.  The local reader is intentionally read-only with regard to
// Evolution's files; only client-owned read/flag metadata is writable until a
// Camel/EDS backend is available.

use std::fmt;
use std::io;

use super::mail_store_service::MailStoreService;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailOperationKind {
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
    Unsupported {
        operation: MailOperationKind,
        backend: &'static str,
    },
}

impl fmt::Display for MailOperationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
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
#[derive(Debug, Clone)]
pub enum MailOperation {
    SetMessageState {
        id: String,
        is_unread: bool,
        is_flagged: bool,
    },
    Delete {
        id: String,
    },
    Move {
        id: String,
        destination: String,
    },
    Archive {
        id: String,
    },
    SetJunk {
        id: String,
        is_junk: bool,
    },
    SetImportant {
        id: String,
        is_important: bool,
    },
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
    pub fn apply(&self, operation: MailOperation) -> Result<(), MailOperationError> {
        match operation {
            MailOperation::SetMessageState {
                id,
                is_unread,
                is_flagged,
            } => self
                .store
                .persist_message_state(&id, is_unread, is_flagged)
                .map_err(Into::into),
            MailOperation::Delete { .. } => Err(Self::unsupported(MailOperationKind::Delete)),
            MailOperation::Move { .. } => Err(Self::unsupported(MailOperationKind::Move)),
            MailOperation::Archive { .. } => Err(Self::unsupported(MailOperationKind::Archive)),
            MailOperation::SetJunk { is_junk, .. } => Err(Self::unsupported(if is_junk {
                MailOperationKind::MarkJunk
            } else {
                MailOperationKind::MarkNotJunk
            })),
            MailOperation::SetImportant { is_important, .. } => {
                Err(Self::unsupported(if is_important {
                    MailOperationKind::MarkImportant
                } else {
                    MailOperationKind::MarkNotImportant
                }))
            }
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
            is_unread,
            is_flagged,
        })
    }

    pub fn mark_read(&self, id: &str, is_flagged: bool) -> Result<(), MailOperationError> {
        self.set_message_state(id, false, is_flagged)
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

    pub fn delete(&self, id: &str) -> Result<(), MailOperationError> {
        self.apply(MailOperation::Delete { id: id.to_string() })
    }

    #[allow(dead_code)]
    pub fn move_to(&self, id: &str, destination: &str) -> Result<(), MailOperationError> {
        self.apply(MailOperation::Move {
            id: id.to_string(),
            destination: destination.to_string(),
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

    fn temporary_directory(label: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("evolution-gtk4-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("temporary directory should be created");
        path
    }
}
