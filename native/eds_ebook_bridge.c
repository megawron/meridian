/* SPDX-License-Identifier: GPL-3.0-or-later */

#include "eds_ebook_bridge.h"

#define __LIBEBOOK_H_INSIDE__
#include <libebook/libebook.h>
#undef __LIBEBOOK_H_INSIDE__

#include <gio/gio.h>
#include <glib.h>
#include <string.h>

typedef struct {
    EvolutionEdsEBookContact *contacts;
    size_t count;
    size_t capacity;
    size_t configured_sources;
    size_t successful_sources;
    size_t failed_sources;
    gchar *first_error;
} EBookLoadContext;

static void
remember_error (EBookLoadContext *context,
                const gchar *source_name,
                GError *error)
{
    if (context->first_error || !error)
        return;

    if (source_name && *source_name)
        context->first_error = g_strdup_printf (
            "%s: %s", source_name, error->message);
    else
        context->first_error = g_strdup (error->message);
}

static gboolean
append_contact (EBookLoadContext *context,
                ESource *source,
                EContact *contact)
{
    EvolutionEdsEBookContact *slot;
    const gchar *uid;
    const gchar *display_name;
    gchar *vcard;

    if (!contact)
        return FALSE;

    vcard = e_vcard_to_string (E_VCARD (contact));
    if (!vcard || !*vcard) {
        g_free (vcard);
        return FALSE;
    }

    if (context->count == context->capacity) {
        size_t next_capacity = context->capacity == 0
            ? 32
            : context->capacity * 2;
        EvolutionEdsEBookContact *next = g_realloc_n (
            context->contacts, next_capacity, sizeof (*next));
        if (!next) {
            g_free (vcard);
            return FALSE;
        }
        context->contacts = next;
        context->capacity = next_capacity;
    }

    uid = e_contact_get_const (contact, E_CONTACT_UID);
    display_name = e_source_get_display_name (source);
    slot = &context->contacts[context->count++];
    slot->source_uid = g_strdup (e_source_get_uid (source));
    slot->source_display_name = g_strdup (display_name ? display_name : "");
    slot->contact_uid = g_strdup (uid ? uid : "");
    slot->vcard = vcard;
    return TRUE;
}

static void
load_source (EBookLoadContext *context,
             ESource *source)
{
    EBookClient *client = NULL;
    GSList *contacts = NULL;
    GSList *link;
    GError *error = NULL;
    const gchar *display_name;

    context->configured_sources++;
    display_name = e_source_get_display_name (source);
    client = E_BOOK_CLIENT (e_book_client_connect_sync (source, 15, NULL, &error));
    if (!client) {
        context->failed_sources++;
        remember_error (context, display_name, error);
        g_clear_error (&error);
        return;
    }

    if (!e_book_client_get_contacts_sync (
            client, "#t", &contacts, NULL, &error)) {
        context->failed_sources++;
        remember_error (context, display_name, error);
        g_clear_error (&error);
        g_clear_object (&client);
        return;
    }

    context->successful_sources++;
    for (link = contacts; link != NULL; link = link->next)
        append_contact (context, source, E_CONTACT (link->data));
    g_slist_free_full (contacts, g_object_unref);
    g_clear_object (&client);
}

int
evolution_eds_ebook_load_contacts (
    EvolutionEdsEBookContact **out_contacts,
    size_t *out_count,
    size_t *out_configured_sources,
    size_t *out_successful_sources,
    size_t *out_failed_sources,
    char **out_error)
{
    ESourceRegistry *registry = NULL;
    GList *sources = NULL;
    GList *link;
    GError *error = NULL;
    EBookLoadContext context = { 0 };

    if (out_contacts)
        *out_contacts = NULL;
    if (out_count)
        *out_count = 0;
    if (out_configured_sources)
        *out_configured_sources = 0;
    if (out_successful_sources)
        *out_successful_sources = 0;
    if (out_failed_sources)
        *out_failed_sources = 0;
    if (out_error)
        *out_error = NULL;

    registry = e_source_registry_new_sync (NULL, &error);
    if (!registry) {
        if (out_error && error)
            *out_error = g_strdup (error->message);
        g_clear_error (&error);
        return 0;
    }

    sources = e_source_registry_list_enabled (
        registry, E_SOURCE_EXTENSION_ADDRESS_BOOK);
    for (link = sources; link != NULL; link = link->next)
        load_source (&context, E_SOURCE (link->data));

    if (out_contacts) {
        *out_contacts = context.contacts;
        context.contacts = NULL;
    }
    if (out_count) {
        *out_count = context.count;
        context.count = 0;
    }
    if (out_configured_sources)
        *out_configured_sources = context.configured_sources;
    if (out_successful_sources)
        *out_successful_sources = context.successful_sources;
    if (out_failed_sources)
        *out_failed_sources = context.failed_sources;
    if (out_error && context.first_error) {
        *out_error = context.first_error;
        context.first_error = NULL;
    }

    g_list_free_full (sources, g_object_unref);
    g_clear_object (&registry);
    g_free (context.first_error);
    evolution_eds_ebook_free_contacts (context.contacts, context.count);
    return 1;
}

