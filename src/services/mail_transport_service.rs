// SPDX-License-Identifier: GPL-3.0-or-later
//
// Framework-neutral mail transport command contract.  The controller owns
// command identity, cancellation, retry, and the explicit offline queue.  It
// deliberately knows nothing about GTK; provider work is delegated to the
// native Camel/EDS or explicit Graph adapters below and is always performed
// by a worker thread.

use super::camel_mail_service;
use super::exchange_service::graph_service_for_configured_account_worker;

use async_channel::{Receiver, Sender};
use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::thread;
use std::time::Duration;

const MAX_ATTEMPTS: usize = 3;
const RETRY_BASE_DELAY: Duration = Duration::from_millis(50);

pub(crate) const MAIL_STOP_ACTION_ID: &str = "mail-stop";
pub(crate) const WORK_OFFLINE_ACTION_ID: &str = "work-offline";
pub(crate) const WORK_ONLINE_ACTION_ID: &str = "work-online";

/// Stable command identifiers used by the GTK action graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MailTransportAction {
    SendReceive,
    ReceiveAll,
    SendAll,
}

impl MailTransportAction {
    pub(crate) const fn action_id(self) -> &'static str {
        match self {
            Self::SendReceive => "mail-send-receive",
            Self::ReceiveAll => "mail-receive-all",
            Self::SendAll => "mail-send-all",
        }
    }

    const fn includes_receive(self) -> bool {
        matches!(self, Self::SendReceive | Self::ReceiveAll)
    }

    const fn includes_send(self) -> bool {
        matches!(self, Self::SendReceive | Self::SendAll)
    }

    const fn label(self) -> &'static str {
        match self {
            Self::SendReceive => "Send and Receive",
            Self::ReceiveAll => "Receive All Accounts",
            Self::SendAll => "Send All Queued Messages",
        }
    }
}

/// Whether the transport may start new provider work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MailTransportMode {
    Online,
    Offline,
}

/// Stable lifecycle state exposed to the UI and portable tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MailTransportState {
    Idle,
    Running,
    Retrying,
    CancelRequested,
    Succeeded,
    Failed,
    Cancelled,
    Offline,
}

impl MailTransportState {
    const fn is_active(self) -> bool {
        matches!(self, Self::Running | Self::Retrying | Self::CancelRequested)
    }
}

/// Coarse transport phase.  Provider-specific progress remains in `detail`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MailTransportPhase {
    Idle,
    Preparing,
    Sending,
    Receiving,
    Retrying,
    Cancelling,
    Completed,
    Failed,
    Offline,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MailTransportProgress {
    pub(crate) phase: MailTransportPhase,
    pub(crate) completed: usize,
    pub(crate) total: Option<usize>,
    pub(crate) attempt: usize,
    pub(crate) max_attempts: usize,
    pub(crate) detail: Option<String>,
}

impl Default for MailTransportProgress {
    fn default() -> Self {
        Self {
            phase: MailTransportPhase::Idle,
            completed: 0,
            total: None,
            attempt: 1,
            max_attempts: MAX_ATTEMPTS,
            detail: None,
        }
    }
}

/// Errors are typed enough for retry and cancellation decisions while keeping
/// provider-specific secrets out of the controller.  `Permanent` is used for
/// sanitized provider messages that cannot be retried automatically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MailTransportError {
    Cancelled,
    Retryable(String),
    Authentication(String),
    Unavailable(String),
    Permanent(String),
}

impl MailTransportError {
    const fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }

    const fn is_retryable(&self) -> bool {
        matches!(self, Self::Retryable(_))
    }
}

impl fmt::Display for MailTransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("Transport was cancelled"),
            Self::Retryable(message) => formatter.write_str(message),
            Self::Authentication(message) => formatter.write_str(message),
            Self::Unavailable(message) => formatter.write_str(message),
            Self::Permanent(message) => formatter.write_str(message),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MailTransportStartError {
    AlreadyRunning,
    Offline,
    NoQueuedMessages,
}

impl fmt::Display for MailTransportStartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyRunning => formatter.write_str("A mail transport is already running"),
            Self::Offline => {
                formatter.write_str("Offline mode: no new mail transport started")
            }
            Self::NoQueuedMessages => formatter.write_str("No queued messages"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MailTransportActionGraph {
    pub(crate) mail_send_receive: bool,
    pub(crate) mail_receive_all: bool,
    pub(crate) mail_send_all: bool,
    pub(crate) mail_stop: bool,
    pub(crate) work_offline: bool,
    pub(crate) work_online: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MailTransportSnapshot {
    pub(crate) generation: u64,
    pub(crate) mode: MailTransportMode,
    pub(crate) state: MailTransportState,
    pub(crate) action: Option<MailTransportAction>,
    pub(crate) phase: MailTransportPhase,
    pub(crate) progress: MailTransportProgress,
    pub(crate) error: Option<MailTransportError>,
    pub(crate) queued_items: usize,
    pub(crate) cancellable: bool,
    pub(crate) message: Option<String>,
    pub(crate) actions: MailTransportActionGraph,
}

impl MailTransportSnapshot {
    pub(crate) fn status_text(&self) -> String {
        if self.mode == MailTransportMode::Offline && !self.state.is_active() {
            return match self.queued_items {
                0 => "Offline".to_string(),
                count => format!("Offline · {count} message(s) in local queue"),
            };
        }

        if let Some(message) = self
            .message
            .as_deref()
            .filter(|message| !message.is_empty())
        {
            match self.state {
                MailTransportState::Idle
                | MailTransportState::Succeeded
                | MailTransportState::Failed
                | MailTransportState::Cancelled => return message.to_string(),
                _ => {}
            }
        }

        if self.state == MailTransportState::CancelRequested {
            return "Cancelling …".to_string();
        }

        if matches!(
            self.state,
            MailTransportState::Running | MailTransportState::Retrying
        ) {
            if let Some(detail) = self
                .progress
                .detail
                .as_deref()
                .filter(|detail| !detail.is_empty())
            {
                return detail.to_string();
            }
        }

        match self.state {
            MailTransportState::Idle => "Ready".to_string(),
            MailTransportState::Running => {
                format!(
                    "{} in progress …",
                    self.action
                        .map_or("Mail transport", |action| action.label())
                )
            }
            MailTransportState::Retrying => format!(
                "Temporary error · retrying {}/{} …",
                self.progress.attempt, self.progress.max_attempts
            ),
            MailTransportState::CancelRequested => "Cancelling …".to_string(),
            MailTransportState::Succeeded => "Mail transport completed".to_string(),
            MailTransportState::Failed => self
                .error
                .as_ref()
                .map(|error| format!("Error: {error}"))
                .unwrap_or_else(|| "Mail transport failed".to_string()),
            MailTransportState::Cancelled => "Mail transport cancelled".to_string(),
            MailTransportState::Offline => "Offline".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MailTransportEvent {
    pub(crate) generation: u64,
    pub(crate) snapshot: MailTransportSnapshot,
}

pub(crate) struct MailTransportOperation {
    pub(crate) generation: u64,
    pub(crate) events: Receiver<MailTransportEvent>,
}

#[derive(Clone)]
pub(crate) struct MailTransportMessage {
    pub(crate) id: String,
    pub(crate) source_uid: String,
    pub(crate) raw_message: Arc<[u8]>,
}

impl fmt::Debug for MailTransportMessage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MailTransportMessage")
            .field("id", &self.id)
            .field("source_uid", &self.source_uid)
            .field("raw_message_len", &self.raw_message.len())
            .finish()
    }
}

pub(crate) struct MailTransportContext {
    pub(crate) generation: u64,
    pub(crate) cancellation: Arc<AtomicBool>,
}

impl MailTransportContext {
    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancellation.load(Ordering::Acquire)
    }
}

/// Provider boundary used by the controller.  Test backends can deterministically
/// exercise all lifecycle paths; the production implementation below performs
/// only real Camel/EDS or explicit Graph work.
pub(crate) trait MailTransportBackend: Send + Sync {
    fn execute(
        &self,
        action: MailTransportAction,
        queue: &[MailTransportMessage],
        context: &MailTransportContext,
        progress: &mut dyn FnMut(MailTransportProgress),
        confirmed: &mut dyn FnMut(&str),
    ) -> Result<(), MailTransportError>;
}

struct ActiveOperation {
    generation: u64,
    cancellation: Arc<AtomicBool>,
    sender: Sender<MailTransportEvent>,
}

struct TransportInner {
    mode: MailTransportMode,
    generation: u64,
    state: MailTransportState,
    action: Option<MailTransportAction>,
    phase: MailTransportPhase,
    progress: MailTransportProgress,
    error: Option<MailTransportError>,
    message: Option<String>,
    queue: VecDeque<MailTransportMessage>,
    active: Option<ActiveOperation>,
}

impl Default for TransportInner {
    fn default() -> Self {
        Self {
            mode: MailTransportMode::Online,
            generation: 0,
            state: MailTransportState::Idle,
            action: None,
            phase: MailTransportPhase::Idle,
            progress: MailTransportProgress::default(),
            error: None,
            message: None,
            queue: VecDeque::new(),
            active: None,
        }
    }
}

#[derive(Clone)]
pub(crate) struct MailTransportService {
    inner: Arc<Mutex<TransportInner>>,
    backend: Arc<dyn MailTransportBackend>,
}

impl fmt::Debug for MailTransportService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MailTransportService")
            .field("snapshot", &self.snapshot())
            .finish_non_exhaustive()
    }
}

