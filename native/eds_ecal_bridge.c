/* SPDX-License-Identifier: GPL-3.0-or-later */

#include "eds_ecal_bridge.h"

#define __LIBECAL_H_INSIDE__
#include <libecal/libecal.h>
#undef __LIBECAL_H_INSIDE__

#include <gio/gio.h>
#include <glib.h>
#include <string.h>

typedef struct {
    EvolutionEdsECalComponent *components;
    size_t count;
    size_t capacity;
    size_t configured_sources;
    size_t successful_sources;
    size_t failed_sources;
    gchar *first_error;
} ECalLoadContext;

static void
remember_error (ECalLoadContext *context,
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
append_component (ECalLoadContext *context,
                  ESource *source,
                  ECalComponent *component)
{
    gchar *component_string;
    EvolutionEdsECalComponent *slot;
    const gchar *uid;
    const gchar *display_name;

    if (!component)
        return FALSE;

    component_string = e_cal_component_get_as_string (component);
    if (!component_string || !*component_string) {
        g_free (component_string);
        return FALSE;
    }

    if (context->count == context->capacity) {
        size_t next_capacity = context->capacity == 0
            ? 32
            : context->capacity * 2;
        EvolutionEdsECalComponent *next = g_realloc_n (
            context->components, next_capacity, sizeof (*next));
        if (!next) {
            g_free (component_string);
            return FALSE;
        }
        context->components = next;
        context->capacity = next_capacity;
    }

    uid = e_source_get_uid (source);
    display_name = e_source_get_display_name (source);
    slot = &context->components[context->count++];
    slot->source_uid = g_strdup (uid ? uid : "");
    slot->source_display_name = g_strdup (display_name ? display_name : "");
    slot->component = component_string;
    return TRUE;
}

static void
load_source (ECalLoadContext *context,
             ESource *source,
             EvolutionEdsECalSourceType source_type)
{
    ECalClient *client = NULL;
    GSList *components = NULL;
    GSList *link;
    GError *error = NULL;
    const gchar *display_name;

    context->configured_sources++;
    display_name = e_source_get_display_name (source);
    client = e_cal_client_connect_sync (
        source, (ECalClientSourceType) source_type, 15, NULL, &error);
    if (!client) {
        context->failed_sources++;
        remember_error (context, display_name, error);
        g_clear_error (&error);
        return;
    }

    if (!e_cal_client_get_object_list_as_comps_sync (
            client, "#t", &components, NULL, &error)) {
        context->failed_sources++;
        remember_error (context, display_name, error);
        g_clear_error (&error);
        g_object_unref (client);
        return;
    }

    context->successful_sources++;
    for (link = components; link != NULL; link = link->next) {
        append_component (
            context, source, E_CAL_COMPONENT (link->data));
        g_object_unref (link->data);
    }
    g_slist_free (components);
    g_object_unref (client);
}

int
evolution_eds_ecal_load_components (
    EvolutionEdsECalSourceType source_type,
    EvolutionEdsECalComponent **out_components,
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
    ECalLoadContext context = { 0 };
    const gchar *extension_name;
    int result = 0;

    if (out_components)
        *out_components = NULL;
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

    if (source_type == EVOLUTION_EDS_ECAL_EVENTS)
        extension_name = E_SOURCE_EXTENSION_CALENDAR;
    else if (source_type == EVOLUTION_EDS_ECAL_TASKS)
        extension_name = E_SOURCE_EXTENSION_TASK_LIST;
    else if (source_type == EVOLUTION_EDS_ECAL_MEMOS)
        extension_name = E_SOURCE_EXTENSION_MEMO_LIST;
    else {
        if (out_error)
            *out_error = g_strdup ("Unbekannter ECal-Quellentyp");
        return 0;
    }

    registry = e_source_registry_new_sync (NULL, &error);
    if (!registry) {
        if (out_error && error)
            *out_error = g_strdup (error->message);
        g_clear_error (&error);
        return 0;
    }

    sources = e_source_registry_list_enabled (registry, extension_name);
    for (link = sources; link != NULL; link = link->next)
        load_source (&context, E_SOURCE (link->data), source_type);

    if (out_configured_sources)
        *out_configured_sources = context.configured_sources;
    if (out_successful_sources)
        *out_successful_sources = context.successful_sources;
    if (out_failed_sources)
        *out_failed_sources = context.failed_sources;

    if (context.successful_sources > 0 || context.configured_sources == 0) {
        if (out_components) {
            *out_components = context.components;
            context.components = NULL;
        }
        if (out_count) {
            *out_count = context.count;
            context.count = 0;
        }
        result = 1;
    } else if (context.configured_sources > 0) {
        /* Preserve an all-failed result as a successful transport response so
         * the Rust layer can show the actual EDS error without mistaking it
         * for an empty local calendar. */
        if (out_components) {
            *out_components = context.components;
            context.components = NULL;
        }
        if (out_count) {
            *out_count = context.count;
            context.count = 0;
        }
        result = 1;
    }

    if (out_error && context.first_error) {
        *out_error = context.first_error;
        context.first_error = NULL;
    }

    g_list_free_full (sources, g_object_unref);
    g_object_unref (registry);
    g_free (context.first_error);
    evolution_eds_ecal_free_components (context.components, context.count);
    return result;
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
            *out_error = g_strdup ("EDS/ECal-Mutation fehlgeschlagen");
        return 0;
    }

    conflict = error_is_conflict (*error);
    message = g_strdup_printf (
        "%s%s",
        conflict ? "conflict: " : "",
        (*error)->message ? (*error)->message : "unbekannter ECal-Fehler");
    if (out_error)
        *out_error = message;
    else
        g_free (message);
    g_clear_error (error);
    return conflict ? 2 : 0;
}

