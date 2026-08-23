/* SPDX-License-Identifier: GPL-3.0-or-later */

#ifndef EVOLUTION_EDS_ECAL_BRIDGE_H
#define EVOLUTION_EDS_ECAL_BRIDGE_H

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef enum {
    EVOLUTION_EDS_ECAL_EVENTS = 0,
    EVOLUTION_EDS_ECAL_TASKS = 1,
    EVOLUTION_EDS_ECAL_MEMOS = 2,
} EvolutionEdsECalSourceType;

typedef struct {
    char *source_uid;
    char *source_display_name;
    char *component;
} EvolutionEdsECalComponent;

typedef struct {
    char *source_uid;
    char *source_display_name;
    int connected;
    int source_writable;
    int writable;
    int refresh_supported;
    int removable;
    int remote_deletable;
    int in_collection;
    int task_assignment_supported;
    int task_assignment_conversion_supported;
} EvolutionEdsECalSourceCapability;

typedef struct {
    char *source_uid;
    char *display_name;
    char *color;
    char *custom_file_uri;
    int custom_file_writable;
    char *email_address;
    int is_builtin;
} EvolutionEdsECalLocalSourceProperties;

typedef enum {
    EVOLUTION_EDS_ECAL_CREATE = 1,
    EVOLUTION_EDS_ECAL_MODIFY = 2,
    EVOLUTION_EDS_ECAL_REMOVE = 3,
} EvolutionEdsECalOperationKind;

typedef enum {
    EVOLUTION_EDS_ECAL_OBJECT_ADDED = 1,
    EVOLUTION_EDS_ECAL_OBJECT_MODIFIED = 2,
    EVOLUTION_EDS_ECAL_OBJECT_REMOVED = 3,
    EVOLUTION_EDS_ECAL_WATCHER_ERROR = 4,
} EvolutionEdsECalWatcherEventKind;

typedef struct {
    EvolutionEdsECalSourceType source_type;
    EvolutionEdsECalWatcherEventKind kind;
    char *source_uid;
    char *source_display_name;
    char *uid;
    char *component;
    char *error;
} EvolutionEdsECalWatcherEvent;

typedef struct _EvolutionEdsECalWatcher EvolutionEdsECalWatcher;

/* Load all enabled EDS calendar objects of one source type.  The component
 * strings are owned by the caller and must be released with
 * evolution_eds_ecal_free_components(). */
int evolution_eds_ecal_load_components(
    EvolutionEdsECalSourceType source_type,
    EvolutionEdsECalComponent **out_components,
    size_t *out_count,
    size_t *out_configured_sources,
    size_t *out_successful_sources,
    size_t *out_failed_sources,
    char **out_error);

void evolution_eds_ecal_free_components(
    EvolutionEdsECalComponent *components,
    size_t count);

/* Apply one ECal mutation.  Returns 1 on success, 0 on a regular backend
 * error and 2 when the backend rejected the operation as a conflict.  For
 * create/modify, out_component contains the component confirmed by EDS. */
int evolution_eds_ecal_apply_operation(
    const char *source_uid,
    EvolutionEdsECalSourceType source_type,
    EvolutionEdsECalOperationKind operation,
    const char *component_string,
    const char *component_uid,
    char **out_uid,
    char **out_component,
    char **out_error);

/* Refresh one exact enabled EDS source.  Returns 1 after a confirmed refresh,
 * 2 when the connected client does not support refresh, and 0 on validation,
 * connection, authentication, cancellation, or backend failure. */
int evolution_eds_ecal_refresh_source(
    const char *source_uid,
    EvolutionEdsECalSourceType source_type,
    char **out_error);

/* List exact enabled EDS sources for one component type.  Sources that could
 * not connect retain source_writable/removal metadata, while
 * connected/writable/refresh_supported are all false; out_error contains only
 * the first non-secret backend error. */
int evolution_eds_ecal_list_source_capabilities(
    EvolutionEdsECalSourceType source_type,
    EvolutionEdsECalSourceCapability **out_sources,
    size_t *out_count,
    char **out_error);

void evolution_eds_ecal_free_source_capabilities(
    EvolutionEdsECalSourceCapability *sources,
    size_t count);

/* Rename one exact EDS source after revalidating its component type and
 * source-level mutability.  A collection-owned source is never renamed. */
int evolution_eds_ecal_rename_source(
    const char *source_uid,
    EvolutionEdsECalSourceType source_type,
    const char *display_name,
    char **out_error);

/* Delete one exact EDS source.  Remote deletion is used only when the source
 * advertises it; otherwise the source must be locally removable. */
int evolution_eds_ecal_delete_source(
    const char *source_uid,
    EvolutionEdsECalSourceType source_type,
    char **out_error);

/* Copy every object from one exact enabled EDS source to another.  Existing
 * destination UIDs are modified; missing UIDs are created after referenced
 * timezones are copied.  Returns 1 on complete success, 2 after an item
 * failure (with confirmed counts), and 0 when validation/preflight fails. */
int evolution_eds_ecal_copy_source(
    const char *from_source_uid,
    const char *to_source_uid,
    EvolutionEdsECalSourceType source_type,
    size_t *out_copied_count,
    size_t *out_updated_count,
    char **out_error);

/* Create a local EDS source of the requested exact component type.  Returns
 * 1 after commit plus a confirmed client connection, 2 when commit succeeded
 * but the source is not yet usable (out_warning), or 0 before commit failure. */
int evolution_eds_ecal_create_local_source(
    const char *display_name,
    const char *color,
    EvolutionEdsECalSourceType source_type,
    char **out_source_uid,
    char **out_warning,
    char **out_error);

/* Read editable properties from one exact local EDS source.  Returns 1 on
 * success, 2 when the source is not a local backend, or 0 on validation or
 * registry failure. */
int evolution_eds_ecal_get_local_source_properties(
    const char *source_uid,
    EvolutionEdsECalSourceType source_type,
    EvolutionEdsECalLocalSourceProperties *out_properties,
    char **out_error);

void evolution_eds_ecal_clear_local_source_properties(
    EvolutionEdsECalLocalSourceProperties *properties);

/* Commit editable local EDS properties.  Returns 1 after commit plus a
 * confirmed client connection, 2 after commit when the source is not yet
 * reachable (out_warning), 3 for a non-local source, and 0 before-commit
 * validation/registry failure. */
int evolution_eds_ecal_update_local_source_properties(
    const char *source_uid,
    EvolutionEdsECalSourceType source_type,
    const char *display_name,
    const char *color,
    const char *custom_file_uri,
    int custom_file_writable,
    const char *email_address,
    char **out_warning,
    char **out_error);

void evolution_eds_ecal_free_error(char *error_message);

/* Watch enabled ECal sources through ECalClientView signals.  The watcher
 * owns the EDS clients and views; next_event may be called from a worker
 * thread and returns 1 for an event, 0 on timeout and -1 after stop. */
int evolution_eds_ecal_watcher_new(
    EvolutionEdsECalWatcher **out_watcher,
    size_t *out_configured_sources,
    size_t *out_active_sources,
    size_t *out_failed_sources,
    char **out_error);

int evolution_eds_ecal_watcher_next_event(
    EvolutionEdsECalWatcher *watcher,
    int timeout_milliseconds,
    EvolutionEdsECalWatcherEvent *out_event);

void evolution_eds_ecal_free_watcher_event(
    EvolutionEdsECalWatcherEvent *event);

void evolution_eds_ecal_watcher_free(
    EvolutionEdsECalWatcher *watcher);

#ifdef __cplusplus
}
#endif

#endif /* EVOLUTION_EDS_ECAL_BRIDGE_H */
