/* SPDX-License-Identifier: GPL-3.0-or-later */

#ifndef MERIDIAN_EDS_EBOOK_BRIDGE_H
#define MERIDIAN_EDS_EBOOK_BRIDGE_H

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct {
    char *source_uid;
    char *source_display_name;
    char *contact_uid;
    char *vcard;
} MeridianEdsEBookContact;

typedef enum {
    MERIDIAN_EDS_EBOOK_CREATE = 1,
    MERIDIAN_EDS_EBOOK_MODIFY = 2,
    MERIDIAN_EDS_EBOOK_REMOVE = 3,
} MeridianEdsEBookOperationKind;

typedef enum {
    MERIDIAN_EDS_EBOOK_OBJECT_ADDED = 1,
    MERIDIAN_EDS_EBOOK_OBJECT_MODIFIED = 2,
    MERIDIAN_EDS_EBOOK_OBJECT_REMOVED = 3,
    MERIDIAN_EDS_EBOOK_WATCHER_ERROR = 4,
} MeridianEdsEBookWatcherEventKind;

typedef struct {
    MeridianEdsEBookWatcherEventKind kind;
    char *source_uid;
    char *source_display_name;
    char *contact_uid;
    char *vcard;
    char *error;
} MeridianEdsEBookWatcherEvent;

typedef struct _MeridianEdsEBookWatcher MeridianEdsEBookWatcher;

/* Load all contacts from enabled EDS address-book sources.  The returned
 * records are owned by the caller and must be released with
 * meridian_eds_ebook_free_contacts(). */
int meridian_eds_ebook_load_contacts(
    MeridianEdsEBookContact **out_contacts,
    size_t *out_count,
    size_t *out_configured_sources,
    size_t *out_successful_sources,
    size_t *out_failed_sources,
    char **out_error);

void meridian_eds_ebook_free_contacts(
    MeridianEdsEBookContact *contacts,
    size_t count);

/* Apply one address-book mutation.  Returns 1 on success, 0 on an ordinary
 * backend error and 2 when the backend rejected the operation as a conflict.
 * For create/modify, out_vcard contains the contact confirmed by EDS. */
int meridian_eds_ebook_apply_operation(
    const char *source_uid,
    MeridianEdsEBookOperationKind operation,
    const char *vcard,
    const char *contact_uid,
    char **out_uid,
    char **out_vcard,
    char **out_error);

void meridian_eds_ebook_free_error(char *error_message);

/* Watch enabled EDS address books through EBookClientView signals.  The
 * watcher owns its EBook clients and views; next_event returns 1 for an
 * event, 0 on timeout and -1 after the watcher has been stopped. */
int meridian_eds_ebook_watcher_new(
    MeridianEdsEBookWatcher **out_watcher,
    size_t *out_configured_sources,
    size_t *out_active_sources,
    size_t *out_failed_sources,
    char **out_error);

int meridian_eds_ebook_watcher_next_event(
    MeridianEdsEBookWatcher *watcher,
    int timeout_milliseconds,
    MeridianEdsEBookWatcherEvent *out_event);

void meridian_eds_ebook_free_watcher_event(
    MeridianEdsEBookWatcherEvent *event);

void meridian_eds_ebook_watcher_free(
    MeridianEdsEBookWatcher *watcher);

#ifdef __cplusplus
}
#endif

#endif /* MERIDIAN_EDS_EBOOK_BRIDGE_H */