static void
set_component_uid_if_missing (ICalComponent *component,
                              const gchar *uid)
{
    if (!component || !uid || !*uid)
        return;
    if (!i_cal_component_get_uid (component))
        i_cal_component_set_uid (component, uid);
}

int
evolution_eds_ecal_apply_operation (
    const char *source_uid,
    EvolutionEdsECalSourceType source_type,
    EvolutionEdsECalOperationKind operation,
    const char *component_string,
    const char *component_uid,
    char **out_uid,
    char **out_component,
    char **out_error)
{
    ESourceRegistry *registry = NULL;
    ESource *source = NULL;
    ECalClient *client = NULL;
    ICalComponent *component = NULL;
    ICalComponent *stored_component = NULL;
    GError *error = NULL;
    gchar *created_uid = NULL;
    const gchar *uid = component_uid;
    gchar *confirmed_component = NULL;
    int result = 0;

    if (out_uid)
        *out_uid = NULL;
    if (out_component)
        *out_component = NULL;
    if (out_error)
        *out_error = NULL;

    if (!source_uid || !*source_uid) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_INVALID_ARGUMENT,
            "ECal-Mutation benötigt eine Quell-UID");
        return return_operation_error (&error, out_error);
    }
    if (source_type != EVOLUTION_EDS_ECAL_EVENTS &&
        source_type != EVOLUTION_EDS_ECAL_TASKS &&
        source_type != EVOLUTION_EDS_ECAL_MEMOS) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_INVALID_ARGUMENT,
            "Unbekannter ECal-Quellentyp");
        return return_operation_error (&error, out_error);
    }
    if (operation != EVOLUTION_EDS_ECAL_CREATE &&
        operation != EVOLUTION_EDS_ECAL_MODIFY &&
        operation != EVOLUTION_EDS_ECAL_REMOVE) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_INVALID_ARGUMENT,
            "Unbekannte ECal-Mutation");
        return return_operation_error (&error, out_error);
    }
    if ((operation == EVOLUTION_EDS_ECAL_CREATE ||
         operation == EVOLUTION_EDS_ECAL_MODIFY) &&
        (!component_string || !*component_string)) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_INVALID_ARGUMENT,
            "ECal-Mutation benötigt eine iCalendar-Komponente");
        return return_operation_error (&error, out_error);
    }
    if ((operation == EVOLUTION_EDS_ECAL_MODIFY ||
         operation == EVOLUTION_EDS_ECAL_REMOVE) &&
        (!component_uid || !*component_uid)) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_INVALID_ARGUMENT,
            "ECal-Mutation benötigt eine Komponenten-UID");
        return return_operation_error (&error, out_error);
    }

    registry = e_source_registry_new_sync (NULL, &error);
    if (!registry)
        goto finish;

    source = e_source_registry_ref_source (registry, source_uid);
    if (!source) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_NOT_FOUND,
            "Die ECal-Quelle existiert nicht mehr");
        goto finish;
    }
    if (!e_source_get_enabled (source)) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_FAILED,
            "Die ECal-Quelle ist deaktiviert");
        goto finish;
    }

    client = e_cal_client_connect_sync (
        source, (ECalClientSourceType) source_type, 15, NULL, &error);
    if (!client)
        goto finish;

    if (operation == EVOLUTION_EDS_ECAL_CREATE ||
        operation == EVOLUTION_EDS_ECAL_MODIFY) {
        component = i_cal_component_new_from_string (component_string);
        if (!component) {
            g_set_error_literal (
                &error, G_IO_ERROR, G_IO_ERROR_INVALID_DATA,
                "Ungültige iCalendar-Komponente");
            goto finish;
        }
        if (!uid || !*uid)
            uid = i_cal_component_get_uid (component);
        set_component_uid_if_missing (component, uid);
        if (operation == EVOLUTION_EDS_ECAL_CREATE) {
            if (!e_cal_client_create_object_sync (
                    client, component, E_CAL_OPERATION_FLAG_CONFLICT_FAIL,
                    &created_uid, NULL, &error))
                goto finish;
            uid = created_uid;
        } else if (!e_cal_client_modify_object_sync (
                       client, component, E_CAL_OBJ_MOD_ALL,
                       E_CAL_OPERATION_FLAG_CONFLICT_FAIL, NULL, &error)) {
            goto finish;
        }

        if (uid && *uid &&
            e_cal_client_get_object_sync (
                client, uid, NULL, &stored_component, NULL, &error)) {
            confirmed_component = i_cal_component_as_ical_string (
                stored_component);
        } else {
            /* The mutation succeeded, but a backend may not expose an
             * immediate read-after-write.  Return the submitted object with
             * the confirmed UID so the Rust layer can refresh it later. */
            g_clear_error (&error);
            set_component_uid_if_missing (component, uid);
            confirmed_component = i_cal_component_as_ical_string (component);
        }
        if (out_uid)
            *out_uid = g_strdup (uid ? uid : "");
        if (out_component)
            *out_component = confirmed_component;
        confirmed_component = NULL;
        result = 1;
    } else if (!e_cal_client_remove_object_sync (
                   client, component_uid, NULL, E_CAL_OBJ_MOD_ALL,
                   E_CAL_OPERATION_FLAG_CONFLICT_FAIL, NULL, &error)) {
        goto finish;
    } else {
        if (out_uid)
            *out_uid = g_strdup (component_uid);
        result = 1;
    }

