// SPDX-License-Identifier: GPL-3.0-or-later
//
// Mail mutations go through this service instead of being implemented by UI
// callbacks.  The local reader is intentionally read-only with regard to
// Evolution's files; client-owned state and logical local folder/deletion
// overrides are writable until a Camel/EDS backend is available.

use std::fmt;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::camel_mail_service::{self, CamelMailOperation};
use super::mail_store_service::{MailBackendLocation, MailStoreService};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailOperationKind {
    SetMessageState,
    Delete,
    Move,
    Copy,
    Archive,
    MarkJunk,
    MarkNotJunk,
    MarkImportant,
    MarkNotImportant,
    SetLabel,
}

impl fmt::Display for MailOperationKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::SetMessageState => "Lesestatus/Markierung",
            Self::Delete => "Löschen",
            Self::Move => "Verschieben",
            Self::Copy => "Kopieren",
            Self::Archive => "Archivieren",
            Self::MarkJunk => "Als Spam markieren",
            Self::MarkNotJunk => "Als Nicht-Spam markieren",
            Self::MarkImportant => "Als wichtig markieren",
            Self::MarkNotImportant => "Wichtig-Markierung entfernen",
            Self::SetLabel => "Label ändern",
        };
        formatter.write_str(label)
    }
}

static NEXT_OPERATION_ID: AtomicU64 = AtomicU64::new(1);

/// Stable correlation ID for one logical mutation.
///
/// A GTK caller can retain this value while a worker is running and discard a
/// late result which belongs to an older selection or view generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct MailOperationId(u64);

impl MailOperationId {
    fn next() -> Self {
        Self(NEXT_OPERATION_ID.fetch_add(1, Ordering::Relaxed))
    }

    pub(crate) fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for MailOperationId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Framework-neutral lifecycle of one mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MailOperationStatus {
    Queued = 0,
    Running = 1,
    RollingBack = 2,
    Succeeded = 3,
    Failed = 4,
    Conflict = 5,
    Cancelled = 6,
    RollbackFailed = 7,
}

impl MailOperationStatus {
    fn from_atomic(value: u8) -> Self {
        match value {
            0 => Self::Queued,
            1 => Self::Running,
            2 => Self::RollingBack,
            3 => Self::Succeeded,
            4 => Self::Failed,
            5 => Self::Conflict,
            6 => Self::Cancelled,
            7 => Self::RollbackFailed,
            _ => Self::Failed,
        }
    }

    pub(crate) fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded
                | Self::Failed
                | Self::Conflict
                | Self::Cancelled
                | Self::RollbackFailed
        )
    }
}

