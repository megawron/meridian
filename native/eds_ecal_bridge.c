/* SPDX-License-Identifier: GPL-3.0-or-later */

#include "eds_ecal_bridge.h"

#define __LIBECAL_H_INSIDE__
#include <libecal/libecal.h>
#undef __LIBECAL_H_INSIDE__

#include <gio/gio.h>
#include <glib.h>
#include <string.h>
#include "eds_ecal_snapshot_cache.h"

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

static gboolean
ecal_cancel_timeout_cb (gpointer data)
{
    GCancellable *c = (GCancellable *) data;
    g_cancellable_cancel (c);
    return G_SOURCE_REMOVE;
}

/* A timeout source owns a cancellable reference until it is destroyed.  This
 * avoids handing an already-removed source ID to g_source_remove() when the
 * synchronous EDS call iterates a main context and the timeout fires first. */
static GSource *
ecal_start_cancellation_timer (GCancellable *cancellable,
                               guint timeout_milliseconds)
{
    GSource *source;

    source = g_timeout_source_new (timeout_milliseconds);
    g_source_set_callback (
        source, ecal_cancel_timeout_cb, g_object_ref (cancellable),
        (GDestroyNotify) g_object_unref);
    g_source_attach (source, g_main_context_get_thread_default ());
    return source;
}