finish:
    if (result == 0)
        result = return_operation_error (&error, out_error);
    g_free (confirmed_component);
    g_free (created_uid);
    g_clear_object (&stored_component);
    g_clear_object (&component);
    g_clear_object (&client);
    g_clear_object (&source);
    g_clear_object (&registry);
    return result;
}

void
evolution_eds_ecal_free_components (
    EvolutionEdsECalComponent *components,
    size_t count)
{
    size_t ii;

    if (!components)
        return;
    for (ii = 0; ii < count; ii++) {
        g_free (components[ii].source_uid);
        g_free (components[ii].source_display_name);
        g_free (components[ii].component);
    }
    g_free (components);
}

void
evolution_eds_ecal_free_error (char *error_message)
{
    g_free (error_message);
}

typedef struct {
    EvolutionEdsECalWatcher *watcher;
    ECalClient *client;
    ECalClientView *view;
    gulong objects_added_id;
    gulong objects_modified_id;
    gulong objects_removed_id;
    gulong complete_id;
    EvolutionEdsECalSourceType source_type;
    gchar *source_uid;
    gchar *source_display_name;
} ECalWatcherView;

typedef struct {
    EvolutionEdsECalWatcherEvent event;
} ECalWatcherQueuedEvent;

struct _EvolutionEdsECalWatcher {
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
ecal_watcher_wakeup_cb (gpointer user_data)
{
    (void) user_data;
    return G_SOURCE_CONTINUE;
}

static void
free_ecal_watcher_event (ECalWatcherQueuedEvent *queued_event)
{
    if (!queued_event)
        return;

    g_free (queued_event->event.source_uid);
    g_free (queued_event->event.source_display_name);
    g_free (queued_event->event.uid);
    g_free (queued_event->event.component);
    g_free (queued_event->event.error);
    g_free (queued_event);
}

static gboolean
ecal_watcher_callback_begin (ECalWatcherView *watch_view)
{
    EvolutionEdsECalWatcher *watcher = watch_view->watcher;
    gboolean active;

    g_mutex_lock (&watcher->lock);
    active = !watcher->stopping;
    if (active)
        watcher->callback_count++;
    g_mutex_unlock (&watcher->lock);

    return active;
}

static void
ecal_watcher_callback_end (ECalWatcherView *watch_view)
{
    EvolutionEdsECalWatcher *watcher = watch_view->watcher;

    g_mutex_lock (&watcher->lock);
    if (watcher->callback_count > 0)
        watcher->callback_count--;
    if (watcher->callback_count == 0)
        g_cond_broadcast (&watcher->condition);
    g_mutex_unlock (&watcher->lock);
}

static void
queue_ecal_watcher_event (ECalWatcherView *watch_view,
                          EvolutionEdsECalWatcherEventKind kind,
                          const gchar *uid,
                          const gchar *component,
                          const gchar *error_message)
{
    EvolutionEdsECalWatcher *watcher = watch_view->watcher;
    ECalWatcherQueuedEvent *queued_event;

    g_mutex_lock (&watcher->lock);
    if (watcher->stopping) {
        g_mutex_unlock (&watcher->lock);
        return;
    }

    queued_event = g_new0 (ECalWatcherQueuedEvent, 1);
    queued_event->event.source_type = watch_view->source_type;
    queued_event->event.kind = kind;
    queued_event->event.source_uid = g_strdup (watch_view->source_uid);
    queued_event->event.source_display_name = g_strdup (
        watch_view->source_display_name);
    queued_event->event.uid = g_strdup (uid);
    queued_event->event.component = g_strdup (component);
    queued_event->event.error = g_strdup (error_message);
    g_queue_push_tail (watcher->events, queued_event);
    g_cond_signal (&watcher->condition);
    g_mutex_unlock (&watcher->lock);
}

static void
ecal_watcher_objects_changed_cb (ECalClientView *view,
                                 const GSList *objects,
                                 gpointer user_data,
                                 EvolutionEdsECalWatcherEventKind kind)
{
    ECalWatcherView *watch_view = user_data;
    const GSList *link;

    (void) view;
    if (!ecal_watcher_callback_begin (watch_view))
        return;

    for (link = objects; link != NULL; link = link->next) {
        ICalComponent *component = link->data;
        gchar *component_string;

        if (!component)
            continue;

        component_string = i_cal_component_as_ical_string (component);
        if (component_string && *component_string) {
            queue_ecal_watcher_event (
                watch_view, kind,
                i_cal_component_get_uid (component),
                component_string, NULL);
        }
        g_free (component_string);
    }

    ecal_watcher_callback_end (watch_view);
}

static void
ecal_watcher_objects_added_cb (ECalClientView *view,
                               const GSList *objects,
                               gpointer user_data)
{
    ecal_watcher_objects_changed_cb (
        view, objects, user_data, EVOLUTION_EDS_ECAL_OBJECT_ADDED);
}

static void
ecal_watcher_objects_modified_cb (ECalClientView *view,
                                  const GSList *objects,
                                  gpointer user_data)
{
    ecal_watcher_objects_changed_cb (
        view, objects, user_data, EVOLUTION_EDS_ECAL_OBJECT_MODIFIED);
}

static void
ecal_watcher_objects_removed_cb (ECalClientView *view,
                                 const GSList *objects,
                                 gpointer user_data)
{
    ECalWatcherView *watch_view = user_data;
    const GSList *link;

    (void) view;
    if (!ecal_watcher_callback_begin (watch_view))
        return;

    for (link = objects; link != NULL; link = link->next) {
        ECalComponentId *id = link->data;

        if (id) {
            queue_ecal_watcher_event (
                watch_view, EVOLUTION_EDS_ECAL_OBJECT_REMOVED,
                e_cal_component_id_get_uid (id), NULL, NULL);
        }
    }

    ecal_watcher_callback_end (watch_view);
}

static void
ecal_watcher_complete_cb (ECalClientView *view,
                          const GError *error,
                          gpointer user_data)
{
    ECalWatcherView *watch_view = user_data;

    (void) view;
    if (!ecal_watcher_callback_begin (watch_view))
        return;

    if (error) {
        queue_ecal_watcher_event (
            watch_view, EVOLUTION_EDS_ECAL_WATCHER_ERROR,
            NULL, NULL, error->message);
    }

    ecal_watcher_callback_end (watch_view);
}

static void
free_ecal_watcher_view (ECalWatcherView *watch_view)
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
        e_cal_client_view_stop (watch_view->view, NULL);
    }

    g_clear_object (&watch_view->view);
    g_clear_object (&watch_view->client);
    g_free (watch_view->source_uid);
    g_free (watch_view->source_display_name);
    g_free (watch_view);
}

