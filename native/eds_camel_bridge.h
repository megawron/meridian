/* SPDX-License-Identifier: GPL-3.0-or-later */

#ifndef EVOLUTION_EDS_CAMEL_BRIDGE_H
#define EVOLUTION_EDS_CAMEL_BRIDGE_H

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
} EvolutionEdsCamelMessage;

typedef struct {
    char *source_uid;
    char *source_display_name;
    char *folder_name;
    char *folder_id;
    char *display_name;
    uint32_t unread_count;
    uint32_t total_count;
    int is_selectable;
} EvolutionEdsCamelFolder;

typedef enum {
    EVOLUTION_EDS_CAMEL_SET_MESSAGE_STATE = 1,
    EVOLUTION_EDS_CAMEL_DELETE_MESSAGE = 2,
    EVOLUTION_EDS_CAMEL_MOVE_MESSAGE = 3,
    EVOLUTION_EDS_CAMEL_ARCHIVE_MESSAGE = 4,
    EVOLUTION_EDS_CAMEL_SET_JUNK = 5,
    EVOLUTION_EDS_CAMEL_SET_IMPORTANT = 6,
} EvolutionEdsCamelOperationKind;

typedef struct {
    EvolutionEdsCamelOperationKind kind;
    const char *source_uid;
    const char *folder_name;
    const char *message_uid;
    const char *destination_folder;
    int is_unread;
    int is_flagged;
    int is_junk;
    int is_important;
} EvolutionEdsCamelOperation;

typedef enum {
    EVOLUTION_EDS_CAMEL_SYNC_DISCOVERING = 1,
    EVOLUTION_EDS_CAMEL_SYNC_CONNECTING = 2,
    EVOLUTION_EDS_CAMEL_SYNC_LOADING_FOLDERS = 3,
    EVOLUTION_EDS_CAMEL_SYNC_LOADING_MESSAGES = 4,
    EVOLUTION_EDS_CAMEL_SYNC_SOURCE_COMPLETE = 5,
    EVOLUTION_EDS_CAMEL_SYNC_COMPLETE = 6,
    EVOLUTION_EDS_CAMEL_SYNC_FAILED = 7,
} EvolutionEdsCamelSyncPhase;

typedef struct {
    EvolutionEdsCamelSyncPhase phase;
    const char *source_uid;
    const char *source_display_name;
    const char *folder_name;
    size_t configured_sources;
    size_t processed_sources;
    size_t successful_sources;
    size_t failed_sources;
    size_t folders_seen;
    size_t messages_loaded;
} EvolutionEdsCamelSyncProgress;

typedef void (*EvolutionEdsCamelProgressCallback) (
    const EvolutionEdsCamelSyncProgress *progress,
    void *user_data);

typedef int (*EvolutionEdsCamelCancellationCallback) (
    void *user_data);

typedef enum {
    EVOLUTION_EDS_CAMEL_CHANGE = 1,
    EVOLUTION_EDS_CAMEL_FOLDER_DELETED = 2,
    EVOLUTION_EDS_CAMEL_FOLDER_RENAMED = 3,
} EvolutionEdsCamelChangeKind;

typedef struct {
    EvolutionEdsCamelChangeKind kind;
    char *source_uid;
    char *folder_name;
    char *old_folder_name;
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
} EvolutionEdsCamelChangeEvent;

typedef struct _EvolutionEdsCamelWatcher EvolutionEdsCamelWatcher;

/*
 * Load messages from the enabled EDS mail-account sources.  The returned
 * records contain owned RFC 822 bytes and must be released with
 * evolution_eds_camel_free_messages().
 *
 * source_uid may be NULL to load every enabled mail-account source.
 * folder_name may be NULL to load every selectable folder for the selected
 * source(s); otherwise only that exact Camel folder receives message data.
 * max_messages_per_folder <= 0 means the implementation default.
 */
int evolution_eds_camel_load_messages(
    const char *source_uid,
    const char *folder_name,
    int message_offset,
    int max_messages_per_folder,
    EvolutionEdsCamelMessage **out_messages,
    size_t *out_count,
    EvolutionEdsCamelFolder **out_folders,
    size_t *out_folder_count,
    size_t *out_configured_sources,
    size_t *out_successful_sources,
    size_t *out_failed_sources,
    char **out_error,
    EvolutionEdsCamelProgressCallback progress_callback,
    void *progress_user_data,
    EvolutionEdsCamelCancellationCallback cancellation_callback,
    void *cancellation_user_data);

/* Load only the UIDs reported by CamelFolder::changed.  This keeps signal
 * handling incremental while retaining the normal RFC-822 parsing path. */
int evolution_eds_camel_load_messages_by_uid(
    const char *source_uid,
    const char *folder_name,
    const char * const *message_uids,
    size_t message_uid_count,
    EvolutionEdsCamelMessage **out_messages,
    size_t *out_count,
    char **out_error,
    EvolutionEdsCamelCancellationCallback cancellation_callback,
    void *cancellation_user_data);

void evolution_eds_camel_free_messages(
    EvolutionEdsCamelMessage *messages,
    size_t count);

void evolution_eds_camel_free_folders(
    EvolutionEdsCamelFolder *folders,
    size_t count);

int evolution_eds_camel_apply_operation(
    const EvolutionEdsCamelOperation *operation,
    char **out_error);

/* Return 1 on success, 0 on an ordinary backend error and 2 when the
 * referenced message disappeared or could not be verified after a remote
 * mutation.  The latter is surfaced as a conflict by the Rust service. */

/* Create a long-lived Camel signal watcher for enabled remote mail sources.
 * The watcher owns its Camel session and folder references.  next_event may
 * be called from a worker thread and returns 1 for an event, 0 on timeout and
 * -1 after stop/fatal watcher failure. */
int evolution_eds_camel_watcher_new(
    const char *source_uid,
    EvolutionEdsCamelWatcher **out_watcher,
    size_t *out_configured_sources,
    size_t *out_successful_sources,
    size_t *out_failed_sources,
    char **out_error);

int evolution_eds_camel_watcher_next_event(
    EvolutionEdsCamelWatcher *watcher,
    int timeout_milliseconds,
    EvolutionEdsCamelChangeEvent *out_event);

void evolution_eds_camel_free_change_event(
    EvolutionEdsCamelChangeEvent *event);

void evolution_eds_camel_watcher_free(
    EvolutionEdsCamelWatcher *watcher);

void evolution_eds_camel_free_error(char *error_message);

#ifdef __cplusplus
}
#endif

#endif /* EVOLUTION_EDS_CAMEL_BRIDGE_H */