static SHARED_SERVICE: OnceLock<MailTransportService> = OnceLock::new();

impl Default for MailTransportService {
    fn default() -> Self {
        Self::shared()
    }
}

impl MailTransportService {
    pub(crate) fn shared() -> Self {
        SHARED_SERVICE
            .get_or_init(|| Self::with_backend(Arc::new(NativeMailTransportBackend)))
            .clone()
    }

    pub(crate) fn with_backend(backend: Arc<dyn MailTransportBackend>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(TransportInner::default())),
            backend,
        }
    }

    pub(crate) fn snapshot(&self) -> MailTransportSnapshot {
        let inner = self.lock_inner();
        snapshot_from_inner(&inner)
    }

    #[allow(dead_code)]
    pub(crate) fn action_graph(&self) -> MailTransportActionGraph {
        self.snapshot().actions
    }

    /// Queue an explicit RFC822 payload.  Drafts are not implicitly treated as
    /// outbox items.  A repeated source/payload pair is deduplicated so a
    /// failed offline send can be retried without creating duplicate mail.
    pub(crate) fn enqueue_message(
        &self,
        id: &str,
        source_uid: &str,
        raw_message: &[u8],
    ) -> Result<String, String> {
        let id = id.trim();
        let source_uid = source_uid.trim();
        if id.is_empty() {
            return Err("Transport ID must not be empty".to_string());
        }
        if source_uid.is_empty() {
            return Err("A transport message requires an account ID".to_string());
        }
        if raw_message.is_empty() {
            return Err("The RFC822 message is empty".to_string());
        }

        let mut inner = self.lock_inner();
        if let Some(existing) = inner.queue.iter().find(|message| {
            message.source_uid == source_uid && message.raw_message.as_ref() == raw_message
        }) {
            return Ok(existing.id.clone());
        }
        if inner.queue.iter().any(|message| message.id == id) {
            return Err(format!(
                "Transport ID {id} is already queued for another message"
            ));
        }
        inner.queue.push_back(MailTransportMessage {
            id: id.to_string(),
            source_uid: source_uid.to_string(),
            raw_message: Arc::<[u8]>::from(raw_message.to_vec()),
        });
        if inner.mode == MailTransportMode::Offline {
            inner.message = Some("Message queued in local transport queue".to_string());
        }
        Ok(id.to_string())
    }

    #[allow(dead_code)]
    pub(crate) fn queued_count(&self) -> usize {
        self.lock_inner().queue.len()
    }

    /// Start the FIFO offline queue after the mode has become online.  The
    /// caller owns the returned event stream; returning `None` is deliberate
    /// when another operation is still winding down or the queue is empty.
    pub(crate) fn start_queued_if_online(&self) -> Option<MailTransportOperation> {
        let snapshot = self.snapshot();
        if snapshot.mode != MailTransportMode::Online
            || snapshot.queued_items == 0
            || matches!(
                snapshot.state,
                MailTransportState::Running
                    | MailTransportState::Retrying
                    | MailTransportState::CancelRequested
            )
        {
            return None;
        }
        self.start(MailTransportAction::SendAll).ok()
    }

    pub(crate) fn set_mode(&self, mode: MailTransportMode) -> MailTransportSnapshot {
        let (event, sender) = {
            let mut inner = self.lock_inner();
            if inner.mode == mode {
                return snapshot_from_inner(&inner);
            }

            inner.mode = mode;
            inner.generation = next_generation(inner.generation);
            let active = inner.active.as_ref().map(|active| {
                active.cancellation.store(true, Ordering::Release);
                active.sender.clone()
            });

            if let Some(sender) = active {
                if mode == MailTransportMode::Offline {
                    inner.state = MailTransportState::Offline;
                    inner.phase = MailTransportPhase::Offline;
                    inner.action = None;
                    inner.error = None;
                    inner.message =
                        Some("Offline mode active; stopping active transport".to_string());
                } else {
                    inner.state = MailTransportState::CancelRequested;
                    inner.phase = MailTransportPhase::Cancelling;
                    inner.message =
                        Some("Online mode waiting for active cancellation".to_string());
                }
                let snapshot = snapshot_from_inner(&inner);
                (
                    Some(MailTransportEvent {
                        // This is a new mode generation, not a stale worker
                        // result.  Let the current GTK watcher apply it even
                        // though it is delivered through the old operation's
                        // channel.
                        generation: snapshot.generation,
                        snapshot,
                    }),
                    Some(sender),
                )
            } else {
                inner.state = if mode == MailTransportMode::Offline {
                    MailTransportState::Offline
                } else {
                    MailTransportState::Idle
                };
                inner.phase = if mode == MailTransportMode::Offline {
                    MailTransportPhase::Offline
                } else {
                    MailTransportPhase::Idle
                };
                inner.action = None;
                inner.error = None;
                inner.message = Some(if mode == MailTransportMode::Offline {
                    "Offline mode active".to_string()
                } else {
                    "Online mode active".to_string()
                });
                (None, None)
            }
        };

        if let (Some(event), Some(sender)) = (event, sender) {
            let _ = sender.try_send(event);
        }
        self.snapshot()
    }

    pub(crate) fn start(
        &self,
        action: MailTransportAction,
    ) -> Result<MailTransportOperation, MailTransportStartError> {
        let (generation, cancellation, sender, receiver, initial) = {
            let mut inner = self.lock_inner();
            if inner.mode == MailTransportMode::Offline {
                inner.message =
                    Some("Offline mode: transport remains in local queue".to_string());
                return Err(MailTransportStartError::Offline);
            }
            if inner.active.is_some() {
                return Err(MailTransportStartError::AlreadyRunning);
            }
            if action == MailTransportAction::SendAll && inner.queue.is_empty() {
                inner.state = MailTransportState::Idle;
                inner.phase = MailTransportPhase::Idle;
                inner.action = None;
                inner.error = None;
                inner.message = Some("No queued messages".to_string());
                return Err(MailTransportStartError::NoQueuedMessages);
            }

            let generation = next_generation(inner.generation);
            let cancellation = Arc::new(AtomicBool::new(false));
            // Provider progress must never be able to block the provider
            // worker or cancellation path on a slow/destroyed GTK watcher.
            // The receiver is owned by one watcher and is dropped with it;
            // senders then fail fast rather than joining or touching GTK.
            let (sender, receiver) = async_channel::unbounded();
            inner.generation = generation;
            inner.state = MailTransportState::Running;
            inner.action = Some(action);
            inner.phase = MailTransportPhase::Preparing;
            inner.progress = MailTransportProgress {
                phase: MailTransportPhase::Preparing,
                total: (action == MailTransportAction::SendAll)
                    .then_some(inner.queue.len())
                    .or_else(|| action.includes_send().then_some(inner.queue.len())),
                max_attempts: MAX_ATTEMPTS,
                ..MailTransportProgress::default()
            };
            inner.error = None;
            inner.message = Some(format!("Preparing {} …", action.label()));
            inner.active = Some(ActiveOperation {
                generation,
                cancellation: cancellation.clone(),
                sender: sender.clone(),
            });
            let initial = snapshot_from_inner(&inner);
            (generation, cancellation, sender, receiver, initial)
        };

        let _ = sender.try_send(MailTransportEvent {
            generation,
            snapshot: initial,
        });

        let service = self.clone();
        let worker_sender = sender.clone();
        thread::spawn(move || {
            run_operation(service, action, generation, cancellation, worker_sender);
        });

        Ok(MailTransportOperation {
            generation,
            events: receiver,
        })
    }

    pub(crate) fn stop(&self) -> bool {
        let event = {
            let mut inner = self.lock_inner();
            let Some(active) = inner.active.as_ref() else {
                return false;
            };
            if active.cancellation.swap(true, Ordering::AcqRel) {
                return false;
            }
            let generation = active.generation;
            let sender = active.sender.clone();
            inner.state = MailTransportState::CancelRequested;
            inner.phase = MailTransportPhase::Cancelling;
            inner.message = Some("Cancellation requested …".to_string());
            Some((generation, sender, snapshot_from_inner(&inner)))
        };
        if let Some((generation, sender, snapshot)) = event {
            let _ = sender.try_send(MailTransportEvent {
                generation,
                snapshot,
            });
            true
        } else {
            false
        }
    }

    fn lock_inner(&self) -> std::sync::MutexGuard<'_, TransportInner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn queued_messages(&self) -> Vec<MailTransportMessage> {
        self.lock_inner().queue.iter().cloned().collect()
    }

    fn confirm_sent(&self, generation: u64, id: &str) {
        let event = {
            let mut inner = self.lock_inner();
            let old_len = inner.queue.len();
            inner.queue.retain(|message| message.id != id);
            if old_len == inner.queue.len() {
                return;
            }
            inner.active.as_ref().and_then(|active| {
                (active.generation == generation)
                    .then(|| (active.sender.clone(), snapshot_from_inner(&inner)))
            })
        };
        if let Some((sender, snapshot)) = event {
            let _ = sender.try_send(MailTransportEvent {
                // Provider confirmation is a real queue mutation even when
                // the worker became stale; publish the current generation so
                // the UI can immediately reflect the reduced queue.
                generation: snapshot.generation,
                snapshot,
            });
        }
    }

    fn publish_progress(
        &self,
        generation: u64,
        sender: &Sender<MailTransportEvent>,
        mut progress: MailTransportProgress,
    ) {
        let snapshot = {
            let mut inner = self.lock_inner();
            if inner
                .active
                .as_ref()
                .is_some_and(|active| active.generation == generation)
                && inner.generation == generation
                && inner.mode == MailTransportMode::Online
                && !inner
                    .active
                    .as_ref()
                    .is_some_and(|active| active.cancellation.load(Ordering::Acquire))
            {
                progress.max_attempts = MAX_ATTEMPTS;
                inner.progress = progress.clone();
                inner.phase = progress.phase;
                inner.state = if progress.phase == MailTransportPhase::Retrying {
                    MailTransportState::Retrying
                } else {
                    MailTransportState::Running
                };
                inner.message = progress.detail.clone();
                inner.error = None;
                Some(snapshot_from_inner(&inner))
            } else {
                None
            }
        };
        if let Some(snapshot) = snapshot {
            let _ = sender.try_send(MailTransportEvent {
                generation,
                snapshot,
            });
        }
    }

    fn publish_retry(
        &self,
        generation: u64,
        sender: &Sender<MailTransportEvent>,
        error: &MailTransportError,
        attempt: usize,
    ) {
        let snapshot = {
            let mut inner = self.lock_inner();
            if inner
                .active
                .as_ref()
                .is_some_and(|active| active.generation == generation)
                && inner.generation == generation
                && inner.mode == MailTransportMode::Online
                && !inner
                    .active
                    .as_ref()
                    .is_some_and(|active| active.cancellation.load(Ordering::Acquire))
            {
                inner.state = MailTransportState::Retrying;
                inner.phase = MailTransportPhase::Retrying;
                inner.progress = MailTransportProgress {
                    phase: MailTransportPhase::Retrying,
                    attempt,
                    max_attempts: MAX_ATTEMPTS,
                    detail: Some(error.to_string()),
                    ..inner.progress.clone()
                };
                inner.error = Some(error.clone());
                inner.message = Some(error.to_string());
                Some(snapshot_from_inner(&inner))
            } else {
                None
            }
        };
        if let Some(snapshot) = snapshot {
            let _ = sender.try_send(MailTransportEvent {
                generation,
                snapshot,
            });
        }
    }

    fn finish(
        &self,
        action: MailTransportAction,
        generation: u64,
        sender: &Sender<MailTransportEvent>,
        result: Result<(), MailTransportError>,
    ) {
        let snapshot = {
            let mut inner = self.lock_inner();
            let owns_operation = inner
                .active
                .as_ref()
                .is_some_and(|active| active.generation == generation);
            if owns_operation {
                let cancellation_requested = inner
                    .active
                    .as_ref()
                    .is_some_and(|active| active.cancellation.load(Ordering::Acquire));
                inner.active = None;
                inner.action = None;
                let final_phase = match result.as_ref() {
                    Ok(()) if cancellation_requested => MailTransportPhase::Idle,
                    Ok(()) => MailTransportPhase::Completed,
                    Err(error) if error.is_cancelled() => MailTransportPhase::Idle,
                    Err(_) => MailTransportPhase::Failed,
                };
                inner.progress = MailTransportProgress {
                    phase: final_phase,
                    completed: inner.progress.completed,
                    total: inner.progress.total,
                    attempt: inner.progress.attempt,
                    max_attempts: inner.progress.max_attempts,
                    detail: None,
                };

                if inner.mode == MailTransportMode::Offline {
                    inner.state = MailTransportState::Offline;
                    inner.phase = MailTransportPhase::Offline;
                    inner.error = None;
                    inner.message = Some("Offline mode active".to_string());
                } else if inner.generation != generation {
                    // An offline/online transition invalidated this result.
                    // The provider may finish eventually, but it cannot alter
                    // the newer UI generation.
                    inner.state = MailTransportState::Cancelled;
                    inner.phase = MailTransportPhase::Idle;
                    inner.error = None;
                    inner.message = Some("Stale transport result discarded".to_string());
                } else {
                    match result {
                        Ok(()) if cancellation_requested => {
                            inner.state = MailTransportState::Cancelled;
                            inner.phase = MailTransportPhase::Idle;
                            inner.error = None;
                            inner.message = Some("Mail transport cancelled".to_string());
                        }
                        Ok(()) => {
                            inner.state = MailTransportState::Succeeded;
                            inner.phase = MailTransportPhase::Completed;
                            inner.error = None;
                            inner.message = Some(match action {
                                MailTransportAction::SendReceive => {
                                    "Send and Receive completed".to_string()
                                }
                                MailTransportAction::ReceiveAll => {
                                    "Receive All Accounts completed".to_string()
                                }
                                MailTransportAction::SendAll => {
                                    "Queued messages sent".to_string()
                                }
                            });
                        }
                        Err(error) if error.is_cancelled() => {
                            inner.state = MailTransportState::Cancelled;
                            inner.phase = MailTransportPhase::Idle;
                            inner.error = None;
                            inner.message = Some("Mail transport cancelled".to_string());
                        }
                        Err(error) => {
                            inner.state = MailTransportState::Failed;
                            inner.phase = MailTransportPhase::Failed;
                            inner.message = Some(error.to_string());
                            inner.error = Some(error);
                        }
                    }
                }
            }
            snapshot_from_inner(&inner)
        };
        let _ = sender.send_blocking(MailTransportEvent {
            // A stale worker can only publish the current snapshot here.  Its
            // provider result is discarded, but the terminal state must still
            // unblock the GTK action graph after an Offline -> Online bounce.
            generation: snapshot.generation,
            snapshot,
        });
    }
}