static gboolean
error_is_conflict (const GError *error)
{
    gchar *lower;
    gboolean conflict;

    if (!error || !error->message)
        return FALSE;
    lower = g_ascii_strdown (error->message, -1);
    conflict = strstr (lower, "conflict") != NULL ||
        strstr (lower, "modified") != NULL ||
        strstr (lower, "changed since") != NULL ||
        strstr (lower, "stale") != NULL;
    g_free (lower);
    return conflict;
}

static int
return_operation_error (GError **error,
                        char **out_error)
{
    gboolean conflict;
    gchar *message;

    if (!error || !*error) {
        if (out_error)
            *out_error = g_strdup ("EDS/EBook-Mutation fehlgeschlagen");
        return 0;
    }

    conflict = error_is_conflict (*error);
    message = g_strdup_printf (
        "%s%s",
        conflict ? "conflict: " : "",
        (*error)->message ? (*error)->message : "unbekannter EBook-Fehler");
    if (out_error)
        *out_error = message;
    else
        g_free (message);
    g_clear_error (error);
    return conflict ? 2 : 0;
}

int
evolution_eds_ebook_apply_operation (
    const char *source_uid,
    EvolutionEdsEBookOperationKind operation,
    const char *vcard,
    const char *contact_uid,
    char **out_uid,
    char **out_vcard,
    char **out_error)
{
    ESourceRegistry *registry = NULL;
    ESource *source = NULL;
    EBookClient *client = NULL;
    EContact *contact = NULL;
    EContact *stored_contact = NULL;
    GError *error = NULL;
    gchar *added_uid = NULL;
    gchar *confirmed_vcard = NULL;
    const gchar *uid = contact_uid;
    int result = 0;

    if (out_uid)
        *out_uid = NULL;
    if (out_vcard)
        *out_vcard = NULL;
    if (out_error)
        *out_error = NULL;

    if (!source_uid || !*source_uid) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_INVALID_ARGUMENT,
            "EBook-Mutation benötigt eine Quell-UID");
        return return_operation_error (&error, out_error);
    }
    if (operation != EVOLUTION_EDS_EBOOK_CREATE &&
        operation != EVOLUTION_EDS_EBOOK_MODIFY &&
        operation != EVOLUTION_EDS_EBOOK_REMOVE) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_INVALID_ARGUMENT,
            "Unbekannte EBook-Mutation");
        return return_operation_error (&error, out_error);
    }
    if ((operation == EVOLUTION_EDS_EBOOK_CREATE ||
         operation == EVOLUTION_EDS_EBOOK_MODIFY) &&
        (!vcard || !*vcard)) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_INVALID_ARGUMENT,
            "EBook-Mutation benötigt eine vCard");
        return return_operation_error (&error, out_error);
    }
    if ((operation == EVOLUTION_EDS_EBOOK_MODIFY ||
         operation == EVOLUTION_EDS_EBOOK_REMOVE) &&
        (!contact_uid || !*contact_uid)) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_INVALID_ARGUMENT,
            "EBook-Mutation benötigt eine Kontakt-UID");
        return return_operation_error (&error, out_error);
    }

    registry = e_source_registry_new_sync (NULL, &error);
    if (!registry)
        goto finish;
    source = e_source_registry_ref_source (registry, source_uid);
    if (!source) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_NOT_FOUND,
            "Die EBook-Quelle existiert nicht mehr");
        goto finish;
    }
    if (!e_source_get_enabled (source)) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_FAILED,
            "Die EBook-Quelle ist deaktiviert");
        goto finish;
    }

    client = E_BOOK_CLIENT (e_book_client_connect_sync (source, 15, NULL, &error));
    if (!client)
        goto finish;

    if (operation == EVOLUTION_EDS_EBOOK_CREATE ||
        operation == EVOLUTION_EDS_EBOOK_MODIFY) {
        if (operation == EVOLUTION_EDS_EBOOK_CREATE)
            contact = e_contact_new_from_vcard (vcard);
        else
            contact = e_contact_new_from_vcard_with_uid (vcard, contact_uid);
        if (!contact) {
            g_set_error_literal (
                &error, G_IO_ERROR, G_IO_ERROR_INVALID_DATA,
                "Ungültige vCard");
            goto finish;
        }
        if (operation == EVOLUTION_EDS_EBOOK_CREATE) {
            if (!e_book_client_add_contact_sync (
                    client, contact, E_BOOK_OPERATION_FLAG_CONFLICT_FAIL,
                    &added_uid, NULL, &error))
                goto finish;
            uid = added_uid;
        } else if (!e_book_client_modify_contact_sync (
                       client, contact, E_BOOK_OPERATION_FLAG_CONFLICT_FAIL,
                       NULL, &error)) {
            goto finish;
        }

        if (uid && *uid && e_book_client_get_contact_sync (
                client, uid, &stored_contact, NULL, &error)) {
            confirmed_vcard = e_vcard_to_string (
                E_VCARD (stored_contact));
        } else {
            g_clear_error (&error);
            confirmed_vcard = g_strdup (vcard);
        }
        if (out_uid)
            *out_uid = g_strdup (uid ? uid : "");
        if (out_vcard)
            *out_vcard = confirmed_vcard;
        confirmed_vcard = NULL;
        result = 1;
    } else if (!e_book_client_remove_contact_by_uid_sync (
                   client, contact_uid, E_BOOK_OPERATION_FLAG_CONFLICT_FAIL,
                   NULL, &error)) {
        goto finish;
    } else {
        if (out_uid)
            *out_uid = g_strdup (contact_uid);
        result = 1;
    }