static void
ecal_stop_cancellation_timer (GSource *source)
{
    if (!source)
        return;

    g_source_destroy (source);
    g_source_unref (source);
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
    const gchar *extension_name = NULL;
    gchar *query = NULL;

    if (source_type == EVOLUTION_EDS_ECAL_EVENTS)
        extension_name = E_SOURCE_EXTENSION_CALENDAR;
    else if (source_type == EVOLUTION_EDS_ECAL_TASKS)
        extension_name = E_SOURCE_EXTENSION_TASK_LIST;
    else if (source_type == EVOLUTION_EDS_ECAL_MEMOS)
        extension_name = E_SOURCE_EXTENSION_MEMO_LIST;

    if (!extension_name || !e_source_has_extension (source, extension_name))
        return;

    {
        ESourceExtension *ext = e_source_get_extension (source, extension_name);
        if (!E_IS_SOURCE_BACKEND (ext))
            return;
        const gchar *backend_name = e_source_backend_get_backend_name (E_SOURCE_BACKEND (ext));
        if (!backend_name || !*backend_name)
            return;
    }

    context->configured_sources++;
    display_name = e_source_get_display_name (source);

    guint connect_timeout_ms = 8000;
    guint32 wait_seconds = 3;

    /* Match Evolution's range model: a cached client is immediately useful
     * for locally synchronized data and must not be held behind an online
     * connection wait.  Only a cache miss gets the existing bounded open. */
    client = ecal_ref_cached_snapshot_client (source, source_type);
    if (!client) {
        GCancellable *cancellable = g_cancellable_new ();
        GSource *timer = ecal_start_cancellation_timer (
            cancellable, connect_timeout_ms);

        client = E_CAL_CLIENT (e_cal_client_connect_sync (
            source, (ECalClientSourceType) source_type, wait_seconds,
            cancellable, &error));

        ecal_stop_cancellation_timer (timer);
        g_object_unref (cancellable);
        if (client)
            client = ecal_cache_snapshot_client (source, source_type, client);
    }

    if (!client) {
        context->failed_sources++;
        remember_error (context, display_name, error);
        g_clear_error (&error);
        return;
    }

    if (source_type == EVOLUTION_EDS_ECAL_EVENTS) {
        GDateTime *now = g_date_time_new_now_utc ();
        GDateTime *start = g_date_time_add_months (now, -6);
        GDateTime *end = g_date_time_add_months (now, 12);
        gchar *start_iso = g_date_time_format (start, "%Y%m%dT000000Z");
        gchar *end_iso = g_date_time_format (end, "%Y%m%dT235959Z");
        query = g_strdup_printf (
            "(occur-in-time-range? (make-time \"%s\") (make-time \"%s\"))",
            start_iso, end_iso);
        g_free (start_iso);
        g_free (end_iso);
        g_date_time_unref (start);
        g_date_time_unref (end);
        g_date_time_unref (now);
    } else {
        query = g_strdup ("#t");
    }

    guint query_timeout_ms = 8000;
    GCancellable *cancellable = g_cancellable_new ();
    GSource *timer = ecal_start_cancellation_timer (
        cancellable, query_timeout_ms);

    gboolean query_ok = e_cal_client_get_object_list_as_comps_sync (
        client, query, &components, cancellable, &error);

    ecal_stop_cancellation_timer (timer);
    g_object_unref (cancellable);
    g_free (query);

    if (!query_ok) {
        context->failed_sources++;
        remember_error (context, display_name, error);
        g_clear_error (&error);
        ecal_evict_snapshot_client (source, source_type, client);
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

static ESourceRegistry *
ecal_get_source_registry_with_retry (GCancellable *cancellable, GError **error)
{
    ESourceRegistry *registry = NULL;
    GError *local_error = NULL;

    for (int retry = 0; retry < 5; retry++) {
        if (cancellable && g_cancellable_is_cancelled (cancellable)) {
            g_set_error_literal (error, G_IO_ERROR, G_IO_ERROR_CANCELLED, "Operation cancelled");
            return NULL;
        }
        local_error = NULL;
        registry = e_source_registry_new_sync (cancellable, &local_error);
        if (registry)
            return registry;

        if (retry < 4) {
            g_clear_error (&local_error);
            g_usleep (200000); /* 200 ms */
        }
    }

    if (local_error)
        g_propagate_error (error, local_error);
    return NULL;
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
    ESourceCredentialsProvider *credentials_provider = NULL;
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

    registry = ecal_get_source_registry_with_retry (NULL, &error);
    if (!registry) {
        if (out_error && error)
            *out_error = g_strdup (error->message);
        g_clear_error (&error);
        return 0;
    }
    credentials_provider = e_source_credentials_provider_new (registry);

    sources = e_source_registry_list_enabled (registry, extension_name);
    ecal_prune_snapshot_clients (sources, source_type);
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
    if (credentials_provider)
        g_object_unref (credentials_provider);
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

    registry = ecal_get_source_registry_with_retry (NULL, &error);
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

    client = E_CAL_CLIENT (e_cal_client_connect_sync (
        source, (ECalClientSourceType) source_type, 15, NULL, &error));
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

static const gchar *
source_extension_name_for_type (EvolutionEdsECalSourceType source_type)
{
    switch (source_type) {
    case EVOLUTION_EDS_ECAL_EVENTS:
        return E_SOURCE_EXTENSION_CALENDAR;
    case EVOLUTION_EDS_ECAL_TASKS:
        return E_SOURCE_EXTENSION_TASK_LIST;
    case EVOLUTION_EDS_ECAL_MEMOS:
        return E_SOURCE_EXTENSION_MEMO_LIST;
    default:
        return NULL;
    }
}

static gboolean
append_source_capability (EvolutionEdsECalSourceCapability **sources,
                          size_t *count,
                          size_t *capacity,
                          ESourceRegistry *registry,
                          ESource *source,
                          ECalClient *client,
                          EvolutionEdsECalSourceType source_type)
{
    ESource *collection;
    EvolutionEdsECalSourceCapability *slot;
    EvolutionEdsECalSourceCapability *next;

    if (*count == *capacity) {
        size_t next_capacity = *capacity == 0 ? 8 : *capacity * 2;
        next = g_realloc_n (*sources, next_capacity, sizeof (*next));
        if (!next)
            return FALSE;
        *sources = next;
        *capacity = next_capacity;
    }
    slot = &(*sources)[(*count)++];
    slot->source_uid = g_strdup (e_source_get_uid (source));
    slot->source_display_name = g_strdup (e_source_get_display_name (source));
    slot->connected = client != NULL;
    slot->source_writable = e_source_get_writable (source);
    slot->writable = client && !e_client_is_readonly (E_CLIENT (client));
    slot->refresh_supported = client &&
        e_client_check_refresh_supported (E_CLIENT (client));
    slot->removable = e_source_get_removable (source);
    slot->remote_deletable = e_source_get_remote_deletable (source);
    collection = e_source_registry_find_extension (
        registry, source, E_SOURCE_EXTENSION_COLLECTION);
    slot->in_collection = collection != NULL;
    slot->task_assignment_supported = source_type == EVOLUTION_EDS_ECAL_TASKS &&
        client && !e_client_check_capability (
            E_CLIENT (client), E_CAL_STATIC_CAPABILITY_NO_TASK_ASSIGNMENT);
    slot->task_assignment_conversion_supported =
        slot->task_assignment_supported && !e_client_check_capability (
            E_CLIENT (client), E_CAL_STATIC_CAPABILITY_NO_CONV_TO_ASSIGN_TASK);
    g_clear_object (&collection);
    return TRUE;
}

int
evolution_eds_ecal_list_source_capabilities (
    EvolutionEdsECalSourceType source_type,
    EvolutionEdsECalSourceCapability **out_sources,
    size_t *out_count,
    char **out_error)
{
    ESourceRegistry *registry = NULL;
    GList *sources = NULL;
    GList *link;
    EvolutionEdsECalSourceCapability *capabilities = NULL;
    size_t count = 0;
    size_t capacity = 0;
    GError *error = NULL;
    gchar *first_error = NULL;
    const gchar *extension_name;
    int result = 0;

    if (out_sources)
        *out_sources = NULL;
    if (out_count)
        *out_count = 0;
    if (out_error)
        *out_error = NULL;
    extension_name = source_extension_name_for_type (source_type);
    if (!extension_name) {
        if (out_error)
            *out_error = g_strdup ("Unbekannter ECal-Quellentyp");
        return 0;
    }
    registry = ecal_get_source_registry_with_retry (NULL, &error);
    if (!registry)
        goto finish;

    sources = e_source_registry_list_enabled (registry, extension_name);
    for (link = sources; link != NULL; link = link->next) {
        ESource *source = E_SOURCE (link->data);
        ECalClient *client = NULL;
        const gchar *display_name = e_source_get_display_name (source);

        /* Defend against a registry/provider inconsistency: only enumerate
         * the exact extension requested by the Rust component type. */
        if (!e_source_has_extension (source, extension_name))
            continue;
        client = E_CAL_CLIENT (e_cal_client_connect_sync (
            source, (ECalClientSourceType) source_type, 1, NULL, &error));
        if (!client && !first_error && error) {
            first_error = g_strdup_printf (
                "%s: %s", display_name && *display_name ? display_name : "ECal-Quelle",
                error->message ? error->message : "Verbindung fehlgeschlagen");
        }
        g_clear_error (&error);
        if (!append_source_capability (
                &capabilities, &count, &capacity, registry, source, client,
                source_type)) {
            g_clear_object (&client);
            g_set_error_literal (
                &error, G_IO_ERROR, G_IO_ERROR_NO_SPACE,
                "ECal-Quellmetadaten konnten nicht gespeichert werden");
            goto finish;
        }
        g_clear_object (&client);
    }
    result = 1;

finish:
    if (result == 0 && out_error) {
        *out_error = g_strdup (
            error && error->message ? error->message :
            "EDS/ECal-Quellen konnten nicht gelesen werden");
    } else if (result == 1 && out_error && first_error) {
        *out_error = g_steal_pointer (&first_error);
    }
    g_clear_error (&error);
    g_free (first_error);
    g_list_free_full (sources, g_object_unref);
    g_clear_object (&registry);
    if (result == 1) {
        if (out_sources) {
            *out_sources = capabilities;
            capabilities = NULL;
        }
        if (out_count)
            *out_count = count;
    }
    evolution_eds_ecal_free_source_capabilities (capabilities, count);
    return result;
}

int
evolution_eds_ecal_refresh_source (
    const char *source_uid,
    EvolutionEdsECalSourceType source_type,
    char **out_error)
{
    ESourceRegistry *registry = NULL;
    ESource *source = NULL;
    ECalClient *client = NULL;
    GError *error = NULL;
    const gchar *extension_name;
    int result = 0;

    if (out_error)
        *out_error = NULL;
    if (!source_uid || !*source_uid) {
        if (out_error)
            *out_error = g_strdup ("ECal-Aktualisierung benötigt eine Quell-UID");
        return 0;
    }
    extension_name = source_extension_name_for_type (source_type);
    if (!extension_name) {
        if (out_error)
            *out_error = g_strdup ("Unbekannter ECal-Quellentyp");
        return 0;
    }

    registry = ecal_get_source_registry_with_retry (NULL, &error);
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
    if (!e_source_has_extension (source, extension_name)) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_INVALID_ARGUMENT,
            "Die ECal-Quelle hat nicht den erwarteten Quellentyp");
        goto finish;
    }
    client = E_CAL_CLIENT (e_cal_client_connect_sync (
        source, (ECalClientSourceType) source_type, 15, NULL, &error));
    if (!client)
        goto finish;
    if (!e_client_check_refresh_supported (E_CLIENT (client))) {
        result = 2;
        goto finish;
    }
    if (!e_client_refresh_sync (E_CLIENT (client), NULL, &error))
        goto finish;
    result = 1;

finish:
    if (result == 0 && out_error) {
        *out_error = g_strdup (
            error && error->message ? error->message :
            "EDS/ECal-Aktualisierung fehlgeschlagen");
    }
    g_clear_error (&error);
    g_clear_object (&client);
    g_clear_object (&source);
    g_clear_object (&registry);
    return result;
}

static int
return_source_management_error (GError **error,
                                char **out_error,
                                const gchar *fallback_message)
{
    if (out_error) {
        *out_error = g_strdup (
            error && *error && (*error)->message ? (*error)->message :
            fallback_message);
    }
    if (error)
        g_clear_error (error);
    return 0;
}

static gboolean
source_is_in_collection (ESourceRegistry *registry,
                         ESource *source)
{
    ESource *collection;
    gboolean in_collection;

    collection = e_source_registry_find_extension (
        registry, source, E_SOURCE_EXTENSION_COLLECTION);
    in_collection = collection != NULL;
    g_clear_object (&collection);
    return in_collection;
}

static gboolean
resolve_source_for_management (ESourceRegistry *registry,
                               const gchar *source_uid,
                               EvolutionEdsECalSourceType source_type,
                               ESource **out_source,
                               GError **error)
{
    const gchar *extension_name;
    ESource *source;

    if (out_source)
        *out_source = NULL;
    extension_name = source_extension_name_for_type (source_type);
    if (!extension_name) {
        g_set_error_literal (
            error, G_IO_ERROR, G_IO_ERROR_INVALID_ARGUMENT,
            "Unbekannter ECal-Quellentyp");
        return FALSE;
    }
    source = e_source_registry_ref_source (registry, source_uid);
    if (!source) {
        g_set_error_literal (
            error, G_IO_ERROR, G_IO_ERROR_NOT_FOUND,
            "Die ECal-Quelle existiert nicht mehr");
        return FALSE;
    }
    if (!e_source_has_extension (source, extension_name)) {
        g_set_error_literal (
            error, G_IO_ERROR, G_IO_ERROR_INVALID_ARGUMENT,
            "Die ECal-Quelle hat nicht den erwarteten Quellentyp");
        g_object_unref (source);
        return FALSE;
    }
    if (out_source)
        *out_source = source;
    else
        g_object_unref (source);
    return TRUE;
}

int
evolution_eds_ecal_rename_source (
    const char *source_uid,
    EvolutionEdsECalSourceType source_type,
    const char *display_name,
    char **out_error)
{
    ESourceRegistry *registry = NULL;
    ESource *source = NULL;
    GError *error = NULL;
    gchar *trimmed_name = NULL;
    int result = 0;

    if (out_error)
        *out_error = NULL;
    if (!source_uid || !*source_uid) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_INVALID_ARGUMENT,
            "ECal-Umbenennung benötigt eine Quell-UID");
        goto finish;
    }
    trimmed_name = g_strdup (display_name ? display_name : "");
    if (!*g_strstrip (trimmed_name)) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_INVALID_ARGUMENT,
            "ECal-Umbenennung benötigt einen Namen");
        goto finish;
    }
    registry = ecal_get_source_registry_with_retry (NULL, &error);
    if (!registry)
        goto finish;
    if (!resolve_source_for_management (
            registry, source_uid, source_type, &source, &error))
        goto finish;
    if (!e_source_get_enabled (source)) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_FAILED,
            "Die ECal-Quelle ist deaktiviert");
        goto finish;
    }
    if (!e_source_get_writable (source)) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_PERMISSION_DENIED,
            "Die ECal-Quelle ist nicht schreibbar");
        goto finish;
    }
    if (source_is_in_collection (registry, source)) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_PERMISSION_DENIED,
            "Eine sammlungsverwaltete ECal-Quelle kann nicht umbenannt werden");
        goto finish;
    }
    e_source_set_display_name (source, display_name);
    if (!e_source_write_sync (source, NULL, &error))
        goto finish;
    result = 1;

