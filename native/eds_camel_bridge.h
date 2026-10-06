/* SPDX-License-Identifier: GPL-3.0-or-later */

#ifndef MERIDIAN_EDS_CAMEL_BRIDGE_H
#define MERIDIAN_EDS_CAMEL_BRIDGE_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct {
    char *source_uid;
    char *folder_name;
    char *folder_id;
    char *message_uid;
    uint8_t *raw_message;
    size_t raw_message_len;
    int is_unread;
    int is_flagged;
    /* Evolution's follow-up state is stored as Camel user tags, not as a
     * provider-specific message flag.  0=None, 1=FollowUp, 2=Completed. */
    int follow_up_status;
    char *follow_up;
    char *completed_on;
    char *due_by;
    int is_replied;
    int is_forwarded;
} MeridianEdsCamelMessage;

typedef struct {
    char *source_uid;
    char *source_display_name;
    char *folder_name;
    char *folder_id;
    char *display_name;
    uint32_t unread_count;
    uint32_t total_count;
    int is_selectable;
} MeridianEdsCamelFolder;

/*
 * A subscription-list snapshot for one exact Camel store.  The entries are
 * owned by the caller and must be released with
 * meridian_eds_camel_free_subscription_folders().  `parent_folder_name` is
 * NULL for a root entry; `is_subscribed` reflects Camel's authoritative
 * CAMEL_FOLDER_SUBSCRIBED flag from a SUBSCRIPTION_LIST refresh.
 */
typedef struct {
    char *source_uid;
    char *source_display_name;
    char *folder_name;
    char *parent_folder_name;
    char *display_name;
    uint32_t unread_count;
    uint32_t total_count;
    int is_selectable;
    int is_subscribed;
} MeridianEdsCamelSubscriptionFolder;

typedef enum {
    MERIDIAN_EDS_CAMEL_SET_MESSAGE_STATE = 1,
    MERIDIAN_EDS_CAMEL_DELETE_MESSAGE = 2,
    MERIDIAN_EDS_CAMEL_MOVE_MESSAGE = 3,
    MERIDIAN_EDS_CAMEL_ARCHIVE_MESSAGE = 4,
    MERIDIAN_EDS_CAMEL_SET_JUNK = 5,
    MERIDIAN_EDS_CAMEL_SET_IMPORTANT = 6,
    MERIDIAN_EDS_CAMEL_RESTORE_DELETED = 7,
    MERIDIAN_EDS_CAMEL_RESTORE_ARCHIVED = 8,
    MERIDIAN_EDS_CAMEL_SET_FOLLOW_UP = 9,
} MeridianEdsCamelOperationKind;

typedef struct {
    MeridianEdsCamelOperationKind kind;
    const char *source_uid;
    const char *folder_name;
    const char *message_uid;
    const char *message_id;
    const char *destination_folder;
    int is_unread;
    int is_flagged;
    int is_junk;
    int is_important;
    int follow_up_status;
    const char *due_by;
} MeridianEdsCamelOperation;

typedef enum {
    MERIDIAN_EDS_CAMEL_SAVE_DRAFT = 1,
    MERIDIAN_EDS_CAMEL_SAVE_SENT = 2,
} MeridianEdsCamelSaveFolder;

typedef enum {
    MERIDIAN_EDS_CAMEL_SYNC_DISCOVERING = 1,
    MERIDIAN_EDS_CAMEL_SYNC_CONNECTING = 2,
    MERIDIAN_EDS_CAMEL_SYNC_LOADING_FOLDERS = 3,
    MERIDIAN_EDS_CAMEL_SYNC_LOADING_MESSAGES = 4,
    MERIDIAN_EDS_CAMEL_SYNC_SOURCE_COMPLETE = 5,
    MERIDIAN_EDS_CAMEL_SYNC_COMPLETE = 6,
    MERIDIAN_EDS_CAMEL_SYNC_FAILED = 7,
} MeridianEdsCamelSyncPhase;

typedef struct {
    MeridianEdsCamelSyncPhase phase;
    const char *source_uid;
    const char *source_display_name;
    const char *folder_name;
    size_t configured_sources;
    size_t processed_sources;
    size_t successful_sources;
    size_t failed_sources;
    size_t folders_seen;
    size_t messages_loaded;
} MeridianEdsCamelSyncProgress;

typedef void (*MeridianEdsCamelProgressCallback) (
    const MeridianEdsCamelSyncProgress *progress,
    void *user_data);

typedef int (*MeridianEdsCamelCancellationCallback) (
    void *user_data);