finish:
    if (result == 0)
        result = return_operation_error (&error, out_error);
    g_free (confirmed_vcard);
    g_free (added_uid);
    g_clear_object (&stored_contact);
    g_clear_object (&contact);
    g_clear_object (&client);
    g_clear_object (&source);
    g_clear_object (&registry);
    return result;
}

void
evolution_eds_ebook_free_contacts (
    EvolutionEdsEBookContact *contacts,
    size_t count)
{
    size_t ii;

    if (!contacts)
        return;
    for (ii = 0; ii < count; ii++) {
        g_free (contacts[ii].source_uid);
        g_free (contacts[ii].source_display_name);
        g_free (contacts[ii].contact_uid);
        g_free (contacts[ii].vcard);
    }
    g_free (contacts);
}

void
evolution_eds_ebook_free_error (char *error_message)
{
    g_free (error_message);
}

typedef struct {
    EvolutionEdsEBookWatcher *watcher;
    EBookClient *client;
    EBookClientView *view;
    gulong objects_added_id;
    gulong objects_modified_id;
    gulong objects_removed_id;
    gulong complete_id;
    gchar *source_uid;
    gchar *source_display_name;
} EBookWatcherView;

typedef struct {
    EvolutionEdsEBookWatcherEvent event;
} EBookWatcherQueuedEvent;

struct _EvolutionEdsEBookWatcher {
    GMutex lock;
    GCond condition;
    gboolean stopping;
    guint callback_count;
    GQueue *events;
    GPtrArray *views;
    GMainContext *context;
    GSource *wakeup_source;
    ESourceRegistry *registry;
};

static gboolean
ebook_watcher_wakeup_cb (gpointer user_data)
{
    (void) user_data;
    return G_SOURCE_CONTINUE;
}

static void
free_ebook_watcher_event (EBookWatcherQueuedEvent *queued_event)
{
    if (!queued_event)
        return;

    g_free (queued_event->event.source_uid);
    g_free (queued_event->event.source_display_name);
    g_free (queued_event->event.contact_uid);
    g_free (queued_event->event.vcard);
    g_free (queued_event->event.error);
    g_free (queued_event);
}

static gboolean
ebook_watcher_callback_begin (EBookWatcherView *watch_view)
{
    EvolutionEdsEBookWatcher *watcher = watch_view->watcher;
    gboolean active;

    g_mutex_lock (&watcher->lock);
    active = !watcher->stopping;
    if (active)
        watcher->callback_count++;
    g_mutex_unlock (&watcher->lock);

    return active;
}