finish:
    if (result == 0)
        result = return_source_management_error (
            &error, out_error, "EDS/ECal-Umbenennung fehlgeschlagen");
    g_free (trimmed_name);
    g_clear_object (&source);
    g_clear_object (&registry);
    return result;
}

int
evolution_eds_ecal_delete_source (
    const char *source_uid,
    EvolutionEdsECalSourceType source_type,
    char **out_error)
{
    ESourceRegistry *registry = NULL;
    ESource *source = NULL;
    GError *error = NULL;
    int result = 0;

    if (out_error)
        *out_error = NULL;
    if (!source_uid || !*source_uid) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_INVALID_ARGUMENT,
            "ECal-Löschen benötigt eine Quell-UID");
        goto finish;
    }
    registry = ecal_get_source_registry_with_retry (NULL, &error);
    if (!registry)
        goto finish;
    if (!resolve_source_for_management (
            registry, source_uid, source_type, &source, &error))
        goto finish;
    if (e_source_get_remote_deletable (source)) {
        if (!e_source_remote_delete_sync (source, NULL, &error))
            goto finish;
    } else {
        if (!e_source_get_removable (source)) {
            g_set_error_literal (
                &error, G_IO_ERROR, G_IO_ERROR_PERMISSION_DENIED,
                "Die ECal-Quelle kann nicht gelöscht werden");
            goto finish;
        }
        if (!e_source_remove_sync (source, NULL, &error))
            goto finish;
    }
    result = 1;