typedef enum {
    MERIDIAN_EDS_CAMEL_CHANGE = 1,
    MERIDIAN_EDS_CAMEL_FOLDER_DELETED = 2,
    MERIDIAN_EDS_CAMEL_FOLDER_RENAMED = 3,
    MERIDIAN_EDS_CAMEL_SOURCE_ADDED = 4,
    MERIDIAN_EDS_CAMEL_SOURCE_REMOVED = 5,
    MERIDIAN_EDS_CAMEL_SOURCE_ENABLED = 6,
    MERIDIAN_EDS_CAMEL_SOURCE_DISABLED = 7,
    MERIDIAN_EDS_CAMEL_SOURCE_CHANGED = 8,
} MeridianEdsCamelChangeKind;

typedef struct {
    MeridianEdsCamelChangeKind kind;
    char *source_uid;
    char *folder_name;
    char *old_folder_name;
    char *source_display_name;
    size_t added_count;
    size_t removed_count;
    size_t changed_count;
    size_t recent_count;
    char **added_uids;
    size_t added_uid_count;
    char **removed_uids;
    size_t removed_uid_count;
    char **changed_uids;
    size_t changed_uid_count;
    char **recent_uids;
    size_t recent_uid_count;
} MeridianEdsCamelChangeEvent;

typedef struct _MeridianEdsCamelWatcher MeridianEdsCamelWatcher;

/*
 * Load messages from the enabled EDS mail-account sources.  The returned
 * records contain owned RFC 822 bytes and must be released with
 * meridian_eds_camel_free_messages().
 *
 * source_uid may be NULL to load every enabled mail-account source.
 * folder_name may be NULL to load every selectable folder for the selected
 * source(s); otherwise only that exact Camel folder receives message data.
 * max_messages_per_folder <= 0 means the implementation default.
 */
int meridian_eds_camel_load_messages(
    const char *source_uid,
    const char *folder_name,
    int message_offset,
    int max_messages_per_folder,
    MeridianEdsCamelMessage **out_messages,
    size_t *out_count,
    MeridianEdsCamelFolder **out_folders,
    size_t *out_folder_count,
    size_t *out_configured_sources,
    size_t *out_successful_sources,
    size_t *out_failed_sources,
    char **out_error,
    MeridianEdsCamelProgressCallback progress_callback,
    void *progress_user_data,
    MeridianEdsCamelCancellationCallback cancellation_callback,
    void *cancellation_user_data);

/* Load only the UIDs reported by CamelFolder::changed.  This keeps signal
 * handling incremental while retaining the normal RFC-822 parsing path. */
int meridian_eds_camel_load_messages_by_uid(
    const char *source_uid,
    const char *folder_name,
    const char * const *message_uids,
    size_t message_uid_count,
    MeridianEdsCamelMessage **out_messages,
    size_t *out_count,
    char **out_error,
    MeridianEdsCamelCancellationCallback cancellation_callback,
    void *cancellation_user_data);

void meridian_eds_camel_free_messages(
    MeridianEdsCamelMessage *messages,
    size_t count);

void meridian_eds_camel_free_folders(
    MeridianEdsCamelFolder *folders,
    size_t count);

/*
 * Inspect one enabled EDS mail-account source.  On success,
 * `out_is_subscribable` reports whether the exact Camel store implements
 * CamelSubscribable.  Unsupported stores return success with zero snapshots;
 * source lookup, connection, list and cancellation failures return 0 with an
 * error.  The list is recursive and never includes virtual folders.
 */
int meridian_eds_camel_load_subscription_folders(
    const char *source_uid,
    MeridianEdsCamelSubscriptionFolder **out_folders,
    size_t *out_folder_count,
    int *out_is_subscribable,
    char **out_error,
    MeridianEdsCamelCancellationCallback cancellation_callback,
    void *cancellation_user_data);

void meridian_eds_camel_free_subscription_folders(
    MeridianEdsCamelSubscriptionFolder *folders,
    size_t count);

/*
 * Set one exact selectable folder's subscription state.  The operation is
 * rejected for disabled, unavailable and non-CamelSubscribable sources.  A
 * successful return means a fresh SUBSCRIPTION_LIST snapshot confirmed the
 * requested state; it is never an optimistic acknowledgement.
 */
int meridian_eds_camel_set_folder_subscription(
    const char *source_uid,
    const char *folder_name,
    int subscribed,
    char **out_error,
    MeridianEdsCamelCancellationCallback cancellation_callback,
    void *cancellation_user_data);

/* Folder-menu operations are intentionally exact-source and exact-folder.
 * `target_name` is a single child/display segment for CREATE_CHILD and
 * RENAME; it must not contain '/'.  Camel provides no atomic folder copy or
 * move primitive, so those operation kinds are kept explicit and fail
 * closed rather than attempting an unsafe recursive transfer. */