static void
ebook_watcher_callback_end (EBookWatcherView *watch_view)
{
    EvolutionEdsEBookWatcher *watcher = watch_view->watcher;

    g_mutex_lock (&watcher->lock);
    if (watcher->callback_count > 0)
        watcher->callback_count--;
    if (watcher->callback_count == 0)
        g_cond_broadcast (&watcher->condition);
    g_mutex_unlock (&watcher->lock);
}

static void
queue_ebook_watcher_event (EBookWatcherView *watch_view,
                           EvolutionEdsEBookWatcherEventKind kind,
                           const gchar *contact_uid,
                           const gchar *vcard,
                           const gchar *error_message)
{
    EvolutionEdsEBookWatcher *watcher = watch_view->watcher;
    EBookWatcherQueuedEvent *queued_event;

    g_mutex_lock (&watcher->lock);
    if (watcher->stopping) {
        g_mutex_unlock (&watcher->lock);
        return;
    }

    queued_event = g_new0 (EBookWatcherQueuedEvent, 1);
    queued_event->event.kind = kind;
    queued_event->event.source_uid = g_strdup (watch_view->source_uid);
    queued_event->event.source_display_name = g_strdup (
        watch_view->source_display_name);
    queued_event->event.contact_uid = g_strdup (contact_uid);
    queued_event->event.vcard = g_strdup (vcard);
    queued_event->event.error = g_strdup (error_message);
    g_queue_push_tail (watcher->events, queued_event);
    g_cond_signal (&watcher->condition);
    g_mutex_unlock (&watcher->lock);
}

static void
ebook_watcher_objects_changed_cb (EBookClientView *view,
                                  const GSList *contacts,
                                  gpointer user_data,
                                  EvolutionEdsEBookWatcherEventKind kind)
{
    EBookWatcherView *watch_view = user_data;
    const GSList *link;

    (void) view;
    if (!ebook_watcher_callback_begin (watch_view))
        return;

    for (link = contacts; link != NULL; link = link->next) {
        EContact *contact = E_CONTACT (link->data);
        gchar *vcard;

        if (!contact)
            continue;

        vcard = e_vcard_to_string (
            E_VCARD (contact));
        if (vcard && *vcard) {
            queue_ebook_watcher_event (
                watch_view, kind,
                e_contact_get_const (contact, E_CONTACT_UID),
                vcard, NULL);
        }
        g_free (vcard);
    }

    ebook_watcher_callback_end (watch_view);
}

static void
ebook_watcher_objects_added_cb (EBookClientView *view,
                                const GSList *contacts,
                                gpointer user_data)
{
    ebook_watcher_objects_changed_cb (
        view, contacts, user_data, EVOLUTION_EDS_EBOOK_OBJECT_ADDED);
}

static void
ebook_watcher_objects_modified_cb (EBookClientView *view,
                                   const GSList *contacts,
                                   gpointer user_data)
{
    ebook_watcher_objects_changed_cb (
        view, contacts, user_data, EVOLUTION_EDS_EBOOK_OBJECT_MODIFIED);
}

static void
ebook_watcher_objects_removed_cb (EBookClientView *view,
                                  const GSList *ids,
                                  gpointer user_data)
{
    EBookWatcherView *watch_view = user_data;
    const GSList *link;

    (void) view;
    if (!ebook_watcher_callback_begin (watch_view))
        return;

    for (link = ids; link != NULL; link = link->next)
        queue_ebook_watcher_event (
            watch_view, EVOLUTION_EDS_EBOOK_OBJECT_REMOVED,
            link->data, NULL, NULL);

    ebook_watcher_callback_end (watch_view);
}

static void
ebook_watcher_complete_cb (EBookClientView *view,
                           const GError *error,
                           gpointer user_data)
{
    EBookWatcherView *watch_view = user_data;

    (void) view;
    if (!ebook_watcher_callback_begin (watch_view))
        return;

    if (error)
        queue_ebook_watcher_event (
            watch_view, EVOLUTION_EDS_EBOOK_WATCHER_ERROR,
            NULL, NULL, error->message);

    ebook_watcher_callback_end (watch_view);
}