finish:
    if (result == 0)
        result = return_source_management_error (
            &error, out_error, "EDS/ECal-Löschen fehlgeschlagen");
    g_clear_object (&source);
    g_clear_object (&registry);
    return result;
}

typedef struct {
    ECalClient *from_client;
    ECalClient *to_client;
    gboolean success;
    GError **error;
} ECalCopyTimezoneData;

static void
copy_source_timezone_cb (ICalParameter *parameter,
                         gpointer user_data)
{
    ECalCopyTimezoneData *data = user_data;
    ICalTimezone *timezone = NULL;
    const gchar *tzid;

    if (!data || !data->success)
        return;
    tzid = i_cal_parameter_get_tzid (parameter);
    if (!tzid || !*tzid)
        return;
    data->success = e_cal_client_get_timezone_sync (
        data->from_client, tzid, &timezone, NULL, data->error);
    if (data->success && timezone) {
        data->success = e_cal_client_add_timezone_sync (
            data->to_client, timezone, NULL, data->error);
    }
}

static void
free_ical_component_list (GSList *components)
{
    GSList *link;

    for (link = components; link; link = link->next) {
        if (link->data)
            g_object_unref (link->data);
    }
    g_slist_free (components);
}

int
evolution_eds_ecal_copy_source (
    const char *from_source_uid,
    const char *to_source_uid,
    EvolutionEdsECalSourceType source_type,
    size_t *out_copied_count,
    size_t *out_updated_count,
    char **out_error)
{
    ESourceRegistry *registry = NULL;
    ESource *from_source = NULL;
    ESource *to_source = NULL;
    ECalClient *from_client = NULL;
    ECalClient *to_client = NULL;
    GSList *components = NULL;
    GSList *link;
    GError *error = NULL;
    size_t copied_count = 0;
    size_t updated_count = 0;
    int result = 0;

    if (out_copied_count)
        *out_copied_count = 0;
    if (out_updated_count)
        *out_updated_count = 0;
    if (out_error)
        *out_error = NULL;
    if (!from_source_uid || !*from_source_uid ||
        !to_source_uid || !*to_source_uid) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_INVALID_ARGUMENT,
            "ECal-Quellenkopie benötigt zwei Quell-UIDs");
        goto finish;
    }
    if (g_strcmp0 (from_source_uid, to_source_uid) == 0) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_INVALID_ARGUMENT,
            "Quelle und Ziel der ECal-Quellenkopie müssen verschieden sein");
        goto finish;
    }
    registry = ecal_get_source_registry_with_retry (NULL, &error);
    if (!registry)
        goto finish;
    if (!resolve_source_for_management (
            registry, from_source_uid, source_type, &from_source, &error) ||
        !resolve_source_for_management (
            registry, to_source_uid, source_type, &to_source, &error))
        goto finish;
    if (!e_source_get_enabled (from_source) || !e_source_get_enabled (to_source)) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_FAILED,
            "Quelle oder Ziel der ECal-Quellenkopie ist deaktiviert");
        goto finish;
    }
    from_client = E_CAL_CLIENT (e_cal_client_connect_sync (
        from_source, (ECalClientSourceType) source_type, 15, NULL, &error));
    if (!from_client)
        goto finish;
    to_client = E_CAL_CLIENT (e_cal_client_connect_sync (
        to_source, (ECalClientSourceType) source_type, 15, NULL, &error));
    if (!to_client)
        goto finish;
    if (e_client_is_readonly (E_CLIENT (to_client))) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_READ_ONLY,
            "Das Ziel der ECal-Quellenkopie ist schreibgeschützt");
        goto finish;
    }
    if (!e_cal_client_get_object_list_sync (
            from_client, "#t", &components, NULL, &error))
        goto finish;

    for (link = components; link; link = link->next) {
        ICalComponent *component = I_CAL_COMPONENT (link->data);
        ICalComponent *existing = NULL;
        GError *local_error = NULL;
        const gchar *uid;

        if (!component) {
            g_set_error_literal (
                &error, G_IO_ERROR, G_IO_ERROR_INVALID_DATA,
                "Die ECal-Quelle enthält eine ungültige Komponente");
            result = 2;
            goto finish;
        }
        uid = i_cal_component_get_uid (component);
        if (!uid || !*uid) {
            g_set_error_literal (
                &error, G_IO_ERROR, G_IO_ERROR_INVALID_DATA,
                "Die ECal-Quelle enthält eine Komponente ohne UID");
            result = 2;
            goto finish;
        }
        if (e_cal_client_get_object_sync (
                to_client, uid, NULL, &existing, NULL, &local_error)) {
            if (!e_cal_client_modify_object_sync (
                    to_client, component, E_CAL_OBJ_MOD_ALL,
                    E_CAL_OPERATION_FLAG_NONE, NULL, &error)) {
                g_clear_object (&existing);
                result = 2;
                goto finish;
            }
            g_clear_object (&existing);
            copied_count++;
            updated_count++;
            continue;
        }
        if (local_error && !g_error_matches (
                local_error, E_CAL_CLIENT_ERROR,
                E_CAL_CLIENT_ERROR_OBJECT_NOT_FOUND)) {
            g_propagate_error (&error, local_error);
            result = 2;
            goto finish;
        }
        g_clear_error (&local_error);

        {
            ECalCopyTimezoneData timezone_data = {
                from_client, to_client, TRUE, &error
            };

            i_cal_component_foreach_tzid (
                component, copy_source_timezone_cb, &timezone_data);
            if (!timezone_data.success) {
                result = 2;
                goto finish;
            }
        }
        if (!e_cal_client_create_object_sync (
                to_client, component, E_CAL_OPERATION_FLAG_NONE,
                NULL, NULL, &error)) {
            result = 2;
            goto finish;
        }
        copied_count++;
    }
    result = 1;

