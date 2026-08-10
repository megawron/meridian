/* SPDX-License-Identifier: GPL-3.0-or-later */

#ifndef EVOLUTION_EDS_EBOOK_BRIDGE_H
#define EVOLUTION_EDS_EBOOK_BRIDGE_H

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct {
    char *source_uid;
    char *source_display_name;
    char *contact_uid;
    char *vcard;
} EvolutionEdsEBookContact;

typedef enum {
    EVOLUTION_EDS_EBOOK_CREATE = 1,
    EVOLUTION_EDS_EBOOK_MODIFY = 2,
    EVOLUTION_EDS_EBOOK_REMOVE = 3,
} EvolutionEdsEBookOperationKind;

typedef enum {
    EVOLUTION_EDS_EBOOK_OBJECT_ADDED = 1,
    EVOLUTION_EDS_EBOOK_OBJECT_MODIFIED = 2,
    EVOLUTION_EDS_EBOOK_OBJECT_REMOVED = 3,
    EVOLUTION_EDS_EBOOK_WATCHER_ERROR = 4,
} EvolutionEdsEBookWatcherEventKind;

typedef struct {
    EvolutionEdsEBookWatcherEventKind kind;
    char *source_uid;
    char *source_display_name;
    char *contact_uid;
    char *vcard;
    char *error;
} EvolutionEdsEBookWatcherEvent;

typedef struct _EvolutionEdsEBookWatcher EvolutionEdsEBookWatcher;

/* Load all contacts from enabled EDS address-book sources.  The returned
 * records are owned by the caller and must be released with
 * evolution_eds_ebook_free_contacts(). */
int evolution_eds_ebook_load_contacts(
    EvolutionEdsEBookContact **out_contacts,
    size_t *out_count,
    size_t *out_configured_sources,
    size_t *out_successful_sources,
    size_t *out_failed_sources,
    char **out_error);

void evolution_eds_ebook_free_contacts(
    EvolutionEdsEBookContact *contacts,
    size_t count);

/* Apply one address-book mutation.  Returns 1 on success, 0 on an ordinary
 * backend error and 2 when the backend rejected the operation as a conflict.
 * For create/modify, out_vcard contains the contact confirmed by EDS. */
int evolution_eds_ebook_apply_operation(
    const char *source_uid,
    EvolutionEdsEBookOperationKind operation,
    const char *vcard,
    const char *contact_uid,
    char **out_uid,
    char **out_vcard,
    char **out_error);

void evolution_eds_ebook_free_error(char *error_message);

/* Watch enabled EDS address books through EBookClientView signals.  The
 * watcher owns its EBook clients and views; next_event returns 1 for an
 * event, 0 on timeout and -1 after the watcher has been stopped. */
int evolution_eds_ebook_watcher_new(
    EvolutionEdsEBookWatcher **out_watcher,
    size_t *out_configured_sources,
    size_t *out_active_sources,
    size_t *out_failed_sources,
    char **out_error);

int evolution_eds_ebook_watcher_next_event(
    EvolutionEdsEBookWatcher *watcher,
    int timeout_milliseconds,
    EvolutionEdsEBookWatcherEvent *out_event);

void evolution_eds_ebook_free_watcher_event(
    EvolutionEdsEBookWatcherEvent *event);

void evolution_eds_ebook_watcher_free(
    EvolutionEdsEBookWatcher *watcher);

#ifdef __cplusplus
}
#endif

#endif /* EVOLUTION_EDS_EBOOK_BRIDGE_H */