static void
free_ebook_watcher_view (EBookWatcherView *watch_view)
{
    if (!watch_view)
        return;

    if (watch_view->view) {
        if (watch_view->objects_added_id)
            g_signal_handler_disconnect (
                watch_view->view, watch_view->objects_added_id);
        if (watch_view->objects_modified_id)
            g_signal_handler_disconnect (
                watch_view->view, watch_view->objects_modified_id);
        if (watch_view->objects_removed_id)
            g_signal_handler_disconnect (
                watch_view->view, watch_view->objects_removed_id);
        if (watch_view->complete_id)
            g_signal_handler_disconnect (
                watch_view->view, watch_view->complete_id);
        e_book_client_view_stop (watch_view->view, NULL);
    }

    g_clear_object (&watch_view->view);
    g_clear_object (&watch_view->client);
    g_free (watch_view->source_uid);
    g_free (watch_view->source_display_name);
    g_free (watch_view);
}

static void
ebook_watcher_remember_error (gchar **first_error,
                              const gchar *source_name,
                              GError *error)
{
    if (!*first_error && error)
        *first_error = g_strdup_printf (
            "%s: %s",
            source_name && *source_name ? source_name : "EBook-Quelle",
            error->message);
}

static gboolean
ebook_watcher_add_source (EvolutionEdsEBookWatcher *watcher,
                          ESource *source,
                          gchar **first_error)
{
    EBookWatcherView *watch_view = NULL;
    EBookClient *client = NULL;
    EBookClientView *view = NULL;
    GError *error = NULL;

    client = E_BOOK_CLIENT (e_book_client_connect_sync (source, 15, NULL, &error));
    if (!client) {
        ebook_watcher_remember_error (
            first_error, e_source_get_display_name (source), error);
        g_clear_error (&error);
        return FALSE;
    }

    if (!e_book_client_get_view_sync (
            client, "#t", &view, NULL, &error)) {
        ebook_watcher_remember_error (
            first_error, e_source_get_display_name (source), error);
        g_clear_error (&error);
        g_clear_object (&client);
        return FALSE;
    }

    watch_view = g_new0 (EBookWatcherView, 1);
    watch_view->watcher = watcher;
    watch_view->client = client;
    watch_view->view = view;
    watch_view->source_uid = g_strdup (e_source_get_uid (source));
    watch_view->source_display_name = g_strdup (
        e_source_get_display_name (source));
    watch_view->objects_added_id = g_signal_connect (
        view, "objects-added",
        G_CALLBACK (ebook_watcher_objects_added_cb), watch_view);
    watch_view->objects_modified_id = g_signal_connect (
        view, "objects-modified",
        G_CALLBACK (ebook_watcher_objects_modified_cb), watch_view);
    watch_view->objects_removed_id = g_signal_connect (
        view, "objects-removed",
        G_CALLBACK (ebook_watcher_objects_removed_cb), watch_view);
    watch_view->complete_id = g_signal_connect (
        view, "complete",
        G_CALLBACK (ebook_watcher_complete_cb), watch_view);

    e_book_client_view_set_fields_of_interest (view, NULL, &error);
    if (error)
        goto failed;

    e_book_client_view_start (view, &error);
    if (error)
        goto failed;

    g_ptr_array_add (watcher->views, watch_view);
    return TRUE;

failed:
    ebook_watcher_remember_error (
        first_error, e_source_get_display_name (source), error);
    g_clear_error (&error);
    free_ebook_watcher_view (watch_view);
    return FALSE;
}