finish:
    if (out_copied_count)
        *out_copied_count = copied_count;
    if (out_updated_count)
        *out_updated_count = updated_count;
    if (result == 0) {
        if (out_error) {
            *out_error = g_strdup (
                error && error->message ? error->message :
                "EDS/ECal-Quellenkopie fehlgeschlagen");
        }
    } else if (result == 2 && out_error) {
        *out_error = g_strdup_printf (
            "ECal-Quellenkopie nach %zu bestätigten Komponenten fehlgeschlagen: %s",
            copied_count,
            error && error->message ? error->message : "unbekannter ECal-Fehler");
    }
    g_clear_error (&error);
    free_ical_component_list (components);
    g_clear_object (&to_client);
    g_clear_object (&from_client);
    g_clear_object (&to_source);
    g_clear_object (&from_source);
    g_clear_object (&registry);
    return result;
}

static gboolean
is_valid_source_color (const gchar *color)
{
    guint ii;

    if (!color || strlen (color) != 7 || color[0] != '#')
        return FALSE;
    for (ii = 1; ii < 7; ii++) {
        if (!g_ascii_isxdigit (color[ii]))
            return FALSE;
    }
    return TRUE;
}

int
evolution_eds_ecal_create_local_source (
    const char *display_name,
    const char *color,
    EvolutionEdsECalSourceType source_type,
    char **out_source_uid,
    char **out_warning,
    char **out_error)
{
    ESourceRegistry *registry = NULL;
    ESource *parent_source = NULL;
    ESource *created_source = NULL;
    ESource *committed_source = NULL;
    ESourceExtension *component_extension;
    ESourceLocal *local_extension;
    ECalClient *client = NULL;
    GError *error = NULL;
    const gchar *extension_name;
    const gchar *source_uid;
    gchar *trimmed_display_name = NULL;
    int result = 0;

    if (out_source_uid)
        *out_source_uid = NULL;
    if (out_warning)
        *out_warning = NULL;
    if (out_error)
        *out_error = NULL;
    trimmed_display_name = g_strdup (display_name ? display_name : "");
    if (!*g_strstrip (trimmed_display_name)) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_INVALID_ARGUMENT,
            "Eine lokale ECal-Quelle benötigt einen Namen");
        goto finish;
    }
    if (!is_valid_source_color (color)) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_INVALID_ARGUMENT,
            "Die Farbe der lokalen ECal-Quelle muss #RRGGBB sein");
        goto finish;
    }
    extension_name = source_extension_name_for_type (source_type);
    if (!extension_name) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_INVALID_ARGUMENT,
            "Unbekannter ECal-Quellentyp");
        goto finish;
    }
    registry = ecal_get_source_registry_with_retry (NULL, &error);
    if (!registry)
        goto finish;
    parent_source = e_source_registry_ref_source (registry, "local-stub");
    if (!parent_source || !e_source_get_enabled (parent_source)) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_NOT_FOUND,
            "Die lokale ECal-Stammquelle ist nicht verfügbar");
        goto finish;
    }
    created_source = e_source_new (NULL, NULL, &error);
    if (!created_source)
        goto finish;
    e_source_set_parent (created_source, "local-stub");
    e_source_set_display_name (created_source, display_name);
    component_extension = e_source_get_extension (created_source, extension_name);
    if (!component_extension) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_FAILED,
            "Die lokale ECal-Quellenerweiterung konnte nicht erstellt werden");
        goto finish;
    }
    e_source_backend_set_backend_name (
        E_SOURCE_BACKEND (component_extension), "local");
    e_source_selectable_set_color (
        E_SOURCE_SELECTABLE (component_extension), color);
    local_extension = e_source_get_extension (
        created_source, E_SOURCE_EXTENSION_LOCAL_BACKEND);
    if (!local_extension) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_FAILED,
            "Die lokale ECal-Backend-Erweiterung konnte nicht erstellt werden");
        goto finish;
    }
    e_source_local_set_writable (local_extension, TRUE);
    e_source_local_set_custom_file (local_extension, NULL);
    if (!e_source_registry_commit_source_sync (
            registry, created_source, NULL, &error))
        goto finish;

    source_uid = e_source_get_uid (created_source);
    if (!source_uid || !*source_uid) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_FAILED,
            "EDS bestätigte eine lokale Quelle ohne gültige UID");
        goto finish;
    }
    if (out_source_uid)
        *out_source_uid = g_strdup (source_uid);
    if (!resolve_source_for_management (
            registry, source_uid, source_type, &committed_source, &error)) {
        if (out_warning) {
            *out_warning = g_strdup (
                error && error->message ? error->message :
                "Die lokale ECal-Quelle wurde erstellt, aber noch nicht gefunden");
        }
        g_clear_error (&error);
        result = 2;
        goto finish;
    }
    if (!e_source_get_enabled (committed_source)) {
        if (out_warning)
            *out_warning = g_strdup (
                "Die lokale ECal-Quelle wurde erstellt, ist aber deaktiviert");
        result = 2;
        goto finish;
    }
    client = E_CAL_CLIENT (e_cal_client_connect_sync (
        committed_source, (ECalClientSourceType) source_type,
        15, NULL, &error));
    if (!client) {
        if (out_warning) {
            *out_warning = g_strdup (
                error && error->message ? error->message :
                "Die lokale ECal-Quelle wurde erstellt, ist aber noch nicht erreichbar");
        }
        g_clear_error (&error);
        result = 2;
        goto finish;
    }
    result = 1;