fn next_generation(current: u64) -> u64 {
    current.wrapping_add(1).max(1)
}

fn snapshot_from_inner(inner: &TransportInner) -> MailTransportSnapshot {
    let cancellable = inner
        .active
        .as_ref()
        .is_some_and(|active| !active.cancellation.load(Ordering::Acquire));
    let active = inner.active.is_some();
    let online = inner.mode == MailTransportMode::Online;
    let actions = MailTransportActionGraph {
        mail_send_receive: online && !active,
        mail_receive_all: online && !active,
        mail_send_all: online && !active && !inner.queue.is_empty(),
        mail_stop: active && cancellable,
        work_offline: online,
        work_online: !online,
    };
    MailTransportSnapshot {
        generation: inner.generation,
        mode: inner.mode,
        state: inner.state,
        action: inner.action,
        phase: inner.phase,
        progress: inner.progress.clone(),
        error: inner.error.clone(),
        queued_items: inner.queue.len(),
        cancellable,
        message: inner.message.clone(),
        actions,
    }
}

fn run_operation(
    service: MailTransportService,
    action: MailTransportAction,
    generation: u64,
    cancellation: Arc<AtomicBool>,
    sender: Sender<MailTransportEvent>,
) {
    let mut confirmed = |id: &str| {
        service.confirm_sent(generation, id);
    };

    let result = (|| {
        let mut retry_error = None;
        for attempt in 1..=MAX_ATTEMPTS {
            if cancellation.load(Ordering::Acquire) {
                return Err(MailTransportError::Cancelled);
            }
            if attempt > 1 {
                let error = retry_error
                    .take()
                    .unwrap_or_else(|| MailTransportError::Retryable("Retrying".to_string()));
                service.publish_retry(generation, &sender, &error, attempt);
                let delay = RETRY_BASE_DELAY
                    .checked_mul(attempt as u32 - 1)
                    .unwrap_or(RETRY_BASE_DELAY);
                let mut waited = Duration::ZERO;
                while waited < delay {
                    if cancellation.load(Ordering::Acquire) {
                        return Err(MailTransportError::Cancelled);
                    }
                    let slice = (delay - waited).min(Duration::from_millis(10));
                    thread::sleep(slice);
                    waited += slice;
                }
            }

            let queue = service.queued_messages();
            let context = MailTransportContext {
                generation,
                cancellation: cancellation.clone(),
            };
            let mut progress = |mut value: MailTransportProgress| {
                value.attempt = value.attempt.max(attempt);
                value.max_attempts = value.max_attempts.max(MAX_ATTEMPTS);
                service.publish_progress(generation, &sender, value);
            };
            match service
                .backend
                .execute(action, &queue, &context, &mut progress, &mut confirmed)
            {
                Ok(()) if cancellation.load(Ordering::Acquire) => {
                    return Err(MailTransportError::Cancelled);
                }
                Ok(()) => return Ok(()),
                Err(error) if error.is_cancelled() || cancellation.load(Ordering::Acquire) => {
                    return Err(MailTransportError::Cancelled);
                }
                Err(error) if error.is_retryable() && attempt < MAX_ATTEMPTS => {
                    retry_error = Some(error);
                }
                Err(error) => return Err(error),
            }
        }
        Err(MailTransportError::Permanent(
            "Mail transport finished without a final result".to_string(),
        ))
    })();

    service.finish(action, generation, &sender, result);
}