impl fmt::Display for MailOperationStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Queued => "wartend",
            Self::Running => "läuft",
            Self::RollingBack => "wird zurückgerollt",
            Self::Succeeded => "erfolgreich",
            Self::Failed => "fehlgeschlagen",
            Self::Conflict => "Konflikt",
            Self::Cancelled => "abgebrochen",
            Self::RollbackFailed => "Rückabwicklung fehlgeschlagen",
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
    Busy {
        operation: MailOperationKind,
        message: String,
    },
    Cancelled {
        operation: MailOperationKind,
    },
    Lifecycle {
        operation: MailOperationKind,
        status: MailOperationStatus,
    },
    RollbackFailed {
        operation: MailOperationKind,
        message: String,
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
            Self::Busy { operation, message } => {
                write!(
                    formatter,
                    "{operation} für {message} wird bereits ausgeführt"
                )
            }
            Self::Cancelled { operation } => {
                write!(formatter, "{operation} wurde abgebrochen")
            }
            Self::Lifecycle { operation, status } => write!(
                formatter,
                "{operation} kann im Status {status} nicht erneut gestartet werden"
            ),
            Self::RollbackFailed { operation, message } => write!(
                formatter,
                "{operation} ist fehlgeschlagen und konnte nicht vollständig zurückgerollt werden: {message}"
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

impl MailOperationError {
    fn status(&self) -> MailOperationStatus {
        match self {
            Self::Conflict { .. } => MailOperationStatus::Conflict,
            Self::Cancelled { .. } => MailOperationStatus::Cancelled,
            Self::RollbackFailed { .. } => MailOperationStatus::RollbackFailed,
            Self::Io(_)
            | Self::Backend { .. }
            | Self::Unsupported { .. }
            | Self::Busy { .. }
            | Self::Lifecycle { .. } => MailOperationStatus::Failed,
        }
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled { .. })
    }

    pub(crate) fn is_conflict(&self) -> bool {
        matches!(self, Self::Conflict { .. })
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
    Copy {
        id: String,
        location: Option<MailBackendLocation>,
        destination: String,
    },
    RemoveCopy {
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
    SetLabel {
        id: String,
        location: Option<MailBackendLocation>,
        label: String,
        labeled: bool,
    },
    RestoreDeleted {
        id: String,
        location: Option<MailBackendLocation>,
    },
    RestoreArchived {
        id: String,
        location: Option<MailBackendLocation>,
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
            message_id: location.message_id.clone(),
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

    pub(crate) fn set_junk_local(
        id: impl Into<String>,
        previous_folder: impl Into<String>,
    ) -> Self {
        let id = id.into();
        let previous_folder = previous_folder.into();
        Self {
            forward: MailOperation::SetJunk {
                id: id.clone(),
                location: None,
                is_junk: true,
            },
            inverse: MailOperation::Move {
                id,
                location: None,
                destination: previous_folder,
            },
            description: "Nachricht lokal als Spam markieren".to_string(),
        }
    }

    pub(crate) fn clear_junk_local(id: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            forward: MailOperation::SetJunk {
                id: id.clone(),
                location: None,
                is_junk: false,
            },
            inverse: MailOperation::SetJunk {
                id,
                location: None,
                is_junk: true,
            },
            description: "Lokale Spam-Markierung entfernen".to_string(),
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

    pub(crate) fn set_important_local(id: impl Into<String>, before: bool, after: bool) -> Self {
        let id = id.into();
        Self {
            forward: MailOperation::SetImportant {
                id: id.clone(),
                location: None,
                is_important: after,
            },
            inverse: MailOperation::SetImportant {
                id,
                location: None,
                is_important: before,
            },
            description: if after {
                "Als wichtig markieren".to_string()
            } else {
                "Wichtig-Markierung entfernen".to_string()
            },
        }
    }

    pub(crate) fn set_label_local(
        id: impl Into<String>,
        label: impl Into<String>,
        before: bool,
        after: bool,
    ) -> Self {
        let id = id.into();
        let label = label.into().trim().to_string();
        Self {
            forward: MailOperation::SetLabel {
                id: id.clone(),
                location: None,
                label: label.clone(),
                labeled: after,
            },
            inverse: MailOperation::SetLabel {
                id,
                location: None,
                label,
                labeled: before,
            },
            description: if after {
                "Label gesetzt".to_string()
            } else {
                "Label entfernt".to_string()
            },
        }
    }

    pub(crate) fn set_label_for_location(
        id: impl Into<String>,
        location: MailBackendLocation,
        label: impl Into<String>,
        before: bool,
        after: bool,
    ) -> Self {
        let id = id.into();
        let label = label.into().trim().to_string();
        Self {
            forward: MailOperation::SetLabel {
                id: id.clone(),
                location: Some(location.clone()),
                label: label.clone(),
                labeled: after,
            },
            inverse: MailOperation::SetLabel {
                id,
                location: Some(location),
                label,
                labeled: before,
            },
            description: if after {
                "Label gesetzt".to_string()
            } else {
                "Label entfernt".to_string()
            },
        }
    }

    pub(crate) fn delete(id: impl Into<String>, location: MailBackendLocation) -> Self {
        let id = id.into();
        Self {
            forward: MailOperation::Delete {
                id: id.clone(),
                location: Some(location.clone()),
            },
            inverse: MailOperation::RestoreDeleted {
                id,
                location: Some(location),
            },
            description: "Nachricht löschen".to_string(),
        }
    }

    pub(crate) fn delete_local(id: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            forward: MailOperation::Delete {
                id: id.clone(),
                location: None,
            },
            inverse: MailOperation::RestoreDeleted { id, location: None },
            description: "Nachricht lokal ausblenden".to_string(),
        }
    }

    pub(crate) fn archive(id: impl Into<String>, location: MailBackendLocation) -> Self {
        let id = id.into();
        Self {
            forward: MailOperation::Archive {
                id: id.clone(),
                location: Some(location.clone()),
            },
            inverse: MailOperation::RestoreArchived {
                id,
                location: Some(location),
            },
            description: "Nachricht archivieren".to_string(),
        }
    }

    pub(crate) fn archive_local(id: impl Into<String>, previous_folder: impl Into<String>) -> Self {
        let id = id.into();
        let previous_folder = previous_folder.into();
        Self {
            forward: MailOperation::Archive {
                id: id.clone(),
                location: None,
            },
            inverse: MailOperation::Move {
                id,
                location: None,
                destination: previous_folder,
            },
            description: "Nachricht lokal archivieren".to_string(),
        }
    }

    pub(crate) fn move_local(
        id: impl Into<String>,
        previous_folder: impl Into<String>,
        destination: impl Into<String>,
    ) -> Self {
        let id = id.into();
        let previous_folder = previous_folder.into();
        let destination = destination.into();
        Self {
            forward: MailOperation::Move {
                id: id.clone(),
                location: None,
                destination: destination.clone(),
            },
            inverse: MailOperation::Move {
                id,
                location: None,
                destination: previous_folder,
            },
            description: format!("Nachricht lokal nach {destination} verschieben"),
        }
    }

    pub(crate) fn copy_local(id: impl Into<String>, destination: impl Into<String>) -> Self {
        let id = id.into();
        let destination = destination.into();
        Self {
            forward: MailOperation::Copy {
                id: id.clone(),
                location: None,
                destination: destination.clone(),
            },
            inverse: MailOperation::RemoveCopy {
                id,
                location: None,
                destination: destination.clone(),
            },
            description: format!("Nachricht lokal nach {destination} kopiert"),
        }
    }

    pub(crate) fn copy_remote(
        id: impl Into<String>,
        location: MailBackendLocation,
        destination: impl Into<String>,
    ) -> Self {
        let id = id.into();
        let destination = destination.into();
        Self {
            forward: MailOperation::Copy {
                id: id.clone(),
                location: Some(location),
                destination: destination.clone(),
            },
            // The backend-created copy location is filled in immediately
            // after the forward operation succeeds.
            inverse: MailOperation::RemoveCopy {
                id,
                location: None,
                destination: destination.clone(),
            },
            description: format!("Nachricht nach {destination} kopiert"),
        }
    }

    fn update_inverse_from_result(&mut self, result: Option<MailBackendLocation>) {
        let Some(copy_location) = result else {
            return;
        };
        let MailOperation::Copy {
            id, destination, ..
        } = &self.forward
        else {
            return;
        };
        self.inverse = MailOperation::RemoveCopy {
            id: id.clone(),
            location: Some(copy_location),
            destination: destination.clone(),
        };
    }

    pub(crate) fn description(&self) -> &str {
        &self.description
    }

    fn operation_kind(&self) -> MailOperationKind {
        self.forward.kind()
    }

    fn operation_key(&self) -> MailOperationKey {
        self.forward.operation_key()
    }
}

impl MailOperation {
    fn kind(&self) -> MailOperationKind {
        match self {
            Self::SetMessageState { .. } => MailOperationKind::SetMessageState,
            Self::Delete { .. } | Self::RestoreDeleted { .. } => MailOperationKind::Delete,
            Self::Move { .. } => MailOperationKind::Move,
            Self::Copy { .. } | Self::RemoveCopy { .. } => MailOperationKind::Copy,
            Self::Archive { .. } | Self::RestoreArchived { .. } => MailOperationKind::Archive,
            Self::SetJunk { is_junk: true, .. } => MailOperationKind::MarkJunk,
            Self::SetJunk { is_junk: false, .. } => MailOperationKind::MarkNotJunk,
            Self::SetImportant {
                is_important: true, ..
            } => MailOperationKind::MarkImportant,
            Self::SetImportant {
                is_important: false,
                ..
            } => MailOperationKind::MarkNotImportant,
            Self::SetLabel { .. } => MailOperationKind::SetLabel,
        }
    }

    fn operation_key(&self) -> MailOperationKey {
        let (id, location) = match self {
            Self::SetMessageState { id, location, .. }
            | Self::Delete { id, location }
            | Self::Move { id, location, .. }
            | Self::Copy { id, location, .. }
            | Self::RemoveCopy { id, location, .. }
            | Self::Archive { id, location }
            | Self::SetJunk { id, location, .. }
            | Self::SetImportant { id, location, .. }
            | Self::SetLabel { id, location, .. }
            | Self::RestoreDeleted { id, location }
            | Self::RestoreArchived { id, location } => (id, location),
        };
        let identity = location.as_ref().map_or_else(
            || format!("local:{id}"),
            |location| {
                location
                    .message_id
                    .as_deref()
                    .filter(|message_id| !message_id.trim().is_empty())
                    .map(|message_id| {
                        format!("remote:{}:message-id:{message_id}", location.source_uid)
                    })
                    .unwrap_or_else(|| {
                        format!(
                            "remote:{}:{}:{}",
                            location.source_uid, location.folder_name, location.message_uid
                        )
                    })
            },
        );
        MailOperationKey { identity }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct MailOperationKey {
    identity: String,
}

/// Coordinates worker-owned mail mutations without depending on GTK. A
/// message can have at most one mutation in flight, which prevents stale
/// read/flag/move results from racing each other. The ticket is intentionally
/// movable to a worker thread while its cancellation handle stays with the UI.
#[derive(Debug, Clone, Default)]
pub(crate) struct MailOperationCoordinator {
    in_flight: Arc<Mutex<HashSet<MailOperationKey>>>,
}

impl MailOperationCoordinator {
    pub(crate) fn prepare(
        &self,
        command: MailCommand,
    ) -> Result<PreparedMailOperation, MailOperationError> {
        let key = command.operation_key();
        let mut in_flight = self
            .in_flight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !in_flight.insert(key.clone()) {
            return Err(MailOperationError::Busy {
                operation: command.operation_kind(),
                message: key.identity,
            });
        }
        drop(in_flight);

        Ok(PreparedMailOperation {
            command: Some(command),
            key,
            in_flight: Arc::clone(&self.in_flight),
            context: MailOperationContext::new(),
        })
    }

    #[cfg(test)]
    fn active_count(&self) -> usize {
        self.in_flight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct MailOperationCancellation {
    requested: Arc<AtomicBool>,
}

impl MailOperationCancellation {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn cancel(&self) {
        self.requested.store(true, Ordering::Release);
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }

    fn check(&self, operation: MailOperationKind) -> Result<(), MailOperationError> {
        if self.is_cancelled() {
            Err(MailOperationError::Cancelled { operation })
        } else {
            Ok(())
        }
    }
}

/// One-shot lifecycle and cancellation handle for a mail mutation.
///
/// Cloning the context only clones the observation/cancellation handle; the
/// operation itself still has to be executed exactly once. Reusing a terminal
/// context returns a lifecycle error instead of issuing a duplicate action.
#[derive(Debug, Clone)]
pub(crate) struct MailOperationContext {
    id: MailOperationId,
    cancellation: MailOperationCancellation,
    status: Arc<AtomicU8>,
}

impl MailOperationContext {
    pub(crate) fn new() -> Self {
        Self::with_cancellation(MailOperationCancellation::new())
    }

    fn with_cancellation(cancellation: MailOperationCancellation) -> Self {
        Self {
            id: MailOperationId::next(),
            cancellation,
            status: Arc::new(AtomicU8::new(MailOperationStatus::Queued as u8)),
        }
    }

    pub(crate) fn id(&self) -> MailOperationId {
        self.id
    }

    pub(crate) fn cancellation(&self) -> MailOperationCancellation {
        self.cancellation.clone()
    }

    pub(crate) fn status(&self) -> MailOperationStatus {
        MailOperationStatus::from_atomic(self.status.load(Ordering::Acquire))
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    /// Request cancellation while the operation is still useful to cancel.
    /// Returns `false` after a terminal result has been published.
    pub(crate) fn cancel(&self) -> bool {
        loop {
            let status = self.status();
            if status.is_terminal() {
                return false;
            }
            self.cancellation.cancel();
            if status == MailOperationStatus::Queued {
                if self
                    .status
                    .compare_exchange(
                        MailOperationStatus::Queued as u8,
                        MailOperationStatus::Cancelled as u8,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_err()
                {
                    continue;
                }
            }
            return true;
        }
    }

    fn begin(&self, operation: MailOperationKind) -> Result<(), MailOperationError> {
        if self.cancellation.is_cancelled() {
            self.status
                .compare_exchange(
                    MailOperationStatus::Queued as u8,
                    MailOperationStatus::Cancelled as u8,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .ok();
            return Err(MailOperationError::Cancelled { operation });
        }

        match self.status.compare_exchange(
            MailOperationStatus::Queued as u8,
            MailOperationStatus::Running as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => Ok(()),
            Err(value) => Err(MailOperationError::Lifecycle {
                operation,
                status: MailOperationStatus::from_atomic(value),
            }),
        }
    }

    fn mark_rolling_back(&self) {
        self.status
            .store(MailOperationStatus::RollingBack as u8, Ordering::Release);
    }

    fn finish<T>(&self, result: &Result<T, MailOperationError>) {
        let status = result
            .as_ref()
            .map(|_| MailOperationStatus::Succeeded)
            .unwrap_or_else(|error| error.status());
        self.status.store(status as u8, Ordering::Release);
    }

    pub(crate) fn run<T, F>(
        &self,
        operation: MailOperationKind,
        work: F,
    ) -> Result<T, MailOperationError>
    where
        F: FnOnce(&MailOperationContext) -> Result<T, MailOperationError>,
    {
        self.begin(operation)?;
        let result = work(self);
        self.finish(&result);
        result
    }
}

#[derive(Debug)]
pub(crate) struct PreparedMailOperation {
    command: Option<MailCommand>,
    key: MailOperationKey,
    in_flight: Arc<Mutex<HashSet<MailOperationKey>>>,
    context: MailOperationContext,
}

impl PreparedMailOperation {
    pub(crate) fn cancellation(&self) -> MailOperationCancellation {
        self.context.cancellation()
    }

    pub(crate) fn context(&self) -> MailOperationContext {
        self.context.clone()
    }

    /// Run this prepared operation from a worker. Cancellation requested
    /// before backend work starts prevents the mutation. Once the backend has
    /// started, a successful result remains successful even if cancellation
    /// was requested meanwhile, so the caller can reconcile visible state
    /// instead of creating a phantom failure.
    pub(crate) fn run(
        self,
        operations: &MailOperationsService,
    ) -> Result<CompletedMailOperation, MailOperationError> {
        self.run_with(|operation| operations.apply_with_result(operation))
    }

    fn run_with<F>(mut self, apply: F) -> Result<CompletedMailOperation, MailOperationError>
    where
        F: FnOnce(MailOperation) -> Result<Option<MailBackendLocation>, MailOperationError>,
    {
        let mut command = self
            .command
            .take()
            .expect("a prepared mail operation can only run once");
        let operation = command.operation_kind();
        let result = self.context.run(operation, |context| {
            context.cancellation().check(operation)?;
            apply(command.forward.clone())
        })?;
        command.update_inverse_from_result(result);
        Ok(CompletedMailOperation {
            command,
            cancellation_requested: self.context.is_cancelled(),
        })
    }
}

impl Drop for PreparedMailOperation {
    fn drop(&mut self) {
        if self.context.status() == MailOperationStatus::Queued {
            self.context.cancellation.cancel();
            self.context
                .status
                .store(MailOperationStatus::Cancelled as u8, Ordering::Release);
        }
        self.in_flight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.key);
    }
}

#[derive(Debug)]
pub(crate) struct CompletedMailOperation {
    command: MailCommand,
    cancellation_requested: bool,
}

impl CompletedMailOperation {
    pub(crate) fn cancellation_requested(&self) -> bool {
        self.cancellation_requested
    }

    /// Commit history only after a worker reported confirmed backend success.
    pub(crate) fn commit(self, history: &mut MailCommandStack) {
        history.record_applied(self.command);
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
            .unwrap_or_else(|| crate::services::app_data_dir().join("mail-undo.json"));
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

    fn record_applied(&mut self, command: MailCommand) {
        self.undo.push(command);
        self.redo.clear();
        self.trim_history();
        self.persist();
    }

    fn apply_command_with_cancellation(
        operations: &MailOperationsService,
        mut command: MailCommand,
        cancellation: &MailOperationCancellation,
    ) -> Result<MailCommand, MailOperationError> {
        let operation = command.operation_kind();
        cancellation.check(operation)?;
        let result = operations.apply_with_result(command.forward.clone())?;
        command.update_inverse_from_result(result);
        Ok(command)
    }

    fn rollback_applied(
        operations: &MailOperationsService,
        applied: &[MailCommand],
        context: &MailOperationContext,
        operation: MailOperationKind,
        original: &MailOperationError,
    ) -> Result<(), MailOperationError> {
        if applied.is_empty() {
            return Ok(());
        }

        // Rollback is a data-integrity action and deliberately ignores the
        // cancellation flag. Stopping halfway through rollback would leave a
        // batch in a less predictable state than the original failure.
        context.mark_rolling_back();
        let mut rollback_errors = Vec::new();
        for previous in applied.iter().rev() {
            if let Err(error) = operations.apply_with_result(previous.inverse.clone()) {
                tracing::error!(%error, "Could not roll back a partially applied mail batch");
                rollback_errors.push(error.to_string());
            }
        }

        if rollback_errors.is_empty() {
            Ok(())
        } else {
            Err(MailOperationError::RollbackFailed {
                operation,
                message: format!(
                    "Ursprünglicher Fehler: {original}; Rückabwicklung: {}",
                    rollback_errors.join("; ")
                ),
            })
        }
    }

    pub(crate) fn execute(
        &mut self,
        operations: &MailOperationsService,
        command: MailCommand,
    ) -> Result<(), MailOperationError> {
        let context = MailOperationContext::new();
        self.execute_with_context(operations, command, &context)
    }

    pub(crate) fn execute_with_cancellation(
        &mut self,
        operations: &MailOperationsService,
        command: MailCommand,
        cancellation: &MailOperationCancellation,
    ) -> Result<(), MailOperationError> {
        let context = MailOperationContext::with_cancellation(cancellation.clone());
        self.execute_with_context(operations, command, &context)
    }

    pub(crate) fn execute_with_context(
        &mut self,
        operations: &MailOperationsService,
        command: MailCommand,
        context: &MailOperationContext,
    ) -> Result<(), MailOperationError> {
        let operation = command.operation_kind();
        context.run(operation, |context| {
            let command =
                Self::apply_command_with_cancellation(operations, command, &context.cancellation)?;
            self.record_applied(command);
            Ok(())
        })
    }

    pub(crate) fn execute_batch(
        &mut self,
        operations: &MailOperationsService,
        commands: Vec<MailCommand>,
    ) -> Result<usize, MailOperationError> {
        if commands.is_empty() {
            return Ok(0);
        }
        let context = MailOperationContext::new();
        self.execute_batch_with_context(operations, commands, &context)
    }

    pub(crate) fn execute_batch_with_cancellation(
        &mut self,
        operations: &MailOperationsService,
        commands: Vec<MailCommand>,
        cancellation: &MailOperationCancellation,
    ) -> Result<usize, MailOperationError> {
        if commands.is_empty() {
            return Ok(0);
        }
        let context = MailOperationContext::with_cancellation(cancellation.clone());
        self.execute_batch_with_context(operations, commands, &context)
    }

    pub(crate) fn execute_batch_with_context(
        &mut self,
        operations: &MailOperationsService,
        commands: Vec<MailCommand>,
        context: &MailOperationContext,
    ) -> Result<usize, MailOperationError> {
        let Some(first) = commands.first() else {
            return Ok(0);
        };
        let operation = first.operation_kind();
        context.run(operation, |context| {
            let mut applied: Vec<MailCommand> = Vec::with_capacity(commands.len());
            for command in commands {
                let command = match Self::apply_command_with_cancellation(
                    operations,
                    command,
                    &context.cancellation,
                ) {
                    Ok(command) => command,
                    Err(error) => {
                        if applied.is_empty() {
                            return Err(error);
                        }
                        return match Self::rollback_applied(
                            operations, &applied, context, operation, &error,
                        ) {
                            Ok(()) => Err(error),
                            Err(rollback_error) => Err(rollback_error),
                        };
                    }
                };
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
        })
    }

    pub(crate) fn undo(
        &mut self,
        operations: &MailOperationsService,
    ) -> Result<Option<MailCommand>, MailOperationError> {
        let Some(command) = self.undo.last() else {
            return Ok(None);
        };
        let context = MailOperationContext::new();
        self.undo_with_context(operations, &context, command.operation_kind())
    }

    pub(crate) fn undo_with_cancellation(
        &mut self,
        operations: &MailOperationsService,
        cancellation: &MailOperationCancellation,
    ) -> Result<Option<MailCommand>, MailOperationError> {
        let Some(command) = self.undo.last() else {
            return Ok(None);
        };
        let context = MailOperationContext::with_cancellation(cancellation.clone());
        self.undo_with_context(operations, &context, command.operation_kind())
    }

    pub(crate) fn undo_with_context(
        &mut self,
        operations: &MailOperationsService,
        context: &MailOperationContext,
        operation: MailOperationKind,
    ) -> Result<Option<MailCommand>, MailOperationError> {
        context.run(operation, |context| {
            context.cancellation.check(operation)?;
            let command = self
                .undo
                .pop()
                .expect("undo entry was checked before starting");
            if let Err(error) = operations.apply_with_result(command.inverse.clone()) {
                self.undo.push(command);
                return Err(error);
            }
            let result = command.clone();
            self.redo.push(command);
            self.trim_history();
            self.persist();
            Ok(Some(result))
        })
    }

    pub(crate) fn redo(
        &mut self,
        operations: &MailOperationsService,
    ) -> Result<Option<MailCommand>, MailOperationError> {
        let Some(command) = self.redo.last() else {
            return Ok(None);
        };
        let context = MailOperationContext::new();
        self.redo_with_context(operations, &context, command.operation_kind())
    }

    pub(crate) fn redo_with_cancellation(
        &mut self,
        operations: &MailOperationsService,
        cancellation: &MailOperationCancellation,
    ) -> Result<Option<MailCommand>, MailOperationError> {
        let Some(command) = self.redo.last() else {
            return Ok(None);
        };
        let context = MailOperationContext::with_cancellation(cancellation.clone());
        self.redo_with_context(operations, &context, command.operation_kind())
    }

    pub(crate) fn redo_with_context(
        &mut self,
        operations: &MailOperationsService,
        context: &MailOperationContext,
        operation: MailOperationKind,
    ) -> Result<Option<MailCommand>, MailOperationError> {
        context.run(operation, |context| {
            context.cancellation.check(operation)?;
            let mut command = self
                .redo
                .pop()
                .expect("redo entry was checked before starting");
            let result = match operations.apply_with_result(command.forward.clone()) {
                Ok(result) => result,
                Err(error) => {
                    self.redo.push(command);
                    return Err(error);
                }
            };
            command.update_inverse_from_result(result);
            let result = command.clone();
            self.undo.push(command);
            self.trim_history();
            self.persist();
            Ok(Some(result))
        })
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
            MailOperation::Delete { id, location } => match location {
                Some(location) => self.apply_camel(
                    CamelMailOperation::Delete { location },
                    MailOperationKind::Delete,
                ),
                None => self
                    .store
                    .metadata_store()
                    .set_deleted(&id, true)
                    .map_err(Into::into),
            },
            MailOperation::Move {
                id,
                location,
                destination,
            } => match location {
                Some(location) => self.apply_camel(
                    CamelMailOperation::Move {
                        location,
                        destination,
                    },
                    MailOperationKind::Move,
                ),
                None => self
                    .store
                    .metadata_store()
                    .set_folder_override(&id, Some(&destination))
                    .map_err(Into::into),
            },
            MailOperation::Copy {
                id,
                location,
                destination,
            } => match location {
                Some(_) => Err(Self::unsupported_backend(
                    MailOperationKind::Copy,
                    "EDS/Camel-Copy-Backend",
                )),
                None => self
                    .store
                    .metadata_store()
                    .set_copy(&id, &destination, true)
                    .map_err(Into::into),
            },
            MailOperation::RemoveCopy {
                id,
                location,
                destination,
            } => match location {
                Some(_) => Err(Self::unsupported_backend(
                    MailOperationKind::Copy,
                    "EDS/Camel-Copy-Backend",
                )),
                None => self
                    .store
                    .metadata_store()
                    .set_copy(&id, &destination, false)
                    .map_err(Into::into),
            },
            MailOperation::Archive { id, location } => match location {
                Some(location) => self.apply_camel(
                    CamelMailOperation::Archive { location },
                    MailOperationKind::Archive,
                ),
                None => self
                    .store
                    .metadata_store()
                    .set_folder_override(&id, Some("archive"))
                    .map_err(Into::into),
            },
            MailOperation::SetJunk {
                id,
                location,
                is_junk,
            } => match location {
                Some(location) => self.apply_camel(
                    CamelMailOperation::SetJunk { location, is_junk },
                    if is_junk {
                        MailOperationKind::MarkJunk
                    } else {
                        MailOperationKind::MarkNotJunk
                    },
                ),
                None => self
                    .store
                    .metadata_store()
                    .set_folder_override(&id, is_junk.then_some("junk"))
                    .map_err(Into::into),
            },
            MailOperation::SetImportant {
                id,
                location,
                is_important,
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
                None => self
                    .store
                    .persist_important_state(&id, is_important)
                    .map_err(Into::into),
            },
            MailOperation::SetLabel {
                id,
                location,
                label,
                labeled,
            } => match location {
                Some(location) => self.apply_camel(
                    CamelMailOperation::SetLabel {
                        location,
                        label,
                        labeled,
                    },
                    MailOperationKind::SetLabel,
                ),
                None => self
                    .store
                    .metadata_store()
                    .set_label(&id, &label, labeled)
                    .map_err(Into::into),
            },
            MailOperation::RestoreDeleted { id, location } => match location {
                Some(location) => self.apply_camel(
                    CamelMailOperation::RestoreDeleted { location },
                    MailOperationKind::Delete,
                ),
                None => self
                    .store
                    .metadata_store()
                    .set_deleted(&id, false)
                    .map_err(Into::into),
            },
            MailOperation::RestoreArchived { id, location } => match location {
                Some(location) => self.apply_camel(
                    CamelMailOperation::RestoreArchived { location },
                    MailOperationKind::Archive,
                ),
                None => self
                    .store
                    .metadata_store()
                    .set_folder_override(&id, None)
                    .map_err(Into::into),
            },
        }
    }

    /// Apply a mutation after a cooperative cancellation check. The check is
    /// intentionally only before the backend call; once the backend starts,
    /// its confirmed result must be reported honestly to the caller.
    pub(crate) fn apply_with_cancellation(
        &self,
        operation: MailOperation,
        cancellation: &MailOperationCancellation,
    ) -> Result<(), MailOperationError> {
        let kind = operation.kind();
        cancellation.check(kind)?;
        self.apply(operation)
    }

    pub(crate) fn apply_with_context(
        &self,
        operation: MailOperation,
        context: &MailOperationContext,
    ) -> Result<(), MailOperationError> {
        let kind = operation.kind();
        context.run(kind, |context| {
            self.apply_with_cancellation(operation, &context.cancellation)
        })
    }

    pub(crate) fn apply_with_result(
        &self,
        operation: MailOperation,
    ) -> Result<Option<MailBackendLocation>, MailOperationError> {
        match operation {
            MailOperation::Copy {
                id: _,
                location: Some(location),
                destination,
            } => self.apply_camel_with_result(
                CamelMailOperation::Copy {
                    location,
                    destination,
                },
                MailOperationKind::Copy,
            ),
            MailOperation::RemoveCopy {
                id: _,
                location: Some(location),
                destination: _,
            } => self.apply_camel_with_result(
                CamelMailOperation::RemoveCopy { location },
                MailOperationKind::Copy,
            ),
            operation => self.apply(operation).map(|()| None),
        }
    }

    pub(crate) fn apply_with_result_and_cancellation(
        &self,
        operation: MailOperation,
        cancellation: &MailOperationCancellation,
    ) -> Result<Option<MailBackendLocation>, MailOperationError> {
        let kind = operation.kind();
        cancellation.check(kind)?;
        self.apply_with_result(operation)
    }

    pub(crate) fn apply_with_result_and_context(
        &self,
        operation: MailOperation,
        context: &MailOperationContext,
    ) -> Result<Option<MailBackendLocation>, MailOperationError> {
        let kind = operation.kind();
        context.run(kind, |context| {
            self.apply_with_result_and_cancellation(operation, &context.cancellation)
        })
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
        self.apply_camel_with_result(operation, kind).map(|_| ())
    }

    fn apply_camel_with_result(
        &self,
        operation: CamelMailOperation,
        kind: MailOperationKind,
    ) -> Result<Option<MailBackendLocation>, MailOperationError> {
        camel_mail_service::apply_operation_with_result(operation).map_err(|message| {
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
        Self::unsupported_backend(operation, "lokaler Nur-Lese-Import")
    }

    fn unsupported_backend(
        operation: MailOperationKind,
        backend: &'static str,
    ) -> MailOperationError {
        MailOperationError::Unsupported { operation, backend }
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
                is_important: false,
            })
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn local_important_command_is_persistent_and_undoable() {
        let root = temporary_directory("operations-local-important");
        let mail_dir = root.join("mail");
        fs::create_dir_all(&mail_dir).expect("mail directory should be created");
        let source = mail_dir.join("message.eml");
        fs::write(
            &source,
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
                MailCommand::set_important_local(&message_id, false, true),
            )
            .expect("local important state should persist");
        assert!(
            MailMetadataStore::new(store.metadata_path())
                .load()
                .get(&message_id)
                .is_some_and(|state| state.is_important)
        );
        assert!(source.is_file());

        history
            .undo(&operations)
            .expect("undo should succeed")
            .expect("one command should be undone");
        assert!(
            !MailMetadataStore::new(store.metadata_path())
                .load()
                .get(&message_id)
                .is_some_and(|state| state.is_important)
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn local_destructive_operations_use_only_the_app_owned_sidecar() {
        let root = temporary_directory("operations-local-sidecar");
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
            .delete(&message_id)
            .expect("local delete should persist in the sidecar");
        assert!(store.load_local_records().is_empty());
        assert!(
            MailMetadataStore::new(store.metadata_path())
                .load_deleted()
                .contains(&message_id)
        );
        assert!(mail_dir.join("message.eml").is_file());

        operations
            .apply(MailOperation::RestoreDeleted {
                id: message_id.clone(),
                location: None,
            })
            .expect("local delete should be restorable");
        assert_eq!(store.load_local_records().len(), 1);

        operations
            .move_to(&message_id, "archive")
            .expect("local move should persist in the sidecar");
        assert_eq!(store.load_local_records()[0].folder_id, "archive");
        assert!(mail_dir.join("message.eml").is_file());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn local_junk_command_can_be_undone_without_touching_the_source_file() {
        let root = temporary_directory("operations-local-junk");
        let mail_dir = root.join("mail");
        fs::create_dir_all(&mail_dir).expect("mail directory should be created");
        let source = mail_dir.join("message.eml");
        fs::write(
            &source,
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
        let original_folder = store
            .load_local_records()
            .first()
            .expect("message should still parse")
            .folder_id
            .clone();
        let operations = MailOperationsService::with_store(store.clone());
        let mut history = MailCommandStack::default();

        history
            .execute(
                &operations,
                MailCommand::set_junk_local(&message_id, &original_folder),
            )
            .expect("local junk command should succeed");
        assert_eq!(store.load_local_records()[0].folder_id, "junk");

        history
            .execute(&operations, MailCommand::clear_junk_local(&message_id))
            .expect("local not-junk command should succeed");
        assert_eq!(store.load_local_records()[0].folder_id, original_folder);

        history
            .undo(&operations)
            .expect("undo should succeed")
            .expect("not-junk command should be undone");
        assert_eq!(store.load_local_records()[0].folder_id, "junk");

        history
            .redo(&operations)
            .expect("redo should succeed")
            .expect("not-junk command should be redone");
        assert_eq!(store.load_local_records()[0].folder_id, original_folder);
        assert!(source.is_file());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn local_label_command_persists_and_undoes_through_the_sidecar() {
        let root = temporary_directory("operations-local-label");
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
                MailCommand::set_label_local(&message_id, "Work", false, true),
            )
            .expect("local label should persist");
        assert!(
            MailMetadataStore::new(store.metadata_path())
                .load_labels()
                .get(&message_id)
                .is_some_and(|labels| labels.contains("Work"))
        );

        history
            .undo(&operations)
            .expect("undo should succeed")
            .expect("label command should be undone");
        assert!(
            MailMetadataStore::new(store.metadata_path())
                .load_labels()
                .is_empty()
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn local_copy_command_persists_and_undoes_through_the_sidecar() {
        let root = temporary_directory("operations-local-copy");
        let mail_dir = root.join("mail");
        fs::create_dir_all(&mail_dir).expect("mail directory should be created");
        let source = mail_dir.join("message.eml");
        fs::write(
            &source,
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
            .execute(&operations, MailCommand::copy_local(&message_id, "archive"))
            .expect("local copy should persist");
        assert!(
            MailMetadataStore::new(store.metadata_path())
                .load_copies()
                .get(&message_id)
                .is_some_and(|folders| folders.contains("archive"))
        );
        assert!(
            store
                .load_local_records()
                .iter()
                .any(|message| message.id.contains("::copy::") && message.folder_id == "archive")
        );
        assert!(source.is_file());

        history
            .undo(&operations)
            .expect("undo should succeed")
            .expect("one command should be undone");
        assert!(
            MailMetadataStore::new(store.metadata_path())
                .load_copies()
                .get(&message_id)
                .is_none_or(|folders| !folders.contains("archive"))
        );

        history
            .redo(&operations)
            .expect("redo should succeed")
            .expect("one command should be redone");
        assert!(
            MailMetadataStore::new(store.metadata_path())
                .load_copies()
                .get(&message_id)
                .is_some_and(|folders| folders.contains("archive"))
        );

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
                is_important: false,
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
                is_important: false,
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
            message_id: None,
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
        let context = MailOperationContext::new();
        let location = MailBackendLocation {
            source_uid: "source".to_string(),
            folder_name: "INBOX".to_string(),
            message_uid: "uid".to_string(),
            message_id: None,
        };

        let result = history.execute_batch_with_context(
            &operations,
            vec![
                MailCommand::set_message_state(message_id.clone(), None, true, false, false, true),
                MailCommand::move_to("remote", location, "Archive"),
            ],
            &context,
        );
        assert!(result.is_err());
        assert_eq!(context.status(), MailOperationStatus::Failed);
        assert!(!history.can_undo());
        assert_eq!(
            MailMetadataStore::new(store.metadata_path())
                .load()
                .get(&message_id),
            Some(&MailMessageState {
                is_unread: true,
                is_flagged: false,
                is_important: false,
            })
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn remote_copy_command_replaces_inverse_with_backend_created_location() {
        let source_location = MailBackendLocation {
            source_uid: "graph-account".to_string(),
            folder_name: "Inbox".to_string(),
            message_uid: "original-id".to_string(),
            message_id: Some("<original@example.org>".to_string()),
        };
        let copied_location = MailBackendLocation {
            source_uid: "graph-account".to_string(),
            folder_name: "Archive".to_string(),
            message_uid: "new-copy-id".to_string(),
            message_id: Some("<copy@example.org>".to_string()),
        };
        let mut command = MailCommand::copy_remote("original", source_location, "Archive");

        command.update_inverse_from_result(Some(copied_location.clone()));

        assert!(matches!(
            command.inverse,
            MailOperation::RemoveCopy {
                location: Some(location),
                ..
            } if location == copied_location
        ));
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

    #[test]
    fn destructive_commands_keep_backend_restore_locations() {
        let location = MailBackendLocation {
            source_uid: "source".to_string(),
            folder_name: "INBOX".to_string(),
            message_uid: "uid".to_string(),
            message_id: Some("message@example.org".to_string()),
        };

        let delete = MailCommand::delete("delete-message", location.clone());
        assert!(matches!(
            &delete.inverse,
            MailOperation::RestoreDeleted {
                id,
                location: Some(saved_location)
            } if id == "delete-message" && saved_location == &location
        ));

        let archive = MailCommand::archive("archive-message", location.clone());
        assert!(matches!(
            &archive.inverse,
            MailOperation::RestoreArchived {
                id,
                location: Some(saved_location)
            } if id == "archive-message" && saved_location == &location
        ));
    }

    #[test]
    fn operation_context_publishes_lifecycle_and_rejects_reuse() {
        let context = MailOperationContext::new();
        assert!(context.id().get() > 0);
        assert_eq!(context.status(), MailOperationStatus::Queued);

        let result: Result<(), MailOperationError> =
            context.run(MailOperationKind::SetLabel, |running| {
                assert_eq!(running.status(), MailOperationStatus::Running);
                Ok(())
            });
        assert!(result.is_ok());
        assert_eq!(context.status(), MailOperationStatus::Succeeded);
        assert!(!context.cancel());

        let reused = context.run(MailOperationKind::SetLabel, |_| Ok(()));
        assert!(matches!(
            reused,
            Err(MailOperationError::Lifecycle {
                operation: MailOperationKind::SetLabel,
                status: MailOperationStatus::Succeeded,
            })
        ));
        assert_eq!(context.status(), MailOperationStatus::Succeeded);
    }

    #[test]
    fn operation_context_distinguishes_conflict_and_cancellation() {
        let conflict_context = MailOperationContext::new();
        let conflict: Result<(), MailOperationError> =
            conflict_context.run(MailOperationKind::Delete, |_| {
                Err(MailOperationError::Conflict {
                    operation: MailOperationKind::Delete,
                    message: "server state changed".to_string(),
                })
            });
        assert!(matches!(
            conflict,
            Err(ref error) if error.is_conflict()
        ));
        assert_eq!(conflict_context.status(), MailOperationStatus::Conflict);

        let cancelled_context = MailOperationContext::new();
        assert!(cancelled_context.cancel());
        assert_eq!(cancelled_context.status(), MailOperationStatus::Cancelled);
        let mut backend_called = false;
        let cancelled: Result<(), MailOperationError> =
            cancelled_context.run(MailOperationKind::Delete, |_| {
                backend_called = true;
                Ok(())
            });
        assert!(matches!(
            cancelled,
            Err(ref error) if error.is_cancelled()
        ));
        assert!(!backend_called);
        assert_eq!(cancelled_context.status(), MailOperationStatus::Cancelled);
    }

    #[test]
    fn cancelled_command_does_not_change_state_or_history() {
        let root = temporary_directory("operations-cancelled-command");
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
        let context = MailOperationContext::new();
        assert!(context.cancel());
        let mut history = MailCommandStack::default();

        let result = history.execute_with_context(
            &operations,
            MailCommand::set_message_state(message_id.clone(), None, true, false, false, true),
            &context,
        );

        assert!(matches!(
            result,
            Err(ref error) if error.is_cancelled()
        ));
        assert_eq!(context.status(), MailOperationStatus::Cancelled);
        assert!(!history.can_undo());
        assert_eq!(
            MailMetadataStore::new(store.metadata_path())
                .load()
                .get(&message_id),
            None
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn coordinator_serializes_mutations_for_the_same_message() {
        let coordinator = MailOperationCoordinator::default();
        let first = coordinator
            .prepare(MailCommand::set_message_state(
                "message-1",
                None,
                true,
                false,
                false,
                false,
            ))
            .expect("the first mutation should be prepared");

        let duplicate =
            coordinator.prepare(MailCommand::set_important_local("message-1", false, true));
        assert!(matches!(
            duplicate,
            Err(MailOperationError::Busy {
                operation: MailOperationKind::MarkImportant,
                ..
            })
        ));
        assert_eq!(coordinator.active_count(), 1);

        drop(first);
        assert_eq!(coordinator.active_count(), 0);
        assert!(
            coordinator
                .prepare(MailCommand::set_important_local("message-1", false, true,))
                .is_ok()
        );
    }

    #[test]
    fn coordinator_allows_independent_messages_in_parallel() {
        let coordinator = MailOperationCoordinator::default();
        let first = coordinator
            .prepare(MailCommand::delete_local("message-1"))
            .expect("first message should be prepared");
        let second = coordinator
            .prepare(MailCommand::delete_local("message-2"))
            .expect("second message should be prepared independently");

        assert_eq!(coordinator.active_count(), 2);
        drop((first, second));
        assert_eq!(coordinator.active_count(), 0);
    }

    #[test]
    fn coordinator_uses_stable_remote_message_id_across_folder_uid_changes() {
        let coordinator = MailOperationCoordinator::default();
        let first_location = MailBackendLocation {
            source_uid: "source".to_string(),
            folder_name: "INBOX".to_string(),
            message_uid: "uid-before-move".to_string(),
            message_id: Some("<message@example.org>".to_string()),
        };
        let second_location = MailBackendLocation {
            source_uid: "source".to_string(),
            folder_name: "Archive".to_string(),
            message_uid: "uid-after-move".to_string(),
            message_id: Some("<message@example.org>".to_string()),
        };
        let first = coordinator
            .prepare(MailCommand::set_message_state(
                "message",
                Some(first_location),
                true,
                false,
                false,
                true,
            ))
            .expect("first remote mutation should be prepared");

        let duplicate = coordinator.prepare(MailCommand::set_message_state(
            "message",
            Some(second_location),
            false,
            true,
            false,
            false,
        ));
        assert!(matches!(
            duplicate,
            Err(MailOperationError::Busy {
                operation: MailOperationKind::SetMessageState,
                ..
            })
        ));
        drop(first);
        assert_eq!(coordinator.active_count(), 0);
    }

    #[test]
    fn cancellation_before_worker_start_skips_the_backend_and_releases_the_key() {
        let coordinator = MailOperationCoordinator::default();
        let prepared = coordinator
            .prepare(MailCommand::delete_local("message-1"))
            .expect("operation should be prepared");
        let cancellation = prepared.cancellation();
        cancellation.cancel();
        let mut backend_called = false;

        let result = prepared.run_with(|_| {
            backend_called = true;
            Ok(None)
        });

        assert!(matches!(
            result,
            Err(MailOperationError::Cancelled {
                operation: MailOperationKind::Delete
            })
        ));
        assert!(!backend_called);
        assert_eq!(coordinator.active_count(), 0);
    }

    #[test]
    fn confirmed_success_is_committed_only_after_worker_completion() {
        let coordinator = MailOperationCoordinator::default();
        let prepared = coordinator
            .prepare(MailCommand::set_message_state(
                "message-1",
                None,
                true,
                false,
                false,
                true,
            ))
            .expect("operation should be prepared");
        let cancellation = prepared.cancellation();
        let mut history = MailCommandStack::default();

        let completed = prepared
            .run_with(|_| {
                // Once backend work has started, cancellation cannot safely
                // turn a confirmed mutation into a visible failure.
                cancellation.cancel();
                Ok(None)
            })
            .expect("confirmed backend success should remain successful");

        assert!(completed.cancellation_requested());
        assert!(!history.can_undo());
        assert_eq!(coordinator.active_count(), 0);
        completed.commit(&mut history);
        assert!(history.can_undo());
        assert!(!history.can_redo());
    }

    #[test]
    fn conflict_releases_the_key_without_creating_history() {
        let coordinator = MailOperationCoordinator::default();
        let prepared = coordinator
            .prepare(MailCommand::delete_local("message-1"))
            .expect("operation should be prepared");
        let context = prepared.context();
        let history = MailCommandStack::default();

        let result = prepared.run_with(|_| {
            Err(MailOperationError::Conflict {
                operation: MailOperationKind::Delete,
                message: "server state changed".to_string(),
            })
        });

        assert!(matches!(result, Err(MailOperationError::Conflict { .. })));
        assert_eq!(context.status(), MailOperationStatus::Conflict);
        assert_eq!(coordinator.active_count(), 0);
        assert!(!history.can_undo());
        assert!(!history.can_redo());
    }

    #[test]
    fn failed_batch_reports_when_rollback_itself_fails() {
        let root = temporary_directory("operations-rollback-failed");
        let mail_dir = root.join("mail");
        fs::create_dir_all(&mail_dir).expect("mail directory should be created");
        let store =
            MailStoreService::with_mail_dir_and_state_path(&mail_dir, root.join("state.json"));
        let operations = MailOperationsService::with_store(store.clone());
        let location = MailBackendLocation {
            source_uid: "source".to_string(),
            folder_name: "INBOX".to_string(),
            message_uid: "uid".to_string(),
            message_id: None,
        };
        let first = MailCommand {
            forward: MailOperation::SetMessageState {
                id: "message".to_string(),
                location: None,
                is_unread: false,
                is_flagged: true,
            },
            inverse: MailOperation::Move {
                id: "message".to_string(),
                location: Some(location.clone()),
                destination: "Archive".to_string(),
            },
            description: "test command".to_string(),
        };
        let second = MailCommand::move_to("remote", location, "Archive");
        let context = MailOperationContext::new();
        let mut history = MailCommandStack::default();

        let result = history.execute_batch_with_context(&operations, vec![first, second], &context);

        assert!(matches!(
            result,
            Err(MailOperationError::RollbackFailed {
                operation: MailOperationKind::SetMessageState,
                ..
            })
        ));
        assert_eq!(context.status(), MailOperationStatus::RollbackFailed);
        assert!(!history.can_undo());
        assert_eq!(
            MailMetadataStore::new(store.metadata_path())
                .load()
                .get("message")
                .map(|state| (state.is_unread, state.is_flagged)),
            Some((false, true))
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn legacy_backend_locations_load_without_message_id() {
        let location: MailBackendLocation = serde_json::from_str(
            r#"{
                "source_uid":"source",
                "folder_name":"INBOX",
                "message_uid":"uid"
            }"#,
        )
        .expect("legacy location should remain readable");

        assert_eq!(location.message_id, None);
    }

    fn temporary_directory(label: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("evolution-gtk4-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("temporary directory should be created");
        path
    }
}