finish:
    if (result == 0 && out_error) {
        *out_error = g_strdup (
            error && error->message ? error->message :
            "EDS konnte die lokale ECal-Quelle nicht erstellen");
    }
    g_clear_error (&error);
    g_free (trimmed_display_name);
    g_clear_object (&client);
    g_clear_object (&committed_source);
    g_clear_object (&created_source);
    g_clear_object (&parent_source);
    g_clear_object (&registry);
    return result;
}

static ESource *
ref_builtin_source_for_type (ESourceRegistry *registry,
                             EvolutionEdsECalSourceType source_type)
{
    switch (source_type) {
    case EVOLUTION_EDS_ECAL_EVENTS:
        return e_source_registry_ref_builtin_calendar (registry);
    case EVOLUTION_EDS_ECAL_TASKS:
        return e_source_registry_ref_builtin_task_list (registry);
    case EVOLUTION_EDS_ECAL_MEMOS:
        return e_source_registry_ref_builtin_memo_list (registry);
    default:
        return NULL;
    }
}

/* Returns 1 for an exact local source, 2 for a supported exact non-local
 * source, and 0 for stale/wrong-type registry failures. */
static int
resolve_local_source_for_properties (ESourceRegistry *registry,
                                     const gchar *source_uid,
                                     EvolutionEdsECalSourceType source_type,
                                     ESource **out_source,
                                     ESourceExtension **out_component_extension,
                                     ESourceLocal **out_local_extension,
                                     GError **error)
{
    ESource *source = NULL;
    ESourceExtension *component_extension;
    ESourceLocal *local_extension;
    const gchar *backend_name;

    if (out_source)
        *out_source = NULL;
    if (out_component_extension)
        *out_component_extension = NULL;
    if (out_local_extension)
        *out_local_extension = NULL;
    if (!resolve_source_for_management (
            registry, source_uid, source_type, &source, error))
        return 0;
    component_extension = e_source_get_extension (
        source, source_extension_name_for_type (source_type));
    backend_name = component_extension
        ? e_source_backend_get_backend_name (E_SOURCE_BACKEND (component_extension))
        : NULL;
    local_extension = e_source_get_extension (
        source, E_SOURCE_EXTENSION_LOCAL_BACKEND);
    if (!backend_name || g_strcmp0 (backend_name, "local") != 0 || !local_extension) {
        g_clear_object (&source);
        return 2;
    }
    if (out_source)
        *out_source = source;
    else
        g_object_unref (source);
    if (out_component_extension)
        *out_component_extension = component_extension;
    if (out_local_extension)
        *out_local_extension = local_extension;
    return 1;
}

void
evolution_eds_ecal_clear_local_source_properties (
    EvolutionEdsECalLocalSourceProperties *properties)
{
    if (!properties)
        return;
    g_free (properties->source_uid);
    g_free (properties->display_name);
    g_free (properties->color);
    g_free (properties->custom_file_uri);
    g_free (properties->email_address);
    memset (properties, 0, sizeof (*properties));
}

