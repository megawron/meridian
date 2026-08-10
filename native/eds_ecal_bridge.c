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