static void
ecal_watcher_remember_error (gchar **first_error,
                             const gchar *source_name,
                             GError *error)
{
    if (!*first_error && error)
        *first_error = g_strdup_printf (
            "%s: %s",
            source_name && *source_name ? source_name : "ECal-Quelle",
            error->message);
}

static gboolean
ecal_watcher_add_source (EvolutionEdsECalWatcher *watcher,
                         ESource *source,
                         EvolutionEdsECalSourceType source_type,
                         gchar **first_error)
{
    ECalWatcherView *watch_view = NULL;
    ECalClient *client = NULL;
    ECalClientView *view = NULL;
    GError *error = NULL;

    client = e_cal_client_connect_sync (
        source, (ECalClientSourceType) source_type, 15, NULL, &error);
    if (!client) {
        ecal_watcher_remember_error (
            first_error, e_source_get_display_name (source), error);
        g_clear_error (&error);
        return FALSE;
    }

    if (!e_cal_client_get_view_sync (
            client, "#t", &view, NULL, &error)) {
        ecal_watcher_remember_error (
            first_error, e_source_get_display_name (source), error);
        g_clear_error (&error);
        g_clear_object (&client);
        return FALSE;
    }

    watch_view = g_new0 (ECalWatcherView, 1);
    watch_view->watcher = watcher;
    watch_view->client = client;
    watch_view->view = view;
    watch_view->source_type = source_type;
    watch_view->source_uid = g_strdup (e_source_get_uid (source));
    watch_view->source_display_name = g_strdup (
        e_source_get_display_name (source));
    watch_view->objects_added_id = g_signal_connect (
        view, "objects-added",
        G_CALLBACK (ecal_watcher_objects_added_cb), watch_view);
    watch_view->objects_modified_id = g_signal_connect (
        view, "objects-modified",
        G_CALLBACK (ecal_watcher_objects_modified_cb), watch_view);
    watch_view->objects_removed_id = g_signal_connect (
        view, "objects-removed",
        G_CALLBACK (ecal_watcher_objects_removed_cb), watch_view);
    watch_view->complete_id = g_signal_connect (
        view, "complete",
        G_CALLBACK (ecal_watcher_complete_cb), watch_view);
    g_ptr_array_add (watcher->views, watch_view);

    e_cal_client_view_set_flags (
        view, E_CAL_CLIENT_VIEW_FLAGS_NOTIFY_INITIAL);
    e_cal_client_view_start (view, &error);
    if (error) {
        ecal_watcher_remember_error (
            first_error, e_source_get_display_name (source), error);
        g_clear_error (&error);
        g_ptr_array_remove (watcher->views, watch_view);
        return FALSE;
    }

    return TRUE;
}