int
evolution_eds_ecal_get_local_source_properties (
    const char *source_uid,
    EvolutionEdsECalSourceType source_type,
    EvolutionEdsECalLocalSourceProperties *out_properties,
    char **out_error)
{
    ESourceRegistry *registry = NULL;
    ESource *source = NULL;
    ESourceExtension *component_extension = NULL;
    ESourceLocal *local_extension = NULL;
    ESource *builtin_source = NULL;
    GFile *custom_file;
    const gchar *color;
    GError *error = NULL;
    int resolved;
    int result = 0;

    if (out_properties)
        memset (out_properties, 0, sizeof (*out_properties));
    if (out_error)
        *out_error = NULL;
    if (!source_uid || !*source_uid || !out_properties) {
        if (out_error)
            *out_error = g_strdup ("Lokale ECal-Eigenschaften benötigen eine Quell-UID");
        return 0;
    }
    registry = ecal_get_source_registry_with_retry (NULL, &error);
    if (!registry)
        goto finish;
    resolved = resolve_local_source_for_properties (
        registry, source_uid, source_type, &source, &component_extension,
        &local_extension, &error);
    if (resolved == 2) {
        result = 2;
        goto finish;
    }
    if (resolved == 0)
        goto finish;
    if (!e_source_get_enabled (source)) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_FAILED,
            "Die ECal-Quelle ist deaktiviert");
        goto finish;
    }
    builtin_source = ref_builtin_source_for_type (registry, source_type);
    custom_file = e_source_local_get_custom_file (local_extension);
    color = e_source_selectable_get_color (E_SOURCE_SELECTABLE (component_extension));
    if (!is_valid_source_color (color)) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_FAILED,
            "EDS lieferte keine gültige Farbe für die lokale ECal-Quelle");
        goto finish;
    }
    out_properties->source_uid = g_strdup (e_source_get_uid (source));
    out_properties->display_name = g_strdup (e_source_get_display_name (source));
    out_properties->color = g_strdup (color);
    out_properties->custom_file_uri = custom_file ? g_file_get_uri (custom_file) : NULL;
    out_properties->custom_file_writable = e_source_local_get_writable (local_extension);
    out_properties->email_address = g_strdup (
        e_source_local_get_email_address (local_extension));
    out_properties->is_builtin = builtin_source && e_source_equal (source, builtin_source);
    result = 1;

finish:
    if (result == 0) {
        evolution_eds_ecal_clear_local_source_properties (out_properties);
        if (out_error) {
            *out_error = g_strdup (
                error && error->message ? error->message :
                "EDS konnte die lokalen ECal-Eigenschaften nicht lesen");
        }
    }
    g_clear_error (&error);
    g_clear_object (&builtin_source);
    g_clear_object (&source);
    g_clear_object (&registry);
    return result;
}