#[derive(Debug, Default)]
struct NativeMailTransportBackend;

impl MailTransportBackend for NativeMailTransportBackend {
    fn execute(
        &self,
        action: MailTransportAction,
        queue: &[MailTransportMessage],
        context: &MailTransportContext,
        progress: &mut dyn FnMut(MailTransportProgress),
        confirmed: &mut dyn FnMut(&str),
    ) -> Result<(), MailTransportError> {
        tracing::trace!(
            generation = context.generation,
            action = action.action_id(),
            "mail transport provider operation started"
        );
        let mut first_error = None;

        if action.includes_send() {
            if let Err(error) = send_queued(queue, context, progress, confirmed) {
                if error.is_cancelled() {
                    return Err(error);
                }
                first_error = Some(error);
            }
        }

        if action.includes_receive() {
            if context.is_cancelled() {
                return Err(MailTransportError::Cancelled);
            }
            if let Err(error) = receive_all(context, progress) {
                if error.is_cancelled() {
                    return Err(error);
                }
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }

        if context.is_cancelled() {
            return Err(MailTransportError::Cancelled);
        }
        first_error.map_or(Ok(()), Err)
    }
}

fn send_queued(
    queue: &[MailTransportMessage],
    context: &MailTransportContext,
    progress: &mut dyn FnMut(MailTransportProgress),
    confirmed: &mut dyn FnMut(&str),
) -> Result<(), MailTransportError> {
    let total = queue.len();
    progress(MailTransportProgress {
        phase: MailTransportPhase::Sending,
        total: Some(total),
        detail: Some(if total == 0 {
            "No queued messages".to_string()
        } else {
            format!("{} queued message(s)", total)
        }),
        ..MailTransportProgress::default()
    });
    for (index, message) in queue.iter().enumerate() {
        if context.is_cancelled() {
            return Err(MailTransportError::Cancelled);
        }
        progress(MailTransportProgress {
            phase: MailTransportPhase::Sending,
            completed: index,
            total: Some(total),
            detail: Some(format!("Sending message {}/{}", index + 1, total)),
            ..MailTransportProgress::default()
        });
        let _sent_folder_copy_saved = camel_mail_service::send_message_with_cancellation(
            &message.source_uid,
            &message.raw_message,
            Some(context.cancellation.clone()),
        )
        .map_err(classify_provider_error)?;
        // `Result::Ok` is the provider confirmation.  The returned boolean
        // only says whether a Sent-folder copy was saved (EDS may legitimately
        // return false there); it must never decide whether the message was
        // sent.  Only now is it safe to remove this queue item.
        confirmed(&message.id);
        progress(MailTransportProgress {
            phase: MailTransportPhase::Sending,
            completed: index + 1,
            total: Some(total),
            detail: Some(format!("Sent message {}/{}", index + 1, total)),
            ..MailTransportProgress::default()
        });
    }
    Ok(())
}

fn receive_all(
    context: &MailTransportContext,
    progress: &mut dyn FnMut(MailTransportProgress),
) -> Result<(), MailTransportError> {
    if context.is_cancelled() {
        return Err(MailTransportError::Cancelled);
    }
    progress(MailTransportProgress {
        phase: MailTransportPhase::Receiving,
        detail: Some("Fetching new messages from all accounts …".to_string()),
        ..MailTransportProgress::default()
    });

    let camel_result = load_camel_for_transport(context, progress);

    if context.is_cancelled() {
        return Err(MailTransportError::Cancelled);
    }

    match camel_result {
        Ok(load) if load.failed_sources > 0 => {
            let warning = load.warning.unwrap_or_else(|| {
                format!(
                    "Could not synchronize {}/{} mail account(s)",
                    load.failed_sources, load.configured_sources
                )
            });
            Err(classify_provider_error(warning))
        }
        Ok(load) => {
            progress(MailTransportProgress {
                phase: MailTransportPhase::Receiving,
                completed: load.successful_sources,
                total: Some(load.configured_sources.max(1)),
                detail: Some("Receive All Accounts completed".to_string()),
                ..MailTransportProgress::default()
            });
            Ok(())
        }
        Err(camel_error) => {
            // Graph is an explicit provider fallback only when an enabled
            // account and process-local access token are available.  There is
            // no local fake transport path.
            if graph_service_for_configured_account_worker().is_some() {
                progress(MailTransportProgress {
                    phase: MailTransportPhase::Receiving,
                    detail: Some("Using Microsoft Graph as provider …".to_string()),
                    ..MailTransportProgress::default()
                });
                let cursors = std::collections::HashMap::new();
                let graph_result = graph_transport_load_result(
                    camel_mail_service::load_messages_graph(
                        None,
                        None,
                        0,
                        camel_mail_service::default_page_limit(),
                        &cursors,
                        &context.cancellation,
                    ),
                    &camel_error,
                );
                if context.is_cancelled() {
                    return Err(MailTransportError::Cancelled);
                }
                return match graph_result {
                    Ok(load) => {
                        progress(MailTransportProgress {
                            phase: MailTransportPhase::Receiving,
                            completed: load.successful_sources,
                            total: Some(load.configured_sources.max(1)),
                            detail: Some("Receive All Accounts completed".to_string()),
                            ..MailTransportProgress::default()
                        });
                        Ok(())
                    }
                    Err(error) => Err(error),
                };
            }
            Err(classify_provider_error(camel_error))
        }
    }
}

fn graph_transport_load_result(
    result: Result<camel_mail_service::CamelMailLoad, String>,
    camel_error: &str,
) -> Result<camel_mail_service::CamelMailLoad, MailTransportError> {
    match result {
        Ok(load) if load.failed_sources > 0 => {
            let warning = load.warning.unwrap_or_else(|| {
                format!(
                    "Could not synchronize {}/{} Graph mail account(s)",
                    load.failed_sources, load.configured_sources
                )
            });
            Err(classify_provider_error(format!(
                "{camel_error}; Graph: {warning}"
            )))
        }
        Ok(load) => Ok(load),
        Err(graph_error) => {
            let graph_error = classify_provider_error(graph_error);
            Err(match graph_error {
                MailTransportError::Retryable(message) => MailTransportError::Retryable(format!(
                    "Mail account synchronization unavailable ({camel_error}); Graph: {message}"
                )),
                MailTransportError::Authentication(message) => {
                    MailTransportError::Authentication(format!(
                        "Mail account synchronization unavailable ({camel_error}); Graph: {message}"
                    ))
                }
                MailTransportError::Unavailable(message) => {
                    MailTransportError::Unavailable(format!(
                        "Mail account synchronization unavailable ({camel_error}); Graph: {message}"
                    ))
                }
                MailTransportError::Permanent(message) => MailTransportError::Permanent(format!(
                    "Mail account synchronization unavailable ({camel_error}); Graph: {message}"
                )),
                MailTransportError::Cancelled => MailTransportError::Cancelled,
            })
        }
    }
}

/// Keep Camel's synchronous provider call off the transport controller's
/// progress path.  The controller worker drains progress while Camel runs,
/// so the bounded UI event channel cannot turn a long receive into a silent
/// no-progress operation or a blocked provider callback.
fn load_camel_for_transport(
    context: &MailTransportContext,
    progress: &mut dyn FnMut(MailTransportProgress),
) -> Result<camel_mail_service::CamelMailLoad, String> {
    let (progress_sender, progress_receiver) = async_channel::unbounded();
    let (result_sender, result_receiver) = mpsc::sync_channel(1);
    let cancellation = context.cancellation.clone();
    let worker = thread::spawn(move || {
        let result = camel_mail_service::load_messages_page_with_progress_and_cancellation(
            None,
            None,
            0,
            camel_mail_service::default_page_limit(),
            Some(&cancellation),
            Some(&progress_sender),
        );
        drop(progress_sender);
        let _ = result_sender.send(result);
    });

    let mut result = None;
    loop {
        while let Ok(camel_progress) = progress_receiver.try_recv() {
            progress(camel_progress_to_transport(&camel_progress));
        }

        match result_receiver.recv_timeout(Duration::from_millis(10)) {
            Ok(received) => {
                result = Some(received);
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    let worker_finished = worker.join().is_ok();
    while let Ok(camel_progress) = progress_receiver.try_recv() {
        progress(camel_progress_to_transport(&camel_progress));
    }

    result.unwrap_or_else(|| {
        Err(if worker_finished {
            "Camel mail worker returned no result".to_string()
        } else {
            "Camel mail worker terminated unexpectedly".to_string()
        })
    })
}

fn camel_progress_to_transport(
    progress: &camel_mail_service::CamelSyncProgress,
) -> MailTransportProgress {
    let phase = match progress.phase {
        camel_mail_service::CamelSyncPhase::Retrying => MailTransportPhase::Retrying,
        camel_mail_service::CamelSyncPhase::Failed => MailTransportPhase::Failed,
        _ => MailTransportPhase::Receiving,
    };
    let detail = progress
        .folder_name
        .as_deref()
        .or(progress.source_display_name.as_deref())
        .map(|name| format!("Receiving {name} …"));
    MailTransportProgress {
        phase,
        completed: progress.processed_sources,
        total: (progress.configured_sources > 0).then_some(progress.configured_sources),
        attempt: progress.attempt,
        max_attempts: progress.max_attempts.max(1),
        detail,
    }
}

fn classify_provider_error(error: String) -> MailTransportError {
    let lower = error.to_ascii_lowercase();
    let has_status_code = |wanted: &str| {
        lower
            .split(|character: char| !character.is_ascii_digit())
            .any(|token| token == wanted)
    };
    if lower.contains("token")
        || lower.contains("auth")
        || lower.contains("anmeld")
        || lower.contains("zugriff")
        || has_status_code("401")
        || has_status_code("403")
    {
        MailTransportError::Authentication(error)
    } else if lower.contains("timeout")
        || lower.contains("timed out")
        || lower.contains("tempor")
        || lower.contains("vorübergehend")
        || lower.contains("netzwerk")
        || lower.contains("network")
        || lower.contains("verbindung")
        || lower.contains("connection")
        || ["408", "425", "429", "500", "502", "503", "504"]
            .iter()
            .any(|status| has_status_code(status))
    {
        MailTransportError::Retryable(error)
    } else if lower.contains("not available")
        || lower.contains("nicht verfügbar")
        || lower.contains("nicht verf")
    {
        MailTransportError::Unavailable(error)
    } else {
        MailTransportError::Permanent(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[derive(Clone)]
    struct ScriptedBackend {
        outcomes: Arc<Mutex<VecDeque<Result<(), MailTransportError>>>>,
        calls: Arc<AtomicUsize>,
        confirmed_ids: Arc<Mutex<Vec<String>>>,
        wait_for_cancel: bool,
    }

    impl ScriptedBackend {
        fn new(outcomes: impl IntoIterator<Item = Result<(), MailTransportError>>) -> Self {
            Self {
                outcomes: Arc::new(Mutex::new(outcomes.into_iter().collect())),
                calls: Arc::new(AtomicUsize::new(0)),
                confirmed_ids: Arc::new(Mutex::new(Vec::new())),
                wait_for_cancel: false,
            }
        }

        fn cancellable() -> Self {
            Self {
                outcomes: Arc::new(Mutex::new(VecDeque::new())),
                calls: Arc::new(AtomicUsize::new(0)),
                confirmed_ids: Arc::new(Mutex::new(Vec::new())),
                wait_for_cancel: true,
            }
        }
    }

    impl MailTransportBackend for ScriptedBackend {
        fn execute(
            &self,
            action: MailTransportAction,
            queue: &[MailTransportMessage],
            context: &MailTransportContext,
            progress: &mut dyn FnMut(MailTransportProgress),
            confirmed: &mut dyn FnMut(&str),
        ) -> Result<(), MailTransportError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let phase = if action.includes_send() {
                MailTransportPhase::Sending
            } else {
                MailTransportPhase::Receiving
            };
            progress(MailTransportProgress {
                phase,
                completed: 0,
                total: Some(1),
                detail: Some("Test-Transport".to_string()),
                ..MailTransportProgress::default()
            });
            if self.wait_for_cancel {
                while !context.is_cancelled() {
                    thread::sleep(Duration::from_millis(5));
                }
                return Err(MailTransportError::Cancelled);
            }
            if let Some(result) = self
                .outcomes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .pop_front()
            {
                if result.is_ok() && action.includes_send() {
                    for message in queue {
                        self.confirmed_ids
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .push(message.id.clone());
                        confirmed(&message.id);
                    }
                    progress(MailTransportProgress {
                        phase,
                        completed: usize::from(!queue.is_empty()),
                        total: Some(1),
                        detail: Some("Test-Transport abgeschlossen".to_string()),
                        ..MailTransportProgress::default()
                    });
                }
                result
            } else {
                Ok(())
            }
        }
    }

    struct DeferredCancelBackend {
        release: Arc<AtomicBool>,
        started: Arc<AtomicBool>,
    }

    impl MailTransportBackend for DeferredCancelBackend {
        fn execute(
            &self,
            _action: MailTransportAction,
            _queue: &[MailTransportMessage],
            context: &MailTransportContext,
            _progress: &mut dyn FnMut(MailTransportProgress),
            _confirmed: &mut dyn FnMut(&str),
        ) -> Result<(), MailTransportError> {
            self.started.store(true, Ordering::Release);
            while !context.is_cancelled() {
                thread::sleep(Duration::from_millis(1));
            }
            while !self.release.load(Ordering::Acquire) {
                thread::sleep(Duration::from_millis(1));
            }
            Err(MailTransportError::Cancelled)
        }
    }

    fn wait_for_terminal(
        service: &MailTransportService,
        operation: MailTransportOperation,
    ) -> Vec<MailTransportEvent> {
        let mut events = Vec::new();
        while let Ok(event) = operation.events.recv_blocking() {
            let terminal = !event.snapshot.state.is_active() && event.snapshot.action.is_none();
            events.push(event);
            if terminal {
                break;
            }
        }
        assert!(!service.snapshot().state.is_active());
        events
    }

    #[test]
    fn action_graph_covers_online_offline_running_and_queue_states() {
        assert_eq!(
            MailTransportAction::SendReceive.action_id(),
            "mail-send-receive"
        );
        assert_eq!(
            MailTransportAction::ReceiveAll.action_id(),
            "mail-receive-all"
        );
        assert_eq!(MailTransportAction::SendAll.action_id(), "mail-send-all");
        assert_eq!(MAIL_STOP_ACTION_ID, "mail-stop");
        assert_eq!(WORK_OFFLINE_ACTION_ID, "work-offline");
        assert_eq!(WORK_ONLINE_ACTION_ID, "work-online");
        assert!(MailTransportAction::SendReceive.includes_send());
        assert!(MailTransportAction::SendReceive.includes_receive());
        assert!(!MailTransportAction::ReceiveAll.includes_send());
        assert!(MailTransportAction::ReceiveAll.includes_receive());
        assert!(MailTransportAction::SendAll.includes_send());
        assert!(!MailTransportAction::SendAll.includes_receive());

        let service = MailTransportService::with_backend(Arc::new(ScriptedBackend::cancellable()));
        let graph = service.action_graph();
        assert!(graph.mail_send_receive);
        assert!(graph.mail_receive_all);
        assert!(!graph.mail_send_all);
        assert!(!graph.mail_stop);
        assert!(graph.work_offline);
        assert!(!graph.work_online);

        service
            .enqueue_message("queued", "account", b"From: a@example.org\r\n\r\nbody")
            .unwrap();
        assert!(service.action_graph().mail_send_all);

        let operation = service.start(MailTransportAction::ReceiveAll).unwrap();
        while service.snapshot().state != MailTransportState::Running {
            thread::sleep(Duration::from_millis(2));
        }
        assert!(!service.action_graph().mail_send_receive);
        assert!(!service.action_graph().mail_receive_all);
        assert!(!service.action_graph().mail_send_all);
        assert!(service.action_graph().mail_stop);
        assert!(service.stop());
        drop(wait_for_terminal(&service, operation));

        service.set_mode(MailTransportMode::Offline);
        let graph = service.action_graph();
        assert!(!graph.mail_send_receive);
        assert!(!graph.mail_receive_all);
        assert!(!graph.mail_send_all);
        assert!(!graph.mail_stop);
        assert!(!graph.work_offline);
        assert!(graph.work_online);
    }

    #[test]
    fn queue_rejects_duplicate_id_for_a_different_payload() {
        let service = MailTransportService::with_backend(Arc::new(ScriptedBackend::new([])));
        service
            .enqueue_message("same-id", "account", b"From: a@example.org\r\n\r\nfirst")
            .unwrap();
        assert!(
            service
                .enqueue_message("same-id", "account", b"From: a@example.org\r\n\r\nsecond")
                .is_err()
        );
        assert_eq!(service.queued_count(), 1);
    }

    #[test]
    fn queue_preserves_fifo_order_and_deduplicates_same_source_payload() {
        let backend = ScriptedBackend::new([Ok(())]);
        let confirmed_ids = backend.confirmed_ids.clone();
        let service = MailTransportService::with_backend(Arc::new(backend));
        let first = b"From: a@example.org\r\n\r\nfirst";
        let second = b"From: a@example.org\r\n\r\nsecond";

        assert_eq!(
            service.enqueue_message("first", "account", first),
            Ok("first".to_string())
        );
        assert_eq!(
            service.enqueue_message("duplicate", "account", first),
            Ok("first".to_string())
        );
        service
            .enqueue_message("second", "account", second)
            .unwrap();

        drop(wait_for_terminal(
            &service,
            service.start(MailTransportAction::SendAll).unwrap(),
        ));
        assert_eq!(
            *confirmed_ids
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec!["first".to_string(), "second".to_string()]
        );
        assert_eq!(service.queued_count(), 0);
    }

    #[test]
    fn successful_transition_confirms_queue_only_after_backend_success() {
        let service = MailTransportService::with_backend(Arc::new(ScriptedBackend::new([Ok(())])));
        service
            .enqueue_message("queued", "account", b"From: a@example.org\r\n\r\nbody")
            .unwrap();
        let events = wait_for_terminal(
            &service,
            service.start(MailTransportAction::SendAll).unwrap(),
        );
        assert_eq!(service.queued_count(), 0);
        assert_eq!(service.snapshot().state, MailTransportState::Succeeded);
        assert!(events.iter().any(|event| {
            event.snapshot.phase == MailTransportPhase::Sending
                && event.snapshot.progress.completed == 1
        }));
    }

    #[test]
    fn retryable_error_emits_retry_state_and_succeeds_on_next_attempt() {
        let backend = ScriptedBackend::new([
            Err(MailTransportError::Retryable("temporär".to_string())),
            Ok(()),
        ]);
        let calls = backend.calls.clone();
        let service = MailTransportService::with_backend(Arc::new(backend));
        let events = wait_for_terminal(
            &service,
            service.start(MailTransportAction::ReceiveAll).unwrap(),
        );
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        assert_eq!(service.snapshot().state, MailTransportState::Succeeded);
        assert!(events.iter().any(|event| {
            event.snapshot.state == MailTransportState::Retrying
                && event.snapshot.progress.attempt == 2
        }));
    }

    #[test]
    fn provider_error_classification_distinguishes_auth_retry_and_unavailable() {
        assert!(matches!(
            classify_provider_error("Microsoft-Graph-Netzwerkfehler: timeout".to_string()),
            MailTransportError::Retryable(_)
        ));
        assert!(matches!(
            classify_provider_error("Microsoft-Graph-Fehler 401".to_string()),
            MailTransportError::Authentication(_)
        ));
        assert!(matches!(
            classify_provider_error("native EDS/Camel adapter is not available".to_string()),
            MailTransportError::Unavailable(_)
        ));
    }

    #[test]
    fn graph_partial_load_is_not_reported_as_success() {
        let partial = camel_mail_service::CamelMailLoad {
            messages: Vec::new(),
            folders: Vec::new(),
            configured_sources: 2,
            successful_sources: 1,
            failed_sources: 1,
            warning: Some("HTTP 503 beim Graph-Ordner".to_string()),
            is_graph: true,
            search_query: None,
            graph_search_next_cursor: None,
            graph_next_links: std::collections::HashMap::new(),
        };
        assert!(matches!(
            graph_transport_load_result(Ok(partial), "Camel unavailable"),
            Err(MailTransportError::Retryable(_))
        ));

        let complete = camel_mail_service::CamelMailLoad {
            messages: Vec::new(),
            folders: Vec::new(),
            configured_sources: 1,
            successful_sources: 1,
            failed_sources: 0,
            warning: None,
            is_graph: true,
            search_query: None,
            graph_search_next_cursor: None,
            graph_next_links: std::collections::HashMap::new(),
        };
        assert!(graph_transport_load_result(Ok(complete), "Camel unavailable").is_ok());
    }

    #[test]
    fn permanent_error_is_visible_and_manual_retry_is_allowed() {
        let backend = ScriptedBackend::new([
            Err(MailTransportError::Permanent("provider down".to_string())),
            Ok(()),
        ]);
        let confirmed_ids = backend.confirmed_ids.clone();
        let service = MailTransportService::with_backend(Arc::new(backend));
        service
            .enqueue_message("queued", "account", b"From: a@example.org\r\n\r\nbody")
            .unwrap();
        drop(wait_for_terminal(
            &service,
            service.start(MailTransportAction::SendAll).unwrap(),
        ));
        assert_eq!(service.snapshot().state, MailTransportState::Failed);
        assert_eq!(service.queued_count(), 1);
        assert_eq!(
            service.snapshot().error.as_ref().unwrap().to_string(),
            "provider down"
        );
        drop(wait_for_terminal(
            &service,
            service.start(MailTransportAction::SendAll).unwrap(),
        ));
        assert_eq!(service.snapshot().state, MailTransportState::Succeeded);
        assert_eq!(service.queued_count(), 0);
        assert_eq!(
            *confirmed_ids
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec!["queued".to_string()]
        );
    }

    #[test]
    fn offline_queue_does_not_start_provider_and_survives_mode_switch() {
        let backend = ScriptedBackend::new([Ok(())]);
        let calls = backend.calls.clone();
        let service = MailTransportService::with_backend(Arc::new(backend));
        service.set_mode(MailTransportMode::Offline);
        service
            .enqueue_message("queued", "account", b"From: a@example.org\r\n\r\nbody")
            .unwrap();
        assert!(matches!(
            service.start(MailTransportAction::SendAll),
            Err(MailTransportStartError::Offline)
        ));
        assert_eq!(service.queued_count(), 1);
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        service.set_mode(MailTransportMode::Online);
        drop(wait_for_terminal(
            &service,
            service.start_queued_if_online().unwrap(),
        ));
        assert_eq!(service.queued_count(), 0);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn stop_requests_cooperative_cancel_and_keeps_unsent_queue() {
        let service = MailTransportService::with_backend(Arc::new(ScriptedBackend::cancellable()));
        service
            .enqueue_message("queued", "account", b"From: a@example.org\r\n\r\nbody")
            .unwrap();
        let operation = service.start(MailTransportAction::SendAll).unwrap();
        while service.snapshot().state != MailTransportState::Running {
            thread::sleep(Duration::from_millis(2));
        }
        assert!(service.stop());
        assert!(matches!(
            service.snapshot().state,
            MailTransportState::CancelRequested | MailTransportState::Cancelled
        ));
        assert!(!service.action_graph().mail_stop);
        drop(wait_for_terminal(&service, operation));
        assert_eq!(service.snapshot().state, MailTransportState::Cancelled);
        assert_eq!(service.queued_count(), 1);
        assert!(!service.stop());
    }

    #[test]
    fn stale_generation_cannot_replace_newer_offline_state() {
        let service = MailTransportService::with_backend(Arc::new(ScriptedBackend::cancellable()));
        let operation = service.start(MailTransportAction::ReceiveAll).unwrap();
        let old_generation = operation.generation;
        service.set_mode(MailTransportMode::Offline);
        let newer = service.snapshot();
        assert!(newer.generation > old_generation);
        assert_eq!(newer.state, MailTransportState::Offline);
        drop(wait_for_terminal(&service, operation));
        let after = service.snapshot();
        assert_eq!(after.generation, newer.generation);
        assert_eq!(after.state, MailTransportState::Offline);
    }

    #[test]
    fn stale_progress_and_retry_cannot_mutate_newer_mode_generation() {
        let release = Arc::new(AtomicBool::new(false));
        let started = Arc::new(AtomicBool::new(false));
        let service = MailTransportService::with_backend(Arc::new(DeferredCancelBackend {
            release: release.clone(),
            started: started.clone(),
        }));
        let operation = service.start(MailTransportAction::ReceiveAll).unwrap();
        while !started.load(Ordering::Acquire) {
            thread::yield_now();
        }
        let old_generation = operation.generation;
        service.set_mode(MailTransportMode::Offline);
        let before = service.snapshot();
        let (sender, _receiver) = async_channel::bounded(4);

        service.publish_progress(
            old_generation,
            &sender,
            MailTransportProgress {
                phase: MailTransportPhase::Receiving,
                detail: Some("veralteter Fortschritt".to_string()),
                ..MailTransportProgress::default()
            },
        );
        service.publish_retry(
            old_generation,
            &sender,
            &MailTransportError::Retryable("veralteter Retry".to_string()),
            2,
        );
        assert_eq!(service.snapshot(), before);

        release.store(true, Ordering::Release);
        drop(wait_for_terminal(&service, operation));
        assert_eq!(service.snapshot().state, MailTransportState::Offline);
    }

    #[test]
    fn stale_finish_publishes_current_generation_after_mode_bounce() {
        let release = Arc::new(AtomicBool::new(false));
        let started = Arc::new(AtomicBool::new(false));
        let backend = DeferredCancelBackend {
            release: release.clone(),
            started: started.clone(),
        };
        let service = MailTransportService::with_backend(Arc::new(backend));
        let operation = service.start(MailTransportAction::ReceiveAll).unwrap();
        while !started.load(Ordering::Acquire) {
            thread::yield_now();
        }
        let old_generation = operation.generation;

        service.set_mode(MailTransportMode::Offline);
        service.set_mode(MailTransportMode::Online);
        let current_generation = service.snapshot().generation;
        assert!(current_generation > old_generation);

        release.store(true, Ordering::Release);
        let mut saw_current_terminal = false;
        while let Ok(event) = operation.events.recv_blocking() {
            if event.generation == current_generation
                && event.snapshot.generation == current_generation
                && event.snapshot.state == MailTransportState::Cancelled
            {
                saw_current_terminal = true;
                break;
            }
        }

        assert!(saw_current_terminal);
        let snapshot = service.snapshot();
        assert_eq!(snapshot.generation, current_generation);
        assert_eq!(snapshot.state, MailTransportState::Cancelled);
    }

    #[test]
    fn send_all_without_queue_is_no_work_not_success() {
        let service = MailTransportService::with_backend(Arc::new(ScriptedBackend::new([])));
        assert!(matches!(
            service.start(MailTransportAction::SendAll),
            Err(MailTransportStartError::NoQueuedMessages)
        ));
        let snapshot = service.snapshot();
        assert_eq!(snapshot.state, MailTransportState::Idle);
        assert_ne!(snapshot.state, MailTransportState::Succeeded);
        assert_eq!(snapshot.queued_items, 0);
    }
}