typedef enum {
    MERIDIAN_EDS_CAMEL_FOLDER_CREATE_CHILD = 1,
    MERIDIAN_EDS_CAMEL_FOLDER_RENAME = 2,
    MERIDIAN_EDS_CAMEL_FOLDER_DELETE = 3,
    MERIDIAN_EDS_CAMEL_FOLDER_REFRESH = 4,
    MERIDIAN_EDS_CAMEL_FOLDER_COPY = 5,
    MERIDIAN_EDS_CAMEL_FOLDER_MOVE = 6,
} MeridianEdsCamelFolderMutationKind;

typedef struct {
    int can_create_child;
    int can_rename;
    int can_delete;
    int can_refresh;
    int can_copy;
    int can_move;
} MeridianEdsCamelFolderMutationCapabilities;

/* Query the provider-derived action sensitivity for one exact existing
 * folder.  A valid but protected, non-selectable or unsupported folder
 * returns success with the respective capability set to zero. */
int meridian_eds_camel_get_folder_mutation_capabilities(
    const char *source_uid,
    const char *folder_name,
    MeridianEdsCamelFolderMutationCapabilities *out_capabilities,
    char **out_error,
    MeridianEdsCamelCancellationCallback cancellation_callback,
    void *cancellation_user_data);

/* Execute a direct Camel folder operation.  CREATE_CHILD, RENAME and DELETE
 * require CAMEL_STORE_CAN_EDIT_FOLDERS and reject account roots, NOSELECT,
 * protected/system/virtual/read-only folders and unsupported providers.
 * REFRESH requires a selectable non-virtual folder.  Every success is
 * followed by a recursive provider REFRESH that verifies the expected folder
 * state. */
int meridian_eds_camel_apply_folder_mutation(
    MeridianEdsCamelFolderMutationKind kind,
    const char *source_uid,
    const char *folder_name,
    const char *target_name,
    char **out_error,
    MeridianEdsCamelCancellationCallback cancellation_callback,
    void *cancellation_user_data);

int meridian_eds_camel_apply_operation(
    const MeridianEdsCamelOperation *operation,
    char **out_error,
    MeridianEdsCamelCancellationCallback cancellation_callback,
    void *cancellation_user_data);

/* Send a complete RFC822 message through the EDS-configured transport linked
 * to the selected mail-account source.  Returns 1 on success and 0 on an
 * ordinary backend error.  out_sent_message_saved is set when the transport
 * also saved a copy in the configured Sent folder. */
int meridian_eds_camel_send_message(
    const char *source_uid,
    const uint8_t *raw_message,
    size_t raw_message_len,
    int *out_sent_message_saved,
    char **out_error);

/* Append a complete RFC822 message to the EDS-configured Drafts or Sent
 * folder for the selected mail-account source. */
int meridian_eds_camel_save_message(
    const char *source_uid,
    const uint8_t *raw_message,
    size_t raw_message_len,
    MeridianEdsCamelSaveFolder folder,
    char **out_error);

/* Return 1 on success, 0 on an ordinary backend error and 2 when the
 * referenced message disappeared or could not be verified after a remote
 * mutation.  The latter is surfaced as a conflict by the Rust service. */

/* Create a long-lived Camel signal watcher for enabled remote mail sources.
 * The watcher owns its Camel session and folder references.  next_event may
 * be called from a worker thread and returns 1 for an event, 0 on timeout and
 * -1 after stop/fatal watcher failure. */
int meridian_eds_camel_watcher_new(
    const char *source_uid,
    MeridianEdsCamelWatcher **out_watcher,
    size_t *out_configured_sources,
    size_t *out_successful_sources,
    size_t *out_failed_sources,
    char **out_error);

int meridian_eds_camel_watcher_next_event(
    MeridianEdsCamelWatcher *watcher,
    int timeout_milliseconds,
    MeridianEdsCamelChangeEvent *out_event);

void meridian_eds_camel_free_change_event(
    MeridianEdsCamelChangeEvent *event);

void meridian_eds_camel_watcher_free(
    MeridianEdsCamelWatcher *watcher);

void meridian_eds_camel_free_error(char *error_message);

int meridian_eds_camel_store_password(
    const char *source_uid,
    const char *password,
    char **out_error);

/* Search a specific Camel folder using a Camel S-expression query (e.g.
 * (match-all (or (contains "subject" "...") (contains "from" "...")))).
 * Returns an allocated array of message UID strings and sets out_count.
 * The returned array must be freed with meridian_eds_camel_free_uids(). */
int meridian_eds_camel_search_folder(
    const char *source_uid,
    const char *folder_name,
    const char *query_sexpr,
    char ***out_uids,
    size_t *out_count,
    char **out_error,
    MeridianEdsCamelCancellationCallback cancellation_callback,
    void *cancellation_user_data);

void meridian_eds_camel_free_uids(
    char **uids,
    size_t count);

void meridian_eds_camel_reset_global_session(void);

#ifdef __cplusplus
}
#endif

#endif /* MERIDIAN_EDS_CAMEL_BRIDGE_H */