int
evolution_eds_ecal_update_local_source_properties (
    const char *source_uid,
    EvolutionEdsECalSourceType source_type,
    const char *display_name,
    const char *color,
    const char *custom_file_uri,
    int custom_file_writable,
    const char *email_address,
    char **out_warning,
    char **out_error)
{
    ESourceRegistry *registry = NULL;
    ESource *source = NULL;
    ESourceExtension *component_extension = NULL;
    ESourceLocal *local_extension = NULL;
    ESource *builtin_source = NULL;
    GDBusObject *dbus_object = NULL;
    ESource *scratch_source = NULL;
    ESourceExtension *scratch_component_extension;
    ESourceLocal *scratch_local_extension;
    ESource *committed_source = NULL;
    ECalClient *client = NULL;
    GFile *custom_file = NULL;
    gchar *normalized_uri = NULL;
    gchar *trimmed_display_name = NULL;
    const gchar *uid;
    GError *error = NULL;
    int resolved;
    int result = 0;

    if (out_warning)
        *out_warning = NULL;
    if (out_error)
        *out_error = NULL;
    if (!source_uid || !*source_uid) {
        g_set_error_literal (&error, G_IO_ERROR, G_IO_ERROR_INVALID_ARGUMENT,
            "Lokale ECal-Eigenschaften benötigen eine Quell-UID");
        goto finish;
    }
    trimmed_display_name = g_strdup (display_name ? display_name : "");
    if (!*g_strstrip (trimmed_display_name)) {
        g_set_error_literal (&error, G_IO_ERROR, G_IO_ERROR_INVALID_ARGUMENT,
            "Lokale ECal-Eigenschaften benötigen einen Namen");
        goto finish;
    }
    if (!is_valid_source_color (color)) {
        g_set_error_literal (&error, G_IO_ERROR, G_IO_ERROR_INVALID_ARGUMENT,
            "Die Farbe der lokalen ECal-Quelle muss #RRGGBB sein");
        goto finish;
    }
    if (custom_file_uri) {
        gchar *scheme = g_uri_parse_scheme (custom_file_uri);
        if (!*custom_file_uri || !scheme) {
            g_free (scheme);
            g_set_error_literal (&error, G_IO_ERROR, G_IO_ERROR_INVALID_ARGUMENT,
                "Die Datei-URI der lokalen ECal-Quelle ist ungültig");
            goto finish;
        }
        g_free (scheme);
        custom_file = g_file_new_for_uri (custom_file_uri);
        normalized_uri = custom_file ? g_file_get_uri (custom_file) : NULL;
        if (!normalized_uri || !*normalized_uri) {
            g_set_error_literal (&error, G_IO_ERROR, G_IO_ERROR_INVALID_ARGUMENT,
                "Die Datei-URI der lokalen ECal-Quelle ist ungültig");
            goto finish;
        }
    }
    registry = ecal_get_source_registry_with_retry (NULL, &error);
    if (!registry)
        goto finish;
    resolved = resolve_local_source_for_properties (
        registry, source_uid, source_type, &source, &component_extension,
        &local_extension, &error);
    if (resolved == 2) {
        result = 3;
        goto finish;
    }
    if (resolved == 0)
        goto finish;
    if (!e_source_get_enabled (source)) {
        g_set_error_literal (&error, G_IO_ERROR, G_IO_ERROR_FAILED,
            "Die ECal-Quelle ist deaktiviert");
        goto finish;
    }
    if (!e_source_get_writable (source)) {
        g_set_error_literal (&error, G_IO_ERROR, G_IO_ERROR_PERMISSION_DENIED,
            "Die ECal-Quelle ist nicht schreibbar");
        goto finish;
    }
    builtin_source = ref_builtin_source_for_type (registry, source_type);
    if (custom_file && builtin_source && e_source_equal (source, builtin_source)) {
        g_set_error_literal (&error, G_IO_ERROR, G_IO_ERROR_INVALID_ARGUMENT,
            "Die eingebaute lokale ECal-Quelle verwendet keine eigene Datei");
        goto finish;
    }
    dbus_object = e_source_ref_dbus_object (source);
    scratch_source = dbus_object
        ? e_source_new (dbus_object, NULL, &error)
        : g_object_ref (source);
    if (!scratch_source)
        goto finish;
    uid = e_source_get_uid (scratch_source);
    if (!uid || g_strcmp0 (uid, source_uid) != 0) {
        g_set_error_literal (&error, G_IO_ERROR, G_IO_ERROR_FAILED,
            "EDS lieferte eine bearbeitbare Kopie für eine andere Quelle");
        goto finish;
    }
    scratch_component_extension = e_source_get_extension (
        scratch_source, source_extension_name_for_type (source_type));
    scratch_local_extension = e_source_get_extension (
        scratch_source, E_SOURCE_EXTENSION_LOCAL_BACKEND);
    if (!scratch_component_extension || !scratch_local_extension) {
        g_set_error_literal (&error, G_IO_ERROR, G_IO_ERROR_FAILED,
            "EDS konnte die lokalen Quellenerweiterungen nicht bearbeiten");
        goto finish;
    }
    e_source_set_display_name (scratch_source, display_name);
    e_source_selectable_set_color (
        E_SOURCE_SELECTABLE (scratch_component_extension), color);
    e_source_local_set_custom_file (scratch_local_extension, custom_file);
    e_source_local_set_writable (scratch_local_extension, custom_file_writable != 0);
    e_source_local_set_email_address (
        scratch_local_extension, email_address ? email_address : "");
    if (!e_source_registry_commit_source_sync (registry, scratch_source, NULL, &error))
        goto finish;
    g_clear_object (&source);
    if (!resolve_source_for_management (
            registry, source_uid, source_type, &committed_source, &error)) {
        if (out_warning)
            *out_warning = g_strdup (error && error->message ? error->message :
                "Die lokalen ECal-Eigenschaften wurden gespeichert, aber die Quelle ist nicht erreichbar");
        g_clear_error (&error);
        result = 2;
        goto finish;
    }
    if (!e_source_get_enabled (committed_source)) {
        if (out_warning)
            *out_warning = g_strdup (
                "Die lokalen ECal-Eigenschaften wurden gespeichert, aber die Quelle ist deaktiviert");
        result = 2;
        goto finish;
    }
    client = E_CAL_CLIENT (e_cal_client_connect_sync (
        committed_source, (ECalClientSourceType) source_type, 15, NULL, &error));
    if (!client) {
        if (out_warning)
            *out_warning = g_strdup (error && error->message ? error->message :
                "Die lokalen ECal-Eigenschaften wurden gespeichert, aber die Quelle ist noch nicht erreichbar");
        g_clear_error (&error);
        result = 2;
        goto finish;
    }
    result = 1;

finish:
    if (result == 0 && out_error) {
        *out_error = g_strdup (error && error->message ? error->message :
            "EDS konnte die lokalen ECal-Eigenschaften nicht speichern");
    }
    g_clear_error (&error);
    g_free (trimmed_display_name);
    g_free (normalized_uri);
    g_clear_object (&custom_file);
    g_clear_object (&client);
    g_clear_object (&committed_source);
    g_clear_object (&scratch_source);
    g_clear_object (&dbus_object);
    g_clear_object (&builtin_source);
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
evolution_eds_ecal_free_source_capabilities (
    EvolutionEdsECalSourceCapability *sources,
    size_t count)
{
    size_t ii;

    if (!sources)
        return;
    for (ii = 0; ii < count; ii++) {
        g_free (sources[ii].source_uid);
        g_free (sources[ii].source_display_name);
    }
    g_free (sources);
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

    client = E_CAL_CLIENT (e_cal_client_connect_sync (
        source, (ECalClientSourceType) source_type, 15, NULL, &error));
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
        view, E_CAL_CLIENT_VIEW_FLAGS_NOTIFY_INITIAL, NULL);
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
    watcher->registry = ecal_get_source_registry_with_retry (NULL, &error);
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