int
evolution_eds_ecal_watcher_new (
    EvolutionEdsECalWatcher **out_watcher,
    size_t *out_configured_sources,
    size_t *out_active_sources,
    size_t *out_failed_sources,
    char **out_error)
{
    EvolutionEdsECalWatcher *watcher;
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

    watcher = g_new0 (EvolutionEdsECalWatcher, 1);
    g_mutex_init (&watcher->lock);
    g_cond_init (&watcher->condition);
    watcher->events = g_queue_new ();
    watcher->views = g_ptr_array_new_with_free_func (
        (GDestroyNotify) free_ecal_watcher_view);
    watcher->context = g_main_context_new ();
    watcher->wakeup_source = g_timeout_source_new (50);
    g_source_set_callback (
        watcher->wakeup_source, ecal_watcher_wakeup_cb, NULL, NULL);
    g_source_attach (watcher->wakeup_source, watcher->context);

    g_main_context_push_thread_default (watcher->context);
    watcher->registry = e_source_registry_new_sync (NULL, &error);
    g_main_context_pop_thread_default (watcher->context);
    if (!watcher->registry) {
        if (out_error && error)
            *out_error = g_strdup (error->message);
        g_clear_error (&error);
        evolution_eds_ecal_watcher_free (watcher);
        return 0;
    }

    g_main_context_push_thread_default (watcher->context);
    for (EvolutionEdsECalSourceType source_type =
             EVOLUTION_EDS_ECAL_EVENTS;
         source_type <= EVOLUTION_EDS_ECAL_MEMOS;
         source_type++) {
        const gchar *extension_name;

        if (source_type == EVOLUTION_EDS_ECAL_EVENTS)
            extension_name = E_SOURCE_EXTENSION_CALENDAR;
        else if (source_type == EVOLUTION_EDS_ECAL_TASKS)
            extension_name = E_SOURCE_EXTENSION_TASK_LIST;
        else
            extension_name = E_SOURCE_EXTENSION_MEMO_LIST;

        sources = e_source_registry_list_enabled (
            watcher->registry, extension_name);
        for (link = sources; link != NULL; link = link->next) {
            ESource *source = E_SOURCE (link->data);

            configured_sources++;
            if (ecal_watcher_add_source (
                    watcher, source, source_type, &first_error))
                active_sources++;
            else
                failed_sources++;
        }
        g_list_free_full (sources, g_object_unref);
        sources = NULL;
    }
    g_main_context_pop_thread_default (watcher->context);

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

    *out_watcher = watcher;
    return 1;
}

int
evolution_eds_ecal_watcher_next_event (
    EvolutionEdsECalWatcher *watcher,
    int timeout_milliseconds,
    EvolutionEdsECalWatcherEvent *out_event)
{
    ECalWatcherQueuedEvent *queued_event = NULL;
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
evolution_eds_ecal_free_watcher_event (
    EvolutionEdsECalWatcherEvent *event)
{
    if (!event)
        return;

    g_free (event->source_uid);
    g_free (event->source_display_name);
    g_free (event->uid);
    g_free (event->component);
    g_free (event->error);
    memset (event, 0, sizeof (*event));
}

void
evolution_eds_ecal_watcher_free (EvolutionEdsECalWatcher *watcher)
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
        free_ecal_watcher_event (g_queue_pop_head (watcher->events));
    g_queue_free (watcher->events);
    g_ptr_array_unref (watcher->views);
    g_clear_object (&watcher->registry);
    g_main_context_unref (watcher->context);
    g_cond_clear (&watcher->condition);
    g_mutex_clear (&watcher->lock);
    g_free (watcher);
}