int
evolution_eds_ebook_watcher_new (
    EvolutionEdsEBookWatcher **out_watcher,
    size_t *out_configured_sources,
    size_t *out_active_sources,
    size_t *out_failed_sources,
    char **out_error)
{
    EvolutionEdsEBookWatcher *watcher;
    GList *sources = NULL;
    GList *link;
    gchar *first_error = NULL;
    GError *error = NULL;
    size_t configured_sources = 0;
    size_t active_sources = 0;
    size_t failed_sources = 0;

    if (out_watcher)
        *out_watcher = NULL;
    if (out_configured_sources)
        *out_configured_sources = 0;
    if (out_active_sources)
        *out_active_sources = 0;
    if (out_failed_sources)
        *out_failed_sources = 0;
    if (out_error)
        *out_error = NULL;
    if (!out_watcher)
        return 0;

    watcher = g_new0 (EvolutionEdsEBookWatcher, 1);
    g_mutex_init (&watcher->lock);
    g_cond_init (&watcher->condition);
    watcher->events = g_queue_new ();
    watcher->views = g_ptr_array_new_with_free_func (
        (GDestroyNotify) free_ebook_watcher_view);
    watcher->context = g_main_context_new ();
    watcher->wakeup_source = g_timeout_source_new (50);
    g_source_set_callback (
        watcher->wakeup_source, ebook_watcher_wakeup_cb, NULL, NULL);
    g_source_attach (watcher->wakeup_source, watcher->context);

    g_main_context_push_thread_default (watcher->context);
    watcher->registry = e_source_registry_new_sync (NULL, &error);
    if (watcher->registry) {
        sources = e_source_registry_list_enabled (
            watcher->registry, E_SOURCE_EXTENSION_ADDRESS_BOOK);
        for (link = sources; link != NULL; link = link->next) {
            ESource *source = E_SOURCE (link->data);

            configured_sources++;
            if (ebook_watcher_add_source (
                    watcher, source, &first_error))
                active_sources++;
            else
                failed_sources++;
        }
    }
    g_main_context_pop_thread_default (watcher->context);

    if (!watcher->registry) {
        if (out_error && error)
            *out_error = g_strdup (error->message);
        g_clear_error (&error);
        g_list_free_full (sources, g_object_unref);
        evolution_eds_ebook_watcher_free (watcher);
        return 0;
    }

    if (out_configured_sources)
        *out_configured_sources = configured_sources;
    if (out_active_sources)
        *out_active_sources = active_sources;
    if (out_failed_sources)
        *out_failed_sources = failed_sources;
    if (out_error)
        *out_error = first_error;
    else
        g_free (first_error);

    g_list_free_full (sources, g_object_unref);
    *out_watcher = watcher;
    return 1;
}

int
evolution_eds_ebook_watcher_next_event (
    EvolutionEdsEBookWatcher *watcher,
    int timeout_milliseconds,
    EvolutionEdsEBookWatcherEvent *out_event)
{
    EBookWatcherQueuedEvent *queued_event = NULL;
    gint64 deadline;

    if (!watcher || !out_event)
        return -1;

    memset (out_event, 0, sizeof (*out_event));
    deadline = g_get_monotonic_time () +
        (timeout_milliseconds > 0 ? timeout_milliseconds * 1000 : 0);

    for (;;) {
        gboolean stopping;

        g_mutex_lock (&watcher->lock);
        if (!g_queue_is_empty (watcher->events))
            queued_event = g_queue_pop_head (watcher->events);
        stopping = watcher->stopping;
        g_mutex_unlock (&watcher->lock);

        if (queued_event)
            break;
        if (stopping)
            return -1;
        if (timeout_milliseconds == 0 ||
            (timeout_milliseconds > 0 &&
             g_get_monotonic_time () >= deadline))
            return 0;

        g_main_context_iteration (
            watcher->context, timeout_milliseconds != 0);
    }

    *out_event = queued_event->event;
    g_free (queued_event);
    return 1;
}

void
evolution_eds_ebook_free_watcher_event (
    EvolutionEdsEBookWatcherEvent *event)
{
    if (!event)
        return;

    g_free (event->source_uid);
    g_free (event->source_display_name);
    g_free (event->contact_uid);
    g_free (event->vcard);
    g_free (event->error);
    memset (event, 0, sizeof (*event));
}

void
evolution_eds_ebook_watcher_free (EvolutionEdsEBookWatcher *watcher)
{
    if (!watcher)
        return;

    g_mutex_lock (&watcher->lock);
    watcher->stopping = TRUE;
    g_cond_broadcast (&watcher->condition);
    g_mutex_unlock (&watcher->lock);
    g_main_context_wakeup (watcher->context);

    g_mutex_lock (&watcher->lock);
    while (watcher->callback_count > 0)
        g_cond_wait (&watcher->condition, &watcher->lock);
    g_mutex_unlock (&watcher->lock);

    g_ptr_array_set_size (watcher->views, 0);
    if (watcher->wakeup_source) {
        g_source_destroy (watcher->wakeup_source);
        g_source_unref (watcher->wakeup_source);
        watcher->wakeup_source = NULL;
    }
    while (!g_queue_is_empty (watcher->events))
        free_ebook_watcher_event (g_queue_pop_head (watcher->events));
    g_queue_free (watcher->events);
    g_ptr_array_unref (watcher->views);
    g_clear_object (&watcher->registry);
    g_main_context_unref (watcher->context);
    g_cond_clear (&watcher->condition);
    g_mutex_clear (&watcher->lock);
    g_free (watcher);
}
