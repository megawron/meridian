/* SPDX-License-Identifier: GPL-3.0-or-later */

#include "eds_camel_bridge.h"

#include <camel/camel.h>
#define __CAMEL_H_INSIDE__
#include <camel/camel-offline-store.h>
#undef __CAMEL_H_INSIDE__
#include <gio/gio.h>
#include <glib.h>
#include <dlfcn.h>

/* Keep the bridge's C dependency surface small.  EDS marks its public
 * headers as include-through-libedataserver.h, so define the same umbrella
 * marker while including only the source/registry APIs used here. */
#define __LIBEDATASERVER_H_INSIDE__
#include <libedataserver/e-source.h>
#include <libedataserver/e-source-authentication.h>
#include <libedataserver/e-source-backend.h>
#include <libedataserver/e-source-camel.h>
#include <libedataserver/e-source-collection.h>
#include <libedataserver/e-source-credentials-provider.h>
#include <libedataserver/e-source-goa.h>
#include <libedataserver/e-source-mail-account.h>
#include <libedataserver/e-source-mail-composition.h>
#include <libedataserver/e-source-mail-identity.h>
#include <libedataserver/e-source-mail-submission.h>
#include <libedataserver/e-source-mail-transport.h>
#include <libedataserver/e-source-registry.h>
#undef __LIBEDATASERVER_H_INSIDE__

#include <string.h>
#include <time.h>

#define DEFAULT_MESSAGES_PER_FOLDER 100

#define EVOLUTION_FOLLOW_UP_TAG "follow-up"
#define EVOLUTION_COMPLETED_ON_TAG "completed-on"
#define EVOLUTION_DUE_BY_TAG "due-by"
#define EVOLUTION_FOLLOW_UP_DEFAULT "Follow-up"

enum {
    EVOLUTION_FOLLOW_UP_NONE = 0,
    EVOLUTION_FOLLOW_UP_ACTIVE = 1,
    EVOLUTION_FOLLOW_UP_COMPLETED = 2,
};

#ifndef EDS_BRIDGE_HAS_DUP_UIDS
#define EDS_BRIDGE_HAS_DUP_UIDS 1
#endif

typedef struct _EdsCamelSession EdsCamelSession;
typedef struct _EdsCamelSessionClass EdsCamelSessionClass;

GType eds_camel_session_get_type (void);
#define EDS_TYPE_CAMEL_SESSION (eds_camel_session_get_type ())

struct _EdsCamelSession {
    CamelSession parent;
    ESourceRegistry *registry;
    ESourceCredentialsProvider *credentials_provider;
};

struct _EdsCamelSessionClass {
    CamelSessionClass parent_class;
};

G_DEFINE_TYPE (EdsCamelSession, eds_camel_session, CAMEL_TYPE_SESSION)

static ESource *
eds_camel_session_ref_ancestor_with_extension (EdsCamelSession *session,
                                               ESource *source,
                                               const gchar *extension_name);

static ESource *
eds_camel_session_ref_source (EdsCamelSession *session,
                               CamelService *service)
{
    const gchar *uid;

    if (!session->registry || !service)
        return NULL;

    uid = camel_service_get_uid (service);
    if (!uid || !*uid)
        return NULL;

    return e_source_registry_ref_source (session->registry, uid);
}

static gchar *
eds_camel_session_get_password (CamelSession *base_session,
                                 CamelService *service,
                                 const gchar *prompt,
                                 const gchar *item,
                                 guint32 flags,
                                 GError **error)
{
    EdsCamelSession *session = (EdsCamelSession *) base_session;
    ESource *source;
    ENamedParameters *credentials = NULL;
    gchar *password = NULL;
    GError *local_error = NULL;

    (void) prompt;
    (void) item;
    (void) flags;

    source = eds_camel_session_ref_source (session, service);
    if (!source)
        return NULL;

    /* 1. First, check if source or any ancestor has GNOME Online Accounts extension */
    {
        ESource *curr = g_object_ref (source);
        while (curr) {
            if (e_source_has_extension (curr, "GNOME Online Accounts")) {
                ESourceExtension *goa_ext = e_source_get_extension (curr, "GNOME Online Accounts");
                if (goa_ext) {
                    const gchar *account_id = e_source_goa_get_account_id (E_SOURCE_GOA (goa_ext));
                    if (account_id && *account_id) {
                        GDBusConnection *bus = g_bus_get_sync (G_BUS_TYPE_SESSION, NULL, NULL);
                        if (bus) {
                            gchar *obj_path = g_strdup_printf (
                                "/org/gnome/OnlineAccounts/Accounts/%s", account_id);
                            GVariant *reply = g_dbus_connection_call_sync (
                                bus, "org.gnome.OnlineAccounts", obj_path,
                                "org.gnome.OnlineAccounts.PasswordBased",
                                "GetPassword", g_variant_new ("(s)", "password"),
                                G_VARIANT_TYPE ("(s)"), G_DBUS_CALL_FLAGS_NONE,
                                5000, NULL, NULL);
                            if (reply) {
                                const gchar *goa_pass = NULL;
                                g_variant_get (reply, "(&s)", &goa_pass);
                                if (goa_pass && *goa_pass) {
                                    password = g_strdup (goa_pass);
                                    g_printerr ("[EDS] Successfully fetched GOA password for %s\n", account_id);
                                }
                                g_variant_unref (reply);
                            }
                            g_free (obj_path);
                            g_object_unref (bus);
                        }
                    }
                }
                g_object_unref (curr);
                break;
            }
            const gchar *parent_uid = e_source_get_parent (curr);
            ESource *parent = parent_uid ? e_source_registry_ref_source (session->registry, parent_uid) : NULL;
            g_object_unref (curr);
            curr = parent;
        }
    }

    /* 2. Fallback to EDS CredentialsProvider (check source and ancestors) */
    if ((!password || !*password) && session->credentials_provider) {
        ESource *curr = g_object_ref (source);
        while (curr) {
            if (e_source_credentials_provider_lookup_sync (
                    session->credentials_provider, curr, NULL,
                    &credentials, NULL) && credentials) {
                const gchar *stored = e_named_parameters_get (
                    credentials, E_SOURCE_CREDENTIAL_PASSWORD);
                if (stored && *stored) {
                    password = g_strdup (stored);
                    e_named_parameters_free (credentials);
                    g_object_unref (curr);
                    break;
                }
                e_named_parameters_free (credentials);
                credentials = NULL;
            }
            const gchar *parent_uid = e_source_get_parent (curr);
            ESource *parent = (parent_uid && *parent_uid)
                ? e_source_registry_ref_source (session->registry, parent_uid)
                : NULL;
            g_object_unref (curr);
            curr = parent;
        }
    }

    if ((!password || !*password) && local_error && error) {
        *error = local_error;
        local_error = NULL;
    }
    g_clear_error (&local_error);

    g_object_unref (source);
    return password;
}

static gboolean
eds_camel_session_get_oauth2_access_token_sync
                                           (CamelSession *base_session,
                                            CamelService *service,
                                            gchar **out_access_token,
                                            gint *out_expires_in,
                                            GCancellable *cancellable,
                                            GError **error)
{
    EdsCamelSession *session = (EdsCamelSession *) base_session;
    ESource *source;
    ESource *cred_source = NULL;
    ESource *current;
    GError *first_error = NULL;
    gboolean success = FALSE;

    source = eds_camel_session_ref_source (session, service);
    if (!source) {
        g_set_error_literal (
            error, G_IO_ERROR, G_IO_ERROR_NOT_FOUND,
            "No EDS source exists for the Camel service");
        return FALSE;
    }

    /* 1. In EDS, OAuth2 tokens for collection accounts (e.g. Google, Office 365)
     * are stored on the collection parent source rather than the child IMAP account. */
    cred_source = e_source_registry_find_extension (
        session->registry, source, E_SOURCE_EXTENSION_COLLECTION);
    if (cred_source) {
        GError *local_error = NULL;
        if (e_source_get_oauth2_access_token_sync (
                cred_source, cancellable, out_access_token, out_expires_in,
                &local_error)) {
            g_printerr ("[EDS] Obtained OAuth2 token via Collection source for %s\n",
                        camel_service_get_uid (service));
            g_clear_error (&local_error);
            g_object_unref (cred_source);
            g_object_unref (source);
            return TRUE;
        }
        if (!first_error && local_error)
            first_error = local_error;
        else
            g_clear_error (&local_error);
        g_clear_object (&cred_source);
    }

    /* 2. Try the concrete source directly, then walk the complete ancestry. */
    current = g_object_ref (source);
    while (current) {
        GError *local_error = NULL;

        if (e_source_get_oauth2_access_token_sync (
                current, cancellable, out_access_token, out_expires_in,
                &local_error)) {
            g_printerr ("[EDS] Obtained OAuth2 token via source ancestry (%s) for %s\n",
                        e_source_get_uid (current), camel_service_get_uid (service));
            g_clear_error (&local_error);
            success = TRUE;
            break;
        }

        if (!first_error && local_error)
            first_error = local_error;
        else
            g_clear_error (&local_error);

        const gchar *parent_uid = e_source_get_parent (current);
        ESource *parent = parent_uid && *parent_uid
            ? e_source_registry_ref_source (session->registry, parent_uid)
            : NULL;
        g_object_unref (current);
        current = parent;
    }
    g_clear_object (&current);

    /* 3. Direct GNOME Online Accounts fallback via D-Bus session bus */
    if (!success) {
        ESource *goa_source = eds_camel_session_ref_ancestor_with_extension (
            session, source, "GNOME Online Accounts");
        if (goa_source) {
            ESourceExtension *goa_ext = e_source_get_extension (
                goa_source, "GNOME Online Accounts");
            const gchar *account_id = goa_ext
                ? e_source_goa_get_account_id (E_SOURCE_GOA (goa_ext))
                : NULL;
            if (account_id && *account_id) {
                GDBusConnection *bus = g_bus_get_sync (G_BUS_TYPE_SESSION, NULL, NULL);
                if (bus) {
                    gchar *obj_path = g_strdup_printf (
                        "/org/gnome/OnlineAccounts/Accounts/%s", account_id);
                    GError *dbus_error = NULL;
                    GVariant *reply = g_dbus_connection_call_sync (
                        bus, "org.gnome.OnlineAccounts", obj_path,
                        "org.gnome.OnlineAccounts.OAuth2Based", "GetAccessToken",
                        NULL, G_VARIANT_TYPE ("(si)"), G_DBUS_CALL_FLAGS_NONE,
                        5000, NULL, &dbus_error);
                    if (reply) {
                        const gchar *token_str = NULL;
                        gint exp_in = 0;
                        g_variant_get (reply, "(&si)", &token_str, &exp_in);
                        if (token_str && *token_str) {
                            if (out_access_token)
                                *out_access_token = g_strdup (token_str);
                            if (out_expires_in)
                                *out_expires_in = exp_in;
                            g_printerr ("[EDS] Obtained OAuth2 token via direct GOA D-Bus for %s\n", account_id);
                            success = TRUE;
                        }
                        g_variant_unref (reply);
                    } else {
                        g_printerr ("[EDS] Direct GOA D-Bus token call failed: %s\n",
                                    dbus_error ? dbus_error->message : "unknown");
                        g_clear_error (&dbus_error);
                    }
                    g_free (obj_path);
                    g_object_unref (bus);
                }
            }
            g_object_unref (goa_source);
        }
    }

    if (!success && error && first_error) {
        *error = first_error;
        first_error = NULL;
    }
    g_clear_error (&first_error);
    if (!success && error && !*error)
        g_set_error_literal (
            error, G_IO_ERROR, G_IO_ERROR_PERMISSION_DENIED,
            "No OAuth2 access token was available for the EDS source");
    g_object_unref (source);
    return success;
}

static gboolean
eds_camel_session_authenticate_sync (CamelSession *base_session,
                                     CamelService *service,
                                     const gchar *mechanism,
                                     GCancellable *cancellable,
                                     GError **error)
{
    CamelAuthenticationResult result;

    gchar *password = camel_session_get_password (base_session, service, "Authentication", "Password", 0, NULL);
    if (password && *password) {
        camel_service_set_password (service, password);
        g_printerr ("[EDS] Set password on service for %s\n", camel_service_get_uid (service));
        g_free (password);
    }

    result = camel_service_authenticate_sync (
        service, mechanism, cancellable, error);

    if (result == CAMEL_AUTHENTICATION_ACCEPTED)
        return TRUE;

    if (error && *error == NULL)
        g_set_error_literal (
            error, G_IO_ERROR, G_IO_ERROR_PERMISSION_DENIED,
            "Camel authentication was rejected");

    return FALSE;
}

static void
eds_camel_session_dispose (GObject *object)
{
    EdsCamelSession *session = (EdsCamelSession *) object;

    g_clear_object (&session->registry);
    g_clear_object (&session->credentials_provider);

    G_OBJECT_CLASS (eds_camel_session_parent_class)->dispose (object);
}

static void
eds_camel_session_class_init (EdsCamelSessionClass *class)
{
    GObjectClass *object_class = G_OBJECT_CLASS (class);
    CamelSessionClass *session_class = CAMEL_SESSION_CLASS (class);

    object_class->dispose = eds_camel_session_dispose;
    session_class->get_password = eds_camel_session_get_password;
    session_class->get_oauth2_access_token_sync =
        eds_camel_session_get_oauth2_access_token_sync;
    session_class->authenticate_sync = eds_camel_session_authenticate_sync;
}

static void
eds_camel_session_init (EdsCamelSession *session)
{
    (void) session;
}

static gchar *
bridge_get_accounts_json_path (void)
{
    const gchar *env_data = g_getenv ("MERIDIAN_DATA_DIR");
    if (env_data && *env_data) {
        gchar *path = g_build_filename (env_data, "accounts.json", NULL);
        if (g_file_test (path, G_FILE_TEST_EXISTS))
            return path;
        g_free (path);
    }
    const gchar *env_evo = g_getenv ("EVOLUTION_GTK4_DATA_DIR");
    if (env_evo && *env_evo) {
        gchar *path = g_build_filename (env_evo, "accounts.json", NULL);
        if (g_file_test (path, G_FILE_TEST_EXISTS))
            return path;
        g_free (path);
    }

    gchar *meridian_path = g_build_filename (g_get_user_data_dir (), "meridian", "accounts.json", NULL);
    if (g_file_test (meridian_path, G_FILE_TEST_EXISTS))
        return meridian_path;

    gchar *legacy_path = g_build_filename (g_get_user_data_dir (), "evolution-gtk4", "accounts.json", NULL);
    if (g_file_test (legacy_path, G_FILE_TEST_EXISTS)) {
        g_free (meridian_path);
        return legacy_path;
    }
    g_free (legacy_path);
    return meridian_path;
}

static gchar *
bridge_get_camel_data_dir (void)
{
    const gchar *env_data = g_getenv ("MERIDIAN_DATA_DIR");
    if (env_data && *env_data)
        return g_build_filename (env_data, "camel", NULL);
    const gchar *env_evo = g_getenv ("EVOLUTION_GTK4_DATA_DIR");
    if (env_evo && *env_evo)
        return g_build_filename (env_evo, "camel", NULL);

    gchar *meridian_dir = g_build_filename (g_get_user_data_dir (), "meridian", "camel", NULL);
    gchar *legacy_dir = g_build_filename (g_get_user_data_dir (), "evolution-gtk4", "camel", NULL);
    if (!g_file_test (meridian_dir, G_FILE_TEST_IS_DIR) && g_file_test (legacy_dir, G_FILE_TEST_IS_DIR)) {
        g_free (meridian_dir);
        return legacy_dir;
    }
    g_free (legacy_dir);
    return meridian_dir;
}

static gchar *
bridge_get_camel_cache_dir (void)
{
    const gchar *env_cache = g_getenv ("MERIDIAN_CACHE_DIR");
    if (env_cache && *env_cache)
        return g_build_filename (env_cache, "camel", NULL);
    const gchar *env_evo = g_getenv ("EVOLUTION_GTK4_CACHE_DIR");
    if (env_evo && *env_evo)
        return g_build_filename (env_evo, "camel", NULL);

    gchar *meridian_dir = g_build_filename (g_get_user_cache_dir (), "meridian", "camel", NULL);
    gchar *legacy_dir = g_build_filename (g_get_user_cache_dir (), "evolution-gtk4", "camel", NULL);
    if (!g_file_test (meridian_dir, G_FILE_TEST_IS_DIR) && g_file_test (legacy_dir, G_FILE_TEST_IS_DIR)) {
        g_free (meridian_dir);
        return legacy_dir;
    }
    g_free (legacy_dir);
    return meridian_dir;
}

static EdsCamelSession *
eds_camel_session_new (ESourceRegistry *registry)
{
    EdsCamelSession *session;
    gchar *data_dir = bridge_get_camel_data_dir ();
    gchar *cache_dir = bridge_get_camel_cache_dir ();

    session = g_object_new (
        EDS_TYPE_CAMEL_SESSION,
        "user-data-dir", data_dir,
        "user-cache-dir", cache_dir,
        NULL);

    g_free (data_dir);
    g_free (cache_dir);

    session->registry = g_object_ref (registry);
    session->credentials_provider = e_source_credentials_provider_new (registry);
    camel_session_set_online (CAMEL_SESSION (session), TRUE);

    return session;
}

static GMutex global_session_lock;
static ESourceRegistry *global_registry = NULL;
static EdsCamelSession *global_session = NULL;

static ESourceRegistry *
eds_camel_ref_registry (GError **error)
{
    ESourceRegistry *res;

    g_mutex_lock (&global_session_lock);
    if (!global_registry) {
        camel_provider_init ();
        global_registry = e_source_registry_new_sync (NULL, error);
    }
    res = global_registry ? g_object_ref (global_registry) : NULL;
    g_mutex_unlock (&global_session_lock);
    return res;
}

static EdsCamelSession *
eds_camel_ref_session (ESourceRegistry *registry)
{
    EdsCamelSession *res;

    g_mutex_lock (&global_session_lock);
    if (!global_session && registry) {
        global_session = eds_camel_session_new (registry);
    }
    res = global_session ? g_object_ref (global_session) : NULL;
    g_mutex_unlock (&global_session_lock);
    return res;
}

static void
eds_camel_session_configure_service_for_source (EdsCamelSession *session,
                                                ESource *source,
                                                CamelService *service)
{
    if (!session || !session->registry || !source || !service)
        return;

    /* 1. Look for extension source matching this provider's settings type */
    if (CAMEL_IS_SERVICE (service)) {
        CamelProvider *provider = camel_service_get_provider (service);
        if (provider && provider->protocol) {
            const gchar *ext_name = e_source_camel_get_extension_name (
                provider->protocol);
            if (ext_name) {
                ESource *ext_source = e_source_registry_find_extension (
                    session->registry, source, ext_name);
                if (ext_source) {
                    e_source_camel_configure_service (ext_source, service);
                    g_object_unref (ext_source);
                }
            }
        }
    }

    /* 2. Apply configuration from the oldest ancestor to the concrete source.
     * Identity, GOA/provider and mail-account extensions may each live on a
     * different ESource in the same hierarchy. */
    GPtrArray *ancestry = g_ptr_array_new_with_free_func (g_object_unref);
    ESource *current = g_object_ref (source);
    while (current) {
        g_ptr_array_add (ancestry, current);

        const gchar *parent_uid = e_source_get_parent (current);
        current = parent_uid && *parent_uid
            ? e_source_registry_ref_source (session->registry, parent_uid)
            : NULL;
    }

    for (gssize index = (gssize) ancestry->len - 1; index >= 0; index--)
        e_source_camel_configure_service (
            E_SOURCE (g_ptr_array_index (ancestry, (guint) index)), service);

    g_ptr_array_unref (ancestry);
}

static ESource *
eds_camel_session_ref_ancestor_with_extension (EdsCamelSession *session,
                                               ESource *source,
                                               const gchar *extension_name)
{
    ESource *current;

    if (!session || !session->registry || !source ||
        !extension_name || !*extension_name)
        return NULL;

    current = g_object_ref (source);
    while (current) {
        if (e_source_has_extension (current, extension_name)) {
            if (g_str_equal (extension_name, "Ews Backend")) {
                ESourceExtension *ext = e_source_get_extension (current, "Ews Backend");
                gchar *hurl = NULL;
                g_object_get (ext, "hosturl", &hurl, NULL);
                if (hurl && *hurl) {
                    g_free (hurl);
                    return current;
                }
                g_free (hurl);
            } else if (g_str_equal (extension_name, "Authentication")) {
                ESourceExtension *ext = e_source_get_extension (current, "Authentication");
                const gchar *host = e_source_authentication_get_host (E_SOURCE_AUTHENTICATION (ext));
                if (host && *host)
                    return current;
            } else if (g_str_equal (extension_name, "GNOME Online Accounts")) {
                ESourceExtension *ext = e_source_get_extension (current, "GNOME Online Accounts");
                const gchar *acc_id = e_source_goa_get_account_id (E_SOURCE_GOA (ext));
                if (acc_id && *acc_id)
                    return current;
            } else {
                return current;
            }
        }

        const gchar *parent_uid = e_source_get_parent (current);
        ESource *parent = parent_uid && *parent_uid
            ? e_source_registry_ref_source (session->registry, parent_uid)
            : NULL;
        g_object_unref (current);
        current = parent;
    }

    return NULL;
}

typedef struct {
    EvolutionEdsCamelMessage *messages;
    size_t count;
    size_t capacity;
    EvolutionEdsCamelFolder *folders;
    size_t folder_count;
    size_t folder_capacity;
    size_t configured_sources;
    size_t successful_sources;
    size_t failed_sources;
    gint message_offset;
    gint max_messages_per_folder;
    gchar *first_error;
    EvolutionEdsCamelProgressCallback progress_callback;
    void *progress_user_data;
    EvolutionEdsCamelCancellationCallback cancellation_callback;
    void *cancellation_user_data;
    gboolean cancelled;
    size_t folders_seen;
    size_t messages_loaded;
    size_t processed_sources;
} LoadContext;

typedef struct {
    GCancellable *cancellable;
    EvolutionEdsCamelCancellationCallback callback;
    void *user_data;
    gint stop;
    GThread *thread;
} CancellationMonitor;

static gboolean
load_cancelled (LoadContext *context)
{
    if (context->cancelled)
        return TRUE;

    if (context->cancellation_callback &&
        context->cancellation_callback (context->cancellation_user_data)) {
        context->cancelled = TRUE;
        return TRUE;
    }

    return FALSE;
}

static gboolean
cancellation_requested (EvolutionEdsCamelCancellationCallback callback,
                        void *user_data)
{
    return callback && callback (user_data);
}

static gboolean
set_cancelled_error (GError **error)
{
    g_set_error_literal (
        error, G_IO_ERROR, G_IO_ERROR_CANCELLED,
        "EDS/Camel-Änderung wurde abgebrochen");
    return FALSE;
}

static gpointer
cancellation_monitor_thread (gpointer user_data)
{
    CancellationMonitor *monitor = user_data;

    while (!g_atomic_int_get (&monitor->stop)) {
        if (monitor->callback && monitor->callback (monitor->user_data)) {
            g_cancellable_cancel (monitor->cancellable);
            break;
        }
        g_usleep (20 * 1000);
    }

    return NULL;
}

static CancellationMonitor *
cancellation_monitor_start (
    GCancellable *cancellable,
    EvolutionEdsCamelCancellationCallback callback,
    void *user_data)
{
    CancellationMonitor *monitor;

    if (!callback)
        return NULL;

    monitor = g_new0 (CancellationMonitor, 1);
    monitor->cancellable = g_object_ref (cancellable);
    monitor->callback = callback;
    monitor->user_data = user_data;
    monitor->stop = 0;
    monitor->thread = g_thread_new (
        "evolution-camel-cancel", cancellation_monitor_thread, monitor);

    return monitor;
}

static void
cancellation_monitor_free (CancellationMonitor *monitor)
{
    if (!monitor)
        return;

    g_atomic_int_set (&monitor->stop, 1);
    if (monitor->thread)
        g_thread_join (monitor->thread);
    g_clear_object (&monitor->cancellable);
    g_free (monitor);
}

static void
emit_progress (LoadContext *context,
               EvolutionEdsCamelSyncPhase phase,
               const gchar *source_uid,
               const gchar *source_display_name,
               const gchar *folder_name)
{
    EvolutionEdsCamelSyncProgress progress;

    if (!context->progress_callback)
        return;

    progress.phase = phase;
    progress.source_uid = source_uid;
    progress.source_display_name = source_display_name;
    progress.folder_name = folder_name;
    progress.configured_sources = context->configured_sources;
    progress.processed_sources = context->processed_sources;
    progress.successful_sources = context->successful_sources;
    progress.failed_sources = context->failed_sources;
    progress.folders_seen = context->folders_seen;
    progress.messages_loaded = context->messages_loaded;

    context->progress_callback (&progress, context->progress_user_data);
}

static void
remember_error (LoadContext *context,
                const gchar *scope,
                GError *error)
{
    if (!context->first_error && error) {
        context->first_error = g_strdup_printf (
            "%s: %s", scope ? scope : "EDS/Camel", error->message);
    }
}

static gboolean
ensure_capacity (LoadContext *context)
{
    size_t new_capacity;

    if (context->count < context->capacity)
        return TRUE;

    new_capacity = context->capacity ? context->capacity * 2 : 32;
    context->messages = g_realloc_n (
        context->messages, new_capacity, sizeof (*context->messages));
    context->capacity = new_capacity;

    return TRUE;
}

static gboolean
ensure_folder_capacity (LoadContext *context)
{
    size_t new_capacity;

    if (context->folder_count < context->folder_capacity)
        return TRUE;

    new_capacity = context->folder_capacity
        ? context->folder_capacity * 2
        : 32;
    context->folders = g_realloc_n (
        context->folders, new_capacity, sizeof (*context->folders));
    context->folder_capacity = new_capacity;

    return TRUE;
}

static const gchar *
folder_id_from_flags (CamelFolderInfoFlags flags)
{
    switch (flags & CAMEL_FOLDER_TYPE_MASK) {
    case CAMEL_FOLDER_TYPE_INBOX:
        return "inbox";
    case CAMEL_FOLDER_TYPE_SENT:
        return "sent";
    case CAMEL_FOLDER_TYPE_DRAFTS:
        return "drafts";
    case CAMEL_FOLDER_TYPE_TRASH:
        return "trash";
    case CAMEL_FOLDER_TYPE_JUNK:
        return "junk";
    case CAMEL_FOLDER_TYPE_ARCHIVE:
        return "archive";
    default:
        return "all";
    }
}

static gboolean
is_mail_folder (CamelFolderInfo *folder_info)
{
    CamelFolderInfoFlags type;

    if (!folder_info)
        return FALSE;

    if (folder_info->full_name) {
        if (folder_info->full_name[0] == '.' || strstr (folder_info->full_name, "/."))
            return FALSE;
    }

    if (folder_info->display_name && folder_info->display_name[0] == '.')
        return FALSE;

    type = folder_info->flags & CAMEL_FOLDER_TYPE_MASK;
    return type != CAMEL_FOLDER_TYPE_CONTACTS &&
           type != CAMEL_FOLDER_TYPE_EVENTS &&
           type != CAMEL_FOLDER_TYPE_MEMOS &&
           type != CAMEL_FOLDER_TYPE_TASKS;
}

static void
append_folder (LoadContext *context,
               const gchar *source_uid,
               const gchar *source_display_name,
               CamelFolderInfo *folder_info)
{
    EvolutionEdsCamelFolder *record;

    if (!is_mail_folder (folder_info) ||
        !folder_info->full_name || !*folder_info->full_name)
        return;

    ensure_folder_capacity (context);
    record = &context->folders[context->folder_count++];
    record->source_uid = g_strdup (source_uid);
    record->source_display_name = g_strdup (
        source_display_name && *source_display_name
            ? source_display_name
            : source_uid);
    record->folder_name = g_strdup (folder_info->full_name);
    record->folder_id = g_strdup (folder_id_from_flags (folder_info->flags));
    record->display_name = g_strdup (
        folder_info->display_name ?: folder_info->full_name);
    record->unread_count = folder_info->unread > 0
        ? (guint32) folder_info->unread
        : 0;
    record->total_count = folder_info->total > 0
        ? (guint32) folder_info->total
        : 0;
    record->is_selectable =
        (folder_info->flags & CAMEL_FOLDER_NOSELECT) == 0;
}

static gchar *
dup_nonempty_user_tag (CamelMessageInfo *message_info,
                       const gchar *tag_name)
{
    const gchar *value;

    if (!message_info || !tag_name)
        return NULL;

    value = camel_message_info_get_user_tag (message_info, tag_name);
    return value && *value ? g_strdup (value) : NULL;
}

static void
read_follow_up_tags (CamelMessageInfo *message_info,
                     EvolutionEdsCamelMessage *record)
{
    record->follow_up = dup_nonempty_user_tag (
        message_info, EVOLUTION_FOLLOW_UP_TAG);
    record->completed_on = dup_nonempty_user_tag (
        message_info, EVOLUTION_COMPLETED_ON_TAG);
    record->due_by = dup_nonempty_user_tag (
        message_info, EVOLUTION_DUE_BY_TAG);

    if (!record->follow_up) {
        record->follow_up_status = EVOLUTION_FOLLOW_UP_NONE;
    } else if (record->completed_on) {
        record->follow_up_status = EVOLUTION_FOLLOW_UP_COMPLETED;
    } else {
        record->follow_up_status = EVOLUTION_FOLLOW_UP_ACTIVE;
    }
}

static gboolean
append_message (LoadContext *context,
                const gchar *source_uid,
                CamelFolderInfo *folder_info,
                const gchar *message_uid,
                CamelMessageInfo *message_info,
                CamelMimeMessage *message,
                GCancellable *cancellable)
{
    GMemoryOutputStream *output;
    GError *error = NULL;
    gssize written;
    gsize data_size;
    gconstpointer data;
    EvolutionEdsCamelMessage *record;

    output = G_MEMORY_OUTPUT_STREAM (g_memory_output_stream_new_resizable ());
    written = camel_data_wrapper_write_to_output_stream_sync (
        CAMEL_DATA_WRAPPER (message),
        G_OUTPUT_STREAM (output),
        cancellable,
        &error);
    if (written < 0) {
        remember_error (context, folder_info->full_name, error);
        g_clear_error (&error);
        g_object_unref (output);
        return FALSE;
    }

    data_size = g_memory_output_stream_get_data_size (output);
    data = g_memory_output_stream_get_data (output);
    if (data_size == 0 || data == NULL) {
        g_object_unref (output);
        return FALSE;
    }

    ensure_capacity (context);
    record = &context->messages[context->count++];
    record->source_uid = g_strdup (source_uid);
    record->folder_name = g_strdup (folder_info->full_name ?: "");
    record->folder_id = g_strdup (folder_id_from_flags (folder_info->flags));
    record->message_uid = g_strdup (message_uid);
    record->raw_message = g_memdup2 (data, data_size);
    record->raw_message_len = data_size;
    record->is_unread = 1;
    record->is_flagged = 0;
    record->is_replied = 0;
    record->is_forwarded = 0;
    record->follow_up_status = EVOLUTION_FOLLOW_UP_NONE;
    record->follow_up = NULL;
    record->completed_on = NULL;
    record->due_by = NULL;

    if (message_info) {
        guint32 flags = camel_message_info_get_flags (message_info);

        record->is_unread = (flags & CAMEL_MESSAGE_SEEN) == 0;
        record->is_flagged = (flags & CAMEL_MESSAGE_FLAGGED) != 0;
        record->is_replied = (flags & (CAMEL_MESSAGE_ANSWERED | CAMEL_MESSAGE_ANSWERED_ALL)) != 0;
        record->is_forwarded = (flags & CAMEL_MESSAGE_FORWARDED) != 0;
        read_follow_up_tags (message_info, record);
    }

    context->messages_loaded++;
    g_object_unref (output);
    return TRUE;
}

static gboolean
append_message_from_info (LoadContext *context,
                          const gchar *source_uid,
                          CamelFolderInfo *folder_info,
                          const gchar *message_uid,
                          CamelMessageInfo *message_info)
{
    GString *rfc822;
    const gchar *subject, *from, *to, *cc, *preview;
    gint64 date_sent;
    GDateTime *dt;
    gchar *date_str = NULL;
    guint32 flags;
    EvolutionEdsCamelMessage *record;

    if (!message_info)
        return FALSE;

    subject = camel_message_info_get_subject (message_info);
    from = camel_message_info_get_from (message_info);
    to = camel_message_info_get_to (message_info);
    cc = camel_message_info_get_cc (message_info);
    preview = camel_message_info_get_preview (message_info);
    date_sent = camel_message_info_get_date_sent (message_info);

    rfc822 = g_string_sized_new (512);
    g_string_append_printf (rfc822, "Message-ID: <%s@eds>\r\n", message_uid);
    g_string_append (rfc822, "X-Evolution-Preview-Only: 1\r\n");
    if (from && *from)
        g_string_append_printf (rfc822, "From: %s\r\n", from);
    if (to && *to)
        g_string_append_printf (rfc822, "To: %s\r\n", to);
    if (cc && *cc)
        g_string_append_printf (rfc822, "Cc: %s\r\n", cc);
    if (subject && *subject)
        g_string_append_printf (rfc822, "Subject: %s\r\n", subject);

    if (date_sent > 0) {
        date_str = camel_header_format_date (date_sent, 0);
        if (!date_str) {
            static const char *const days[] = {"Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"};
            static const char *const months[] = {"Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"};
            dt = g_date_time_new_from_unix_utc (date_sent);
            if (dt) {
                int d = g_date_time_get_day_of_week (dt);
                int m = g_date_time_get_month (dt);
                date_str = g_strdup_printf ("%s, %02d %s %04d %02d:%02d:%02d +0000",
                    days[d % 7],
                    g_date_time_get_day_of_month (dt),
                    months[(m - 1) % 12],
                    g_date_time_get_year (dt),
                    g_date_time_get_hour (dt),
                    g_date_time_get_minute (dt),
                    g_date_time_get_second (dt));
                g_date_time_unref (dt);
            }
        }
    }
    if (date_str) {
        g_string_append_printf (rfc822, "Date: %s\r\n", date_str);
        g_free (date_str);
    }

    g_string_append (rfc822, "MIME-Version: 1.0\r\n");
    g_string_append (rfc822, "Content-Type: text/plain; charset=utf-8\r\n");
    g_string_append (rfc822, "\r\n");
    if (preview && *preview)
        g_string_append (rfc822, preview);

    ensure_capacity (context);
    record = &context->messages[context->count++];
    record->source_uid = g_strdup (source_uid);
    record->folder_name = g_strdup (folder_info->full_name ?: "");
    record->folder_id = g_strdup (folder_id_from_flags (folder_info->flags));
    record->message_uid = g_strdup (message_uid);
    record->raw_message_len = rfc822->len;
    record->raw_message = (guchar *) g_string_free (rfc822, FALSE);
    record->is_unread = 1;
    record->is_flagged = 0;
    record->is_replied = 0;
    record->is_forwarded = 0;
    record->follow_up_status = EVOLUTION_FOLLOW_UP_NONE;
    record->follow_up = NULL;
    record->completed_on = NULL;
    record->due_by = NULL;

    flags = camel_message_info_get_flags (message_info);
    record->is_unread = (flags & CAMEL_MESSAGE_SEEN) == 0;
    record->is_flagged = (flags & CAMEL_MESSAGE_FLAGGED) != 0;
    record->is_replied = (flags & (CAMEL_MESSAGE_ANSWERED | CAMEL_MESSAGE_ANSWERED_ALL)) != 0;
    record->is_forwarded = (flags & CAMEL_MESSAGE_FORWARDED) != 0;
    read_follow_up_tags (message_info, record);

    context->messages_loaded++;
    return TRUE;
}

typedef struct {
    const gchar *uid;
    gint64 date;
} MessageSortEntry;

static gint
compare_message_sort_entries_desc (gconstpointer a, gconstpointer b)
{
    const MessageSortEntry *ea = (const MessageSortEntry *) a;
    const MessageSortEntry *eb = (const MessageSortEntry *) b;

    if (ea->date > eb->date)
        return -1;
    if (ea->date < eb->date)
        return 1;
    return 0;
}

static void
load_folder (LoadContext *context,
             CamelStore *store,
             const gchar *source_uid,
             const gchar *source_display_name,
             const gchar *folder_name,
    CamelFolderInfo *folder_info,
    GCancellable *cancellable)
{
    CamelFolderInfo *current;

    for (current = folder_info; current != NULL; current = current->next) {
        CamelFolder *folder = NULL;
        GPtrArray *uids = NULL;
        GError *error = NULL;

        if (load_cancelled (context))
            return;

        append_folder (
            context, source_uid, source_display_name, current);

        if (is_mail_folder (current)) {
            context->folders_seen++;
        }

        gboolean should_load_messages = FALSE;
        if (is_mail_folder (current) && (current->flags & CAMEL_FOLDER_NOSELECT) == 0) {
            if (folder_name && current->full_name) {
                should_load_messages = (g_ascii_strcasecmp (current->full_name, folder_name) == 0);
            } else if (!folder_name) {
                gboolean is_foreign = current->full_name && g_str_has_prefix (current->full_name, "Foreign Folders");
                if (!is_foreign) {
                    should_load_messages = ((current->flags & CAMEL_FOLDER_TYPE_MASK) == CAMEL_FOLDER_TYPE_INBOX) ||
                        (current->full_name && (g_ascii_strcasecmp (current->full_name, "INBOX") == 0 ||
                                                g_ascii_strcasecmp (current->full_name, "Inbox") == 0 ||
                                                g_ascii_strcasecmp (current->full_name, "Posteingang") == 0 ||
                                                g_str_has_suffix (current->full_name, "/Posteingang") ||
                                                g_str_has_suffix (current->full_name, "/INBOX") ||
                                                g_str_has_suffix (current->full_name, "/Inbox")));
                }
            }
        }

        if (should_load_messages) {
            emit_progress (
                context, EVOLUTION_EDS_CAMEL_SYNC_LOADING_MESSAGES,
                source_uid, source_display_name, current->full_name);

            folder = camel_store_get_folder_sync (
                store, current->full_name, CAMEL_STORE_FOLDER_NONE,
                cancellable, &error);
            if (!folder) {
                remember_error (context, current->full_name, error);
                g_clear_error (&error);
            } else {
                CamelFolderSummary *summary = camel_folder_get_folder_summary (folder);
                if (summary) {
                    camel_folder_summary_prepare_fetch_all (summary, NULL);
                }

                /* Synchronize folder info from server to pull unread and new messages */
                GError *refresh_err = NULL;
                camel_folder_refresh_info_sync (folder, cancellable, &refresh_err);
                if (refresh_err) {
                    g_printerr ("[EDS] Folder refresh info warning on '%s': %s\n",
                                current->full_name ? current->full_name : "",
                                refresh_err->message);
                    g_clear_error (&refresh_err);
                }

                #if EDS_BRIDGE_HAS_DUP_UIDS
                uids = camel_folder_dup_uids (folder);
                #else
                uids = camel_folder_get_uids (folder);
                #endif
                if (uids && uids->len > 0) {
                    MessageSortEntry *sort_entries = g_new0 (MessageSortEntry, uids->len);
                    for (guint i = 0; i < uids->len; i++) {
                        const gchar *message_uid = g_ptr_array_index (uids, i);
                        sort_entries[i].uid = message_uid;
                        if (message_uid) {
                            CamelMessageInfo *mi = camel_folder_get_message_info (folder, message_uid);
                            if (mi) {
                                gint64 date = camel_message_info_get_date_sent (mi);
                                if (date <= 0)
                                    date = camel_message_info_get_date_received (mi);
                                sort_entries[i].date = date;
                                g_object_unref (mi);
                            }
                        }
                    }

                    qsort (sort_entries, uids->len, sizeof (MessageSortEntry), compare_message_sort_entries_desc);

                    gsize index = (gsize) context->message_offset;
                    gsize loaded = 0;

                    while (index < (gsize) uids->len &&
                           !load_cancelled (context) &&
                           (context->max_messages_per_folder <= 0 ||
                            loaded < (gsize) context->max_messages_per_folder)) {
                        const gchar *message_uid = sort_entries[index].uid;
                        CamelMessageInfo *message_info;

                        index++;
                        if (!message_uid)
                            continue;

                        message_info = camel_folder_get_message_info (
                            folder, message_uid);
                        if (message_info) {
                            if (append_message_from_info (
                                    context, source_uid, current, message_uid,
                                    message_info))
                                loaded++;
                            g_object_unref (message_info);
                        }
                    }

                    g_free (sort_entries);

                    #if EDS_BRIDGE_HAS_DUP_UIDS
                    g_ptr_array_unref (uids);
                    #else
                    camel_folder_free_uids (folder, uids);
                    #endif
                }

                g_object_unref (folder);
            }
        }

        if (load_cancelled (context))
            return;

        if (current->child)
            load_folder (
                context, store, source_uid, source_display_name,
                folder_name, current->child, cancellable);
    }
}

static void
configure_ews_service_credentials_and_settings (EdsCamelSession *session,
                                                ESource *source,
                                                CamelService *service,
                                                const gchar *backend_name,
                                                const gchar *source_uid)
{
    if (!session || !source || !service)
        return;

    eds_camel_session_configure_service_for_source (session, source, service);

    if (backend_name && g_str_equal (backend_name, "ews")) {
        CamelSettings *settings = camel_service_ref_settings (service);
        if (settings) {
            GParamSpec *pspec_hosturl = g_object_class_find_property (
                G_OBJECT_GET_CLASS (settings), "hosturl");
            GParamSpec *pspec_oaburl = g_object_class_find_property (
                G_OBJECT_GET_CLASS (settings), "oaburl");
            GParamSpec *pspec_host = g_object_class_find_property (
                G_OBJECT_GET_CLASS (settings), "host");
            GParamSpec *pspec_email = g_object_class_find_property (
                G_OBJECT_GET_CLASS (settings), "email");
            GParamSpec *pspec_user = g_object_class_find_property (
                G_OBJECT_GET_CLASS (settings), "user");
            GParamSpec *pspec_use_imp = g_object_class_find_property (
                G_OBJECT_GET_CLASS (settings), "use-impersonation");
            GParamSpec *pspec_imp_user = g_object_class_find_property (
                G_OBJECT_GET_CLASS (settings), "impersonate-user");

            ESource *ews_source =
                eds_camel_session_ref_ancestor_with_extension (
                    session, source, "Ews Backend");
            if (!ews_source && e_source_has_extension (source, "Ews Backend"))
                ews_source = g_object_ref (source);

            if (ews_source) {
                ESourceExtension *ews_ext = e_source_get_extension (ews_source, "Ews Backend");
                if (ews_ext) {
                    gchar *ext_hurl = NULL;
                    gchar *ext_oab = NULL;
                    gchar *ext_email = NULL;
                    gboolean ext_use_imp = FALSE;
                    gchar *ext_imp_user = NULL;

                    GParamSpec *hpspec = g_object_class_find_property (G_OBJECT_GET_CLASS (ews_ext), "hosturl");
                    if (hpspec)
                        g_object_get (ews_ext, "hosturl", &ext_hurl, NULL);
                    GParamSpec *oab_spec = g_object_class_find_property (G_OBJECT_GET_CLASS (ews_ext), "oaburl");
                    if (oab_spec)
                        g_object_get (ews_ext, "oaburl", &ext_oab, NULL);
                    GParamSpec *mail_spec = g_object_class_find_property (G_OBJECT_GET_CLASS (ews_ext), "email");
                    if (mail_spec)
                        g_object_get (ews_ext, "email", &ext_email, NULL);

                    GParamSpec *use_imp_spec = g_object_class_find_property (G_OBJECT_GET_CLASS (ews_ext), "use-impersonation");
                    if (use_imp_spec)
                        g_object_get (ews_ext, "use-impersonation", &ext_use_imp, NULL);
                    GParamSpec *imp_user_spec = g_object_class_find_property (G_OBJECT_GET_CLASS (ews_ext), "impersonate-user");
                    if (imp_user_spec)
                        g_object_get (ews_ext, "impersonate-user", &ext_imp_user, NULL);

                    if (ext_hurl && *ext_hurl && pspec_hosturl) {
                        g_object_set (settings, "hosturl", ext_hurl, NULL);
                        g_printerr ("[EDS] Explicitly set hosturl from Ews Backend: %s\n", ext_hurl);
                    }
                    if (ext_oab && *ext_oab && pspec_oaburl)
                        g_object_set (settings, "oaburl", ext_oab, NULL);
                    if (ext_email && *ext_email && pspec_email)
                        g_object_set (settings, "email", ext_email, NULL);

                    if (pspec_use_imp)
                        g_object_set (settings, "use-impersonation", ext_use_imp, NULL);
                    if (pspec_imp_user) {
                        if (ext_imp_user && *ext_imp_user) {
                            g_object_set (settings, "impersonate-user", ext_imp_user, NULL);
                        } else if (ext_use_imp && ext_email && *ext_email) {
                            g_object_set (settings, "impersonate-user", ext_email, NULL);
                        }
                    }

                    g_printerr ("[EDS] EWS settings configured: hosturl=%s email=%s use_impersonation=%d impersonate_user=%s\n",
                                ext_hurl ? ext_hurl : "(none)",
                                ext_email ? ext_email : "(none)",
                                ext_use_imp,
                                ext_imp_user ? ext_imp_user : "");

                    g_free (ext_hurl);
                    g_free (ext_oab);
                    g_free (ext_email);
                    g_free (ext_imp_user);
                }
                g_object_unref (ews_source);
            }

            gchar *current_url = NULL;
            if (pspec_hosturl)
                g_object_get (settings, "hosturl", &current_url, NULL);

            if (!current_url || !*current_url || !g_str_has_prefix (current_url, "http")) {
                g_clear_pointer (&current_url, g_free);
                ESource *goa_source =
                    eds_camel_session_ref_ancestor_with_extension (
                        session, source, "GNOME Online Accounts");
                if (goa_source) {
                    ESourceExtension *goa_ext = e_source_get_extension (
                        goa_source, "GNOME Online Accounts");
                    if (goa_ext) {
                        const gchar *account_id = e_source_goa_get_account_id (E_SOURCE_GOA (goa_ext));
                        if (account_id && *account_id) {
                            GDBusConnection *bus = g_bus_get_sync (G_BUS_TYPE_SESSION, NULL, NULL);
                            if (bus) {
                                gchar *obj_path = g_strdup_printf (
                                    "/org/gnome/OnlineAccounts/Accounts/%s", account_id);
                                GVariant *reply = g_dbus_connection_call_sync (
                                    bus, "org.gnome.OnlineAccounts", obj_path,
                                    "org.freedesktop.DBus.Properties", "Get",
                                    g_variant_new ("(ss)", "org.gnome.OnlineAccounts.Exchange", "Host"),
                                    G_VARIANT_TYPE ("(v)"), G_DBUS_CALL_FLAGS_NONE, 3000, NULL, NULL);
                                if (reply) {
                                    GVariant *var = NULL;
                                    g_variant_get (reply, "(v)", &var);
                                    if (var) {
                                        const gchar *host = g_variant_get_string (var, NULL);
                                        if (host && *host) {
                                            gchar *ews_url = g_strdup_printf ("https://%s/EWS/Exchange.asmx", host);
                                            g_printerr ("[EDS] Resolved GOA Exchange host for %s: %s (URL: %s)\n",
                                                        e_source_get_display_name (source), host, ews_url);
                                            if (pspec_hosturl)
                                                g_object_set (settings, "hosturl", ews_url, NULL);
                                            if (pspec_host)
                                                g_object_set (settings, "host", host, NULL);
                                            current_url = ews_url;
                                        }
                                        g_variant_unref (var);
                                    }
                                    g_variant_unref (reply);
                                }
                                g_free (obj_path);
                                g_object_unref (bus);
                            }
                        }
                    }
                    g_object_unref (goa_source);
                }
            }

            if (!current_url || !*current_url || !g_str_has_prefix (current_url, "http")) {
                g_clear_pointer (&current_url, g_free);
                ESource *auth_source =
                    eds_camel_session_ref_ancestor_with_extension (
                        session, source, "Authentication");
                if (auth_source) {
                    ESourceExtension *auth_ext = e_source_get_extension (auth_source, "Authentication");
                    if (auth_ext) {
                        const gchar *host = e_source_authentication_get_host (E_SOURCE_AUTHENTICATION (auth_ext));
                        if (host && *host) {
                            gchar *ews_url = g_str_has_prefix (host, "http")
                                ? g_strdup (host)
                                : g_strdup_printf ("https://%s/EWS/Exchange.asmx", host);
                            if (pspec_hosturl)
                                g_object_set (settings, "hosturl", ews_url, NULL);
                            if (pspec_host)
                                g_object_set (settings, "host", host, NULL);
                            g_printerr ("[EDS] Configured EWS hosturl from Authentication host: %s\n", ews_url);
                            current_url = ews_url;
                        }
                    }
                    g_object_unref (auth_source);
                }
            }
            g_free (current_url);

            /* Ensure user, email, and auth-mechanism are set on settings */
            gchar *curr_user = NULL;
            gchar *curr_auth = NULL;
            g_object_get (settings, "user", &curr_user, "auth-mechanism", &curr_auth, NULL);

            if (!curr_user || !*curr_user || !curr_auth || !*curr_auth) {
                ESource *auth_source =
                    eds_camel_session_ref_ancestor_with_extension (
                        session, source, "Authentication");
                ESourceExtension *auth_ext = auth_source
                    ? e_source_get_extension (auth_source, "Authentication")
                    : NULL;
                ESource *goa_source =
                    eds_camel_session_ref_ancestor_with_extension (
                        session, source, "GNOME Online Accounts");

                if (auth_ext) {
                    const gchar *user = e_source_authentication_get_user (E_SOURCE_AUTHENTICATION (auth_ext));
                    const gchar *method = e_source_authentication_get_method (E_SOURCE_AUTHENTICATION (auth_ext));
                    gchar *dup_user = NULL;

                    if ((!user || !*user) && goa_source) {
                        ESourceExtension *goa = e_source_get_extension (
                            goa_source, "GNOME Online Accounts");
                        dup_user = e_source_goa_dup_address (E_SOURCE_GOA (goa));
                        user = dup_user;
                    }
                    gchar *col_identity = NULL;
                    gchar *ews_email = NULL;
                    if ((!user || !*user) && auth_source && e_source_has_extension (auth_source, "Collection")) {
                        ESourceExtension *col = e_source_get_extension (auth_source, "Collection");
                        if (g_object_class_find_property (G_OBJECT_GET_CLASS (col), "identity")) {
                            g_object_get (col, "identity", &col_identity, NULL);
                            if (col_identity && *col_identity)
                                user = col_identity;
                        }
                    }
                    if ((!user || !*user) && auth_source && e_source_has_extension (auth_source, "Ews Backend")) {
                        ESourceExtension *ews = e_source_get_extension (auth_source, "Ews Backend");
                        if (g_object_class_find_property (G_OBJECT_GET_CLASS (ews), "email")) {
                            g_object_get (ews, "email", &ews_email, NULL);
                            if (ews_email && *ews_email)
                                user = ews_email;
                        }
                    }
                    if (user && *user && (!curr_user || !*curr_user)) {
                        g_object_set (settings, "user", user, "email", user, NULL);
                        g_printerr ("[EDS] Configured CamelSettings user/email = '%s'\n", user);
                    }
                    if (!method || !*method || g_str_equal (method, "none"))
                        method = "NTLM";

                    if (method && *method && (!curr_auth || !*curr_auth)) {
                        g_object_set (settings, "auth-mechanism", method, NULL);
                        g_printerr ("[EDS] Configured CamelSettings auth-mechanism = '%s'\n", method);
                    }
                    g_free (dup_user);
                    g_free (col_identity);
                    g_free (ews_email);
                }
                g_clear_object (&goa_source);
                g_clear_object (&auth_source);
            }
            g_free (curr_user);
            g_free (curr_auth);

            g_object_unref (settings);
        }
    }
}

static gchar *
bridge_json_extract_string (const gchar *block, const gchar *key)
{
    gchar *pattern = g_strdup_printf ("\"%s\"", key);
    const gchar *p = strstr (block, pattern);
    g_free (pattern);
    if (!p)
        return NULL;
    const gchar *colon = strchr (p, ':');
    if (!colon)
        return NULL;
    const gchar *q1 = strchr (colon, '"');
    if (!q1)
        return NULL;
    q1++;
    const gchar *q2 = strchr (q1, '"');
    if (!q2)
        return NULL;
    return g_strndup (q1, q2 - q1);
}

static gboolean
bridge_json_extract_bool (const gchar *block, const gchar *key)
{
    gchar *pat_true1 = g_strdup_printf ("\"%s\": true", key);
    gchar *pat_true2 = g_strdup_printf ("\"%s\":true", key);
    gboolean val = (strstr (block, pat_true1) != NULL || strstr (block, pat_true2) != NULL);
    g_free (pat_true1);
    g_free (pat_true2);
    return val;
}

static gboolean
is_source_foreign_or_shared (ESource *source)
{
    if (!source)
        return FALSE;

    const gchar *source_uid = e_source_get_uid (source);
    const gchar *display_name = e_source_get_display_name (source);

    /* 1. Check if the source has [Exchange Web Services Folder] extension with foreign=TRUE */
    if (e_source_has_extension (source, "Exchange Web Services Folder")) {
        ESourceExtension *ews_ext = e_source_get_extension (source, "Exchange Web Services Folder");
        if (ews_ext && g_object_class_find_property (G_OBJECT_GET_CLASS (ews_ext), "foreign")) {
            gboolean is_foreign = FALSE;
            g_object_get (ews_ext, "foreign", &is_foreign, NULL);
            if (is_foreign)
                return TRUE;
        }
    }

    /* 2. Check accounts.json for is_shared_mailbox */
    gchar *accounts_path = bridge_get_accounts_json_path ();
    if (g_file_test (accounts_path, G_FILE_TEST_EXISTS)) {
        gchar *json = NULL;
        if (g_file_get_contents (accounts_path, &json, NULL, NULL) && json) {
            const gchar *accounts_start = strstr (json, "\"accounts\"");
            const gchar *p = accounts_start ? accounts_start : json;
            while ((p = strchr (p, '{')) != NULL) {
                if (p == json) {
                    p++;
                    continue;
                }
                const gchar *block_start = p;
                const gchar *block_end = strchr (block_start, '}');
                if (!block_end)
                    break;
                gchar *block = g_strndup (block_start, block_end - block_start + 1);
                p = block_end + 1;

                gboolean is_shared = bridge_json_extract_bool (block, "is_shared_mailbox") ||
                                     bridge_json_extract_bool (block, "open_mailbox_other_user");
                if (is_shared) {
                    gchar *email = bridge_json_extract_string (block, "email");
                    gchar *dname = bridge_json_extract_string (block, "display_name");
                    gchar *id = bridge_json_extract_string (block, "id");

                    gboolean match = FALSE;
                    if (email && *email) {
                        GChecksum *ck = g_checksum_new (G_CHECKSUM_SHA1);
                        g_checksum_update (ck, (const guchar *) email, strlen (email));
                        const gchar *digest = g_checksum_get_string (ck);
                        if (source_uid && digest && g_ascii_strcasecmp (source_uid, digest) == 0)
                            match = TRUE;
                        if (display_name && g_ascii_strcasecmp (display_name, email) == 0)
                            match = TRUE;
                        g_checksum_free (ck);
                    }
                    if (!match && dname && *dname && display_name) {
                        if (g_ascii_strcasecmp (display_name, dname) == 0)
                            match = TRUE;
                    }
                    if (!match && id && *id && source_uid) {
                        const gchar *stripped_id = g_str_has_prefix (id, "eds:") ? id + 4 : id;
                        if (g_ascii_strcasecmp (source_uid, stripped_id) == 0 ||
                            g_ascii_strcasecmp (source_uid, id) == 0)
                            match = TRUE;
                    }

                    g_free (email);
                    g_free (dname);
                    g_free (id);

                    if (match) {
                        g_free (block);
                        g_free (json);
                        g_free (accounts_path);
                        return TRUE;
                    }
                }
                g_free (block);
            }
            g_free (json);
        }
        g_free (accounts_path);
    }

    return FALSE;
}

static CamelStore *
open_store_for_source (EdsCamelSession *session,
                       ESource *source,
                       GCancellable *cancellable,
                       GError **error)
{
    ESourceBackend *backend_extension;
    const gchar *backend_name;
    const gchar *source_uid;
    CamelService *service;

    if (!session || !source) {
        g_set_error_literal (
            error, G_IO_ERROR, G_IO_ERROR_INVALID_ARGUMENT,
            "Invalid session or source for Camel store");
        return NULL;
    }

    if (is_source_foreign_or_shared (source)) {
        g_printerr ("[EDS] Skipping separate store for foreign/shared source '%s'\n",
                    e_source_get_uid (source));
        g_set_error_literal (
            error, G_IO_ERROR, G_IO_ERROR_NOT_SUPPORTED,
            "Shared mailboxes are accessed via delegate folders under their parent Exchange account");
        return NULL;
    }

    backend_extension = (ESourceBackend *) e_source_get_extension (
        source, E_SOURCE_EXTENSION_MAIL_ACCOUNT);
    backend_name = backend_extension
        ? e_source_backend_get_backend_name (backend_extension)
        : NULL;
    source_uid = e_source_get_uid (source);

    if (!source_uid || !backend_name || !*backend_name) {
        g_set_error_literal (
            error, G_IO_ERROR, G_IO_ERROR_NOT_SUPPORTED,
            "EDS mail source has no Camel backend");
        return NULL;
    }

    service = camel_session_ref_service (CAMEL_SESSION (session), source_uid);
    if (!service) {
        service = camel_session_add_service (
            CAMEL_SESSION (session), source_uid, backend_name,
            CAMEL_PROVIDER_STORE, error);
    }
    if (!service)
        return NULL;

    {
        const gchar *parent_uid = e_source_get_parent (source);
        ESource *parent = (parent_uid && *parent_uid)
            ? e_source_registry_ref_source (session->registry, parent_uid)
            : NULL;
        g_printerr ("[EDS-DEBUG] Source '%s' (%s), parent='%s'\n",
            e_source_get_display_name (source), source_uid,
            parent ? e_source_get_display_name (parent) : "NULL");
        g_clear_object (&parent);
    }

    configure_ews_service_credentials_and_settings (session, source, service, backend_name, source_uid);

    if (!camel_service_get_password (service) && session->registry) {
        ESourceCredentialsProvider *provider = session->credentials_provider
            ? g_object_ref (session->credentials_provider)
            : e_source_credentials_provider_new (session->registry);
        if (provider) {
            ENamedParameters *credentials = NULL;
            if (!e_source_credentials_provider_lookup_sync (provider, source, NULL, &credentials, NULL) || !credentials) {
                const gchar *parent_uid = e_source_get_parent (source);
                if (parent_uid && *parent_uid) {
                    ESource *parent = e_source_registry_ref_source (session->registry, parent_uid);
                    if (parent) {
                        e_source_credentials_provider_lookup_sync (provider, parent, NULL, &credentials, NULL);
                        g_object_unref (parent);
                    }
                }
            }
            if (credentials) {
                const gchar *pwd = e_named_parameters_get (credentials, E_SOURCE_CREDENTIAL_PASSWORD);
                if (pwd && *pwd) {
                    camel_service_set_password (service, pwd);
                    g_printerr ("[EDS] Loaded password from CredentialsProvider for %s\n", source_uid);
                }
                e_named_parameters_free (credentials);
            }
            g_object_unref (provider);
        }
    }

    if (CAMEL_IS_OFFLINE_STORE (service)) {
        if (!camel_offline_store_get_online (CAMEL_OFFLINE_STORE (service))) {
            GError *offline_err = NULL;
            if (!camel_offline_store_set_online_sync (
                    CAMEL_OFFLINE_STORE (service), TRUE, cancellable, &offline_err)) {
                g_printerr ("[EDS] Failed to set CamelOfflineStore online for %s: %s\n",
                            source_uid, offline_err ? offline_err->message : "unknown");
                g_clear_error (&offline_err);
            } else {
                g_printerr ("[EDS] Successfully set CamelOfflineStore online for %s\n", source_uid);
            }
        }
    }

    if (camel_service_get_connection_status (service) != CAMEL_SERVICE_CONNECTED) {
        if (!camel_service_connect_sync (service, cancellable, error)) {
            g_printerr ("[EDS] camel_service_connect_sync failed for %s (%s): %s\n",
                        source_uid, backend_name,
                        error && *error ? (*error)->message : "unknown error");
            g_object_unref (service);
            return NULL;
        }
    }

    if (!CAMEL_IS_STORE (service)) {
        g_set_error_literal (
            error, G_IO_ERROR, G_IO_ERROR_NOT_SUPPORTED,
            "EDS mail backend did not create a Camel store");
        g_object_unref (service);
        return NULL;
    }

    return CAMEL_STORE (service);
}

typedef struct _BridgeEEwsConnection BridgeEEwsConnection;
typedef struct {
    gchar *id;
    gchar *change_key;
    gboolean is_distinguished_id;
} BridgeEwsFolderId;

typedef struct {
    CamelOfflineStore parent;
    void *priv;
    void *summary;
    gchar *storage_path;
} BridgeEwsStoreLayout;

static void
ensure_foreign_folders_for_ews_store (CamelStore *store,
                                      ESourceRegistry *registry,
                                      GCancellable *cancellable)
{
    if (!store || !CAMEL_IS_STORE (store))
        return;

    const gchar *type_name = G_OBJECT_TYPE_NAME (store);
    if (!type_name || !g_str_has_prefix (type_name, "CamelEwsStore"))
        return;

    static void *h_ews = NULL;
    static void *h_priv = NULL;
    static gboolean probed = FALSE;

    if (!probed) {
        probed = TRUE;
        h_ews = dlopen ("/usr/lib64/evolution-ews/libevolution-ews.so", RTLD_LAZY | RTLD_GLOBAL);
        if (!h_ews)
            h_ews = dlopen ("libevolution-ews.so", RTLD_LAZY | RTLD_GLOBAL);
        h_priv = dlopen ("/usr/lib64/evolution-ews/libcamelews-priv.so", RTLD_LAZY | RTLD_GLOBAL);
        if (!h_priv)
            h_priv = dlopen ("libcamelews-priv.so", RTLD_LAZY | RTLD_GLOBAL);
    }

    if (!h_ews || !h_priv)
        return;

    BridgeEEwsConnection *(*fn_ref_cnc)(CamelStore *) = dlsym (h_priv, "camel_ews_store_ref_connection");
    gboolean (*fn_get_folder_info)(BridgeEEwsConnection *, gint, const gchar *, const void *, void **, GCancellable *, GError **) =
        dlsym (h_ews, "e_ews_connection_get_folder_info_sync");
    const BridgeEwsFolderId *(*fn_get_id)(void *) = dlsym (h_ews, "e_ews_folder_get_id");
    guint32 (*fn_get_total_count)(void *) = dlsym (h_ews, "e_ews_folder_get_total_count");
    gboolean (*fn_has_folder)(void *, const gchar *) = dlsym (h_priv, "camel_ews_store_summary_has_folder");
    void (*fn_new_folder)(void *, const gchar *, const gchar *, const gchar *, const gchar *, gint, guint32, guint32, gboolean, gboolean) =
        dlsym (h_priv, "camel_ews_store_summary_new_folder");
    void (*fn_ensure_virtual_folders)(CamelStore *) = dlsym (h_priv, "camel_ews_store_ensure_virtual_folders");
    void (*fn_set_foreign_subfolders)(void *, const gchar *, gboolean) = dlsym (h_priv, "camel_ews_store_summary_set_foreign_subfolders");
    gboolean (*fn_summary_save)(void *, GError **) = dlsym (h_priv, "camel_ews_store_summary_save");
    void (*fn_update_foreign_subfolders)(CamelStore *, const gchar *) = dlsym (h_priv, "camel_ews_store_update_foreign_subfolders");

    if (!fn_ref_cnc || !fn_get_folder_info || !fn_get_id || !fn_has_folder || !fn_new_folder || !fn_ensure_virtual_folders)
        return;

    BridgeEwsStoreLayout *ews_store = (BridgeEwsStoreLayout *) store;
    void *summary = ews_store->summary;
    if (!summary)
        return;

    fn_ensure_virtual_folders (store);

    gchar *accounts_path = bridge_get_accounts_json_path ();
    if (!g_file_test (accounts_path, G_FILE_TEST_EXISTS)) {
        g_free (accounts_path);
        return;
    }

    gchar *json = NULL;
    if (!g_file_get_contents (accounts_path, &json, NULL, NULL) || !json) {
        g_free (accounts_path);
        return;
    }
    g_free (accounts_path);

    const gchar *accounts_start = strstr (json, "\"accounts\"");
    const gchar *p = accounts_start ? accounts_start : json;
    while ((p = strchr (p, '{')) != NULL) {
        if (p == json) {
            p++;
            continue;
        }
        const gchar *block_start = p;
        const gchar *block_end = strchr (block_start, '}');
        if (!block_end)
            break;
        gchar *block = g_strndup (block_start, block_end - block_start + 1);
        p = block_end + 1;

        gboolean is_shared = bridge_json_extract_bool (block, "is_shared_mailbox") ||
                             bridge_json_extract_bool (block, "open_mailbox_other_user");
        gchar *email = bridge_json_extract_string (block, "email");
        gchar *display_name = bridge_json_extract_string (block, "display_name");
        g_free (block);

        if (is_shared && email && *email) {
            gchar *foreign_mailbox_id = g_strdup_printf ("ForeignMailbox::%s", email);
            BridgeEEwsConnection *cnc = fn_ref_cnc (store);
            if (cnc) {
                gchar *mailbox = g_strdup_printf ("Postfach — %s", email);

                if (!fn_has_folder (summary, foreign_mailbox_id)) {
                    fn_new_folder (summary, foreign_mailbox_id, "ForeignRoot", NULL, mailbox, 1 /* MAILBOX */,
                                   CAMEL_FOLDER_SYSTEM | CAMEL_FOLDER_NOSELECT, 0, FALSE, FALSE);
                }

                static const struct {
                    const gchar *dist_id;
                    const gchar *folder_name;
                    guint32 flags;
                } dist_folders[] = {
                    { "inbox", "Posteingang", CAMEL_FOLDER_TYPE_INBOX | CAMEL_FOLDER_SUBSCRIBED },
                    { "sentitems", "Gesendete Elemente", CAMEL_FOLDER_TYPE_SENT | CAMEL_FOLDER_SUBSCRIBED },
                    { "drafts", "Entwürfe", CAMEL_FOLDER_TYPE_DRAFTS | CAMEL_FOLDER_SUBSCRIBED },
                    { "deleteditems", "Gelöschte Elemente", CAMEL_FOLDER_TYPE_TRASH | CAMEL_FOLDER_SUBSCRIBED },
                    { "junkemail", "Junk-E-Mail", CAMEL_FOLDER_TYPE_JUNK | CAMEL_FOLDER_SUBSCRIBED },
                };

                gboolean any_added = FALSE;
                for (gsize i = 0; i < G_N_ELEMENTS (dist_folders); i++) {
                    BridgeEwsFolderId fid;
                    fid.id = (gchar *) dist_folders[i].dist_id;
                    fid.change_key = NULL;
                    fid.is_distinguished_id = TRUE;

                    void *folder = NULL;
                    GError *sub_err = NULL;
                    if (fn_get_folder_info (cnc, G_PRIORITY_DEFAULT, email, &fid, &folder, cancellable, &sub_err) && folder) {
                        const BridgeEwsFolderId *res_fid = fn_get_id (folder);
                        if (res_fid && res_fid->id) {
                            if (!fn_has_folder (summary, res_fid->id)) {
                                guint32 total_cnt = fn_get_total_count ? fn_get_total_count (folder) : 0;
                                fn_new_folder (summary, res_fid->id, foreign_mailbox_id, res_fid->change_key,
                                               dist_folders[i].folder_name, 1 /* MAILBOX */,
                                               dist_folders[i].flags, total_cnt, TRUE, FALSE);
                                any_added = TRUE;
                            }
                            if (fn_set_foreign_subfolders)
                                fn_set_foreign_subfolders (summary, res_fid->id, TRUE);
                            if (fn_update_foreign_subfolders)
                                fn_update_foreign_subfolders (store, res_fid->id);
                        }
                        g_object_unref (folder);
                    } else {
                        g_clear_error (&sub_err);
                    }
                }

                /* Also resolve the shared calendar folder ID if a foreign calendar source exists */
                if (registry) {
                    BridgeEwsFolderId cal_fid;
                    cal_fid.id = (gchar *) "calendar";
                    cal_fid.change_key = NULL;
                    cal_fid.is_distinguished_id = TRUE;

                    void *cal_folder = NULL;
                    GError *cal_err = NULL;
                    if (fn_get_folder_info (cnc, G_PRIORITY_DEFAULT, email, &cal_fid, &cal_folder, cancellable, &cal_err) && cal_folder) {
                        const BridgeEwsFolderId *res_cal_fid = fn_get_id (cal_folder);
                        if (res_cal_fid && res_cal_fid->id) {
                            GList *cal_sources = e_source_registry_list_sources (registry, "Calendar");
                            for (GList *l = cal_sources; l != NULL; l = l->next) {
                                ESource *cal_source = E_SOURCE (l->data);
                                if (e_source_has_extension (cal_source, "Exchange Web Services Folder")) {
                                    ESourceExtension *ews_ext = e_source_get_extension (cal_source, "Exchange Web Services Folder");
                                    gboolean is_foreign = FALSE;
                                    gchar *f_mail = NULL;
                                    gchar *cur_id = NULL;
                                    g_object_get (ews_ext, "foreign", &is_foreign, "foreign-mail", &f_mail, "id", &cur_id, NULL);
                                    if (is_foreign && f_mail && g_ascii_strcasecmp (f_mail, email) == 0) {
                                        if (!cur_id || g_strcmp0 (cur_id, res_cal_fid->id) != 0 || g_ascii_strcasecmp (cur_id, "calendar") == 0) {
                                            g_object_set (ews_ext,
                                                          "id", res_cal_fid->id,
                                                          "change-key", res_cal_fid->change_key ? res_cal_fid->change_key : "",
                                                          NULL);
                                            GError *commit_err = NULL;
                                            if (e_source_registry_commit_source_sync (registry, cal_source, cancellable, &commit_err)) {
                                                g_printerr ("[EDS] Successfully committed resolved EWS calendar FolderId for %s\n", email);
                                            } else {
                                                g_printerr ("[EDS] Failed to commit calendar FolderId for %s: %s\n",
                                                            email, commit_err ? commit_err->message : "unknown");
                                                g_clear_error (&commit_err);
                                            }
                                        }
                                    }
                                    g_free (f_mail);
                                    g_free (cur_id);
                                }
                            }
                            g_list_free_full (cal_sources, g_object_unref);
                        }
                        g_object_unref (cal_folder);
                    } else {
                        g_clear_error (&cal_err);
                    }
                }

                if (any_added) {
                    fn_ensure_virtual_folders (store);
                    if (fn_summary_save)
                        fn_summary_save (summary, NULL);
                }
                g_free (mailbox);
                g_object_unref (cnc);
            }
            g_free (foreign_mailbox_id);
        }
        g_free (display_name);
        g_free (email);
    }
    g_free (json);
}

static gboolean
load_source (LoadContext *context,
             EdsCamelSession *session,
             ESource *source,
             const gchar *folder_name,
             GCancellable *cancellable)
{
    ESourceBackend *backend;
    const gchar *source_uid;
    const gchar *backend_name;
    CamelStore *store = NULL;
    CamelFolderInfo *folder_info = NULL;
    GError *error = NULL;
    gboolean success = FALSE;

    if (!context || !session || !source)
        return FALSE;

    backend = (ESourceBackend *) e_source_get_extension (
        source, E_SOURCE_EXTENSION_MAIL_ACCOUNT);
    backend_name = backend
        ? e_source_backend_get_backend_name (backend)
        : NULL;
    source_uid = e_source_get_uid (source);

    if (!source_uid || !backend_name || !*backend_name)
        return FALSE;

    /* The local provider is already handled by the read-only local parser;
     * network sources are opened through the original EDS/Camel provider. */
    if (g_str_equal (backend_name, "maildir") ||
        g_str_equal (backend_name, "mbox") ||
        g_str_equal (backend_name, "mh") ||
        g_str_equal (backend_name, "spool") ||
        g_str_equal (backend_name, "vfolder") ||
        g_str_equal (backend_name, "rss"))
        return FALSE;

    /* Shared mailboxes and foreign folder sources are accessed via delegate folders
     * under their parent Exchange store. Opening an independent Camel store for them
     * with primary user credentials would incorrectly duplicate the user's primary mailbox. */
    if (is_source_foreign_or_shared (source)) {
        g_printerr ("[EDS] Skipping separate store for foreign/shared mailbox source '%s'\n", source_uid);
        return FALSE;
    }

    if (load_cancelled (context))
        return FALSE;

    context->configured_sources++;
    emit_progress (
        context, EVOLUTION_EDS_CAMEL_SYNC_CONNECTING,
        source_uid, e_source_get_display_name (source), NULL);

    store = open_store_for_source (session, source, cancellable, &error);
    if (!store) {
        context->failed_sources++;
        context->processed_sources++;
        gchar *scope = g_strdup_printf (
            "%s (%s)", e_source_get_display_name (source), backend_name);
        remember_error (context, scope, error);
        g_free (scope);
        g_clear_error (&error);
        return FALSE;
    }

    ensure_foreign_folders_for_ews_store (store, session ? session->registry : NULL, cancellable);

    folder_info = camel_store_get_folder_info_sync (
        store, NULL,
        CAMEL_STORE_FOLDER_INFO_RECURSIVE |
        CAMEL_STORE_FOLDER_INFO_REFRESH,
        cancellable, &error);
    if (!folder_info) {
        context->failed_sources++;
        context->processed_sources++;
        gchar *scope = g_strdup_printf (
            "%s: Ordner konnten nicht geladen werden",
            e_source_get_display_name (source));
        remember_error (context, scope, error);
        g_free (scope);
        g_clear_error (&error);
    } else {
        context->successful_sources++;
        emit_progress (
            context, EVOLUTION_EDS_CAMEL_SYNC_LOADING_FOLDERS,
            source_uid, e_source_get_display_name (source), NULL);
        load_folder (
            context, store, source_uid,
            e_source_get_display_name (source), folder_name,
            folder_info, cancellable);
        camel_folder_info_free (folder_info);
        if (context->cancelled) {
            g_object_unref (store);
            return FALSE;
        }
        context->processed_sources++;
        emit_progress (
            context, EVOLUTION_EDS_CAMEL_SYNC_SOURCE_COMPLETE,
            source_uid, e_source_get_display_name (source), NULL);
        success = TRUE;
    }

    g_object_unref (store);
    return success;
}

int
evolution_eds_camel_load_messages (const char *source_uid,
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
                                   void *cancellation_user_data)
{
    ESourceRegistry *registry = NULL;
    EdsCamelSession *session = NULL;
    GList *sources = NULL;
    GList *link;
    GCancellable *cancellable = NULL;
    CancellationMonitor *cancellation_monitor = NULL;
    LoadContext context = { 0 };
    GError *error = NULL;
    int result = 0;

    if (out_messages)
        *out_messages = NULL;
    if (out_count)
        *out_count = 0;
    if (out_folders)
        *out_folders = NULL;
    if (out_folder_count)
        *out_folder_count = 0;
    if (out_configured_sources)
        *out_configured_sources = 0;
    if (out_successful_sources)
        *out_successful_sources = 0;
    if (out_failed_sources)
        *out_failed_sources = 0;
    if (out_error)
        *out_error = NULL;

    context.message_offset = message_offset > 0 ? message_offset : 0;
    if (folder_name && *folder_name) {
        context.max_messages_per_folder = max_messages_per_folder > 0
            ? max_messages_per_folder
            : 10000;
    } else {
        context.max_messages_per_folder = max_messages_per_folder > 0
            ? max_messages_per_folder
            : DEFAULT_MESSAGES_PER_FOLDER;
    }
    context.progress_callback = progress_callback;
    context.progress_user_data = progress_user_data;
    context.cancellation_callback = cancellation_callback;
    context.cancellation_user_data = cancellation_user_data;

    emit_progress (
        &context, EVOLUTION_EDS_CAMEL_SYNC_DISCOVERING, NULL, NULL, NULL);

    camel_provider_init ();

    registry = eds_camel_ref_registry (&error);
    if (!registry) {
        emit_progress (
            &context, EVOLUTION_EDS_CAMEL_SYNC_FAILED, NULL, NULL, NULL);
        if (out_error && error)
            *out_error = g_strdup (error->message);
        g_clear_error (&error);
        return 0;
    }

    session = eds_camel_ref_session (registry);
    cancellable = g_cancellable_new ();
    cancellation_monitor = cancellation_monitor_start (
        cancellable, cancellation_callback, cancellation_user_data);
    sources = e_source_registry_list_enabled (
        registry, E_SOURCE_EXTENSION_MAIL_ACCOUNT);

    for (link = sources; link != NULL; link = link->next) {
        ESource *source = E_SOURCE (link->data);

        if (load_cancelled (&context))
            break;

        if (source_uid &&
            !g_str_equal (source_uid, e_source_get_uid (source)))
            continue;

        load_source (&context, session, source, folder_name, cancellable);
    }

    load_cancelled (&context);

    if (out_configured_sources)
        *out_configured_sources = context.configured_sources;
    if (out_successful_sources)
        *out_successful_sources = context.successful_sources;
    if (out_failed_sources)
        *out_failed_sources = context.failed_sources;

    if (context.cancelled) {
        if (!context.first_error)
            context.first_error = g_strdup (
                "EDS/Camel-Ladevorgang wurde abgebrochen");
        result = 0;
    } else if (context.successful_sources > 0 || context.configured_sources == 0) {
        if (out_messages)
            *out_messages = context.messages;
        if (out_count)
            *out_count = context.count;
        context.messages = NULL;
        context.count = 0;
        if (out_folders)
            *out_folders = context.folders;
        if (out_folder_count)
            *out_folder_count = context.folder_count;
        context.folders = NULL;
        context.folder_count = 0;
        result = 1;
    } else if (context.configured_sources > 0) {
        /* Keep an all-failed account result observable by the UI.  Returning
         * an opaque failure here made the caller fall back to the unrelated
         * local mailbox, which looked like an empty account and hid the real
         * authentication/network/provider error. */
        if (out_messages)
            *out_messages = context.messages;
        if (out_count)
            *out_count = context.count;
        context.messages = NULL;
        context.count = 0;
        if (out_folders)
            *out_folders = context.folders;
        if (out_folder_count)
            *out_folder_count = context.folder_count;
        context.folders = NULL;
        context.folder_count = 0;
        result = 1;
    } else {
        /* No enabled remote EDS mail account is a normal state; the Rust
         * caller will use its local Evolution mailbox fallback. */
        result = 1;
    }

    if (out_error && context.first_error)
        *out_error = g_strdup (context.first_error);

    emit_progress (
        &context,
        context.cancelled ||
        (context.failed_sources > 0 && context.successful_sources == 0)
            ? EVOLUTION_EDS_CAMEL_SYNC_FAILED
            : EVOLUTION_EDS_CAMEL_SYNC_COMPLETE,
        NULL, NULL, NULL);

    g_free (context.first_error);
    evolution_eds_camel_free_messages (context.messages, context.count);
    evolution_eds_camel_free_folders (context.folders, context.folder_count);
    cancellation_monitor_free (cancellation_monitor);
    g_clear_object (&cancellable);
    g_list_free_full (sources, g_object_unref);
    g_clear_object (&session);
    g_clear_object (&registry);

    return result;
}

static CamelFolderInfo *
find_folder_info_by_name (CamelFolderInfo *folder_info,
                          const gchar *folder_name)
{
    CamelFolderInfo *current;

    for (current = folder_info; current != NULL; current = current->next) {
        if (current->full_name &&
            g_str_equal (current->full_name, folder_name))
            return current;

        if (current->child) {
            CamelFolderInfo *match = find_folder_info_by_name (
                current->child, folder_name);
            if (match)
                return match;
        }
    }

    return NULL;
}

int
evolution_eds_camel_load_messages_by_uid (
    const char *source_uid,
    const char *folder_name,
    const char * const *message_uids,
    size_t message_uid_count,
    EvolutionEdsCamelMessage **out_messages,
    size_t *out_count,
    char **out_error,
    EvolutionEdsCamelCancellationCallback cancellation_callback,
    void *cancellation_user_data)
{
    ESourceRegistry *registry = NULL;
    EdsCamelSession *session = NULL;
    ESource *source = NULL;
    CamelStore *store = NULL;
    CamelFolder *folder = NULL;
    GCancellable *cancellable = NULL;
    CancellationMonitor *cancellation_monitor = NULL;
    LoadContext context = { 0 };
    CamelFolderInfo synthetic_folder = { 0 };
    CamelFolderInfo *matching_folder = NULL;
    GError *error = NULL;
    gboolean success = FALSE;
    size_t ii;
    gsize cache_hits = 0;
    gsize provider_fetches = 0;
    gint64 started = g_get_monotonic_time ();

    if (out_messages)
        *out_messages = NULL;
    if (out_count)
        *out_count = 0;
    if (out_error)
        *out_error = NULL;

    if (!source_uid || !*source_uid || !folder_name || !*folder_name ||
        !message_uids || message_uid_count == 0) {
        if (out_error)
            *out_error = g_strdup (
                "Inkrementelles Camel-Laden benötigt Konto, Ordner und UIDs");
        return 0;
    }

    context.cancellation_callback = cancellation_callback;
    context.cancellation_user_data = cancellation_user_data;
    camel_provider_init ();

    registry = eds_camel_ref_registry (&error);
    if (!registry)
        goto finish;

    source = e_source_registry_ref_source (registry, source_uid);
    if (!source) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_NOT_FOUND,
            "EDS source for incremental Camel loading no longer exists");
        goto finish;
    }
    if (!e_source_get_enabled (source)) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_FAILED,
            "EDS source for incremental Camel loading is disabled");
        goto finish;
    }

    session = eds_camel_ref_session (registry);
    cancellable = g_cancellable_new ();
    cancellation_monitor = cancellation_monitor_start (
        cancellable, cancellation_callback, cancellation_user_data);
    store = open_store_for_source (session, source, cancellable, &error);
    if (!store)
        goto finish;

    folder = camel_store_get_folder_sync (
        store, folder_name, CAMEL_STORE_FOLDER_NONE,
        cancellable, &error);
    if (!folder)
        goto finish;

    /* The caller already has an exact folder and UID. We have the CamelFolder
     * directly; avoid camel_store_get_folder_info_sync which triggers slow
     * synchronous network roundtrips to IMAP/Exchange servers on every message click. */
    synthetic_folder.full_name = (gchar *) folder_name;
    synthetic_folder.display_name = (gchar *) folder_name;
    synthetic_folder.flags = 0;
    matching_folder = &synthetic_folder;

    for (ii = 0; ii < message_uid_count; ii++) {
        CamelMessageInfo *message_info = NULL;
        CamelMimeMessage *message = NULL;

        if (load_cancelled (&context))
            break;
        if (!message_uids[ii] || !*message_uids[ii])
            continue;

        message_info = camel_folder_get_message_info (
            folder, message_uids[ii]);
        if (!message_info)
            continue;

        message = camel_folder_get_message_cached (
            folder, message_uids[ii], cancellable);
        if (message) {
            cache_hits++;
        } else if (!load_cancelled (&context)) {
            provider_fetches++;
            message = camel_folder_get_message_sync (
                folder, message_uids[ii], cancellable, &error);
        }
        if (!message) {
            remember_error (&context, folder_name, error);
            g_clear_error (&error);
            g_object_unref (message_info);
            continue;
        }

        append_message (
            &context, source_uid, matching_folder, message_uids[ii],
            message_info, message, cancellable);
        g_object_unref (message);
        g_object_unref (message_info);
    }

    load_cancelled (&context);
    success = !context.cancelled;
    if (context.cancelled && !context.first_error)
        context.first_error = g_strdup (
            "Inkrementelles Camel-Laden wurde abgebrochen");

finish:
    if (!success && !context.first_error && error)
        context.first_error = g_strdup (error->message);
    if (out_error && context.first_error)
        *out_error = g_strdup (context.first_error);

    if (success) {
        if (out_messages)
            *out_messages = context.messages;
        if (out_count)
            *out_count = context.count;
        context.messages = NULL;
        context.count = 0;
    }

    g_free (context.first_error);
    evolution_eds_camel_free_messages (context.messages, context.count);
    g_clear_error (&error);
    cancellation_monitor_free (cancellation_monitor);
    g_clear_object (&cancellable);
    g_clear_object (&folder);
    g_clear_object (&store);
    g_clear_object (&session);
    g_clear_object (&source);
    g_clear_object (&registry);

    g_debug ("Camel UID retrieval: elapsed_ms=%" G_GINT64_FORMAT
             " requested=%" G_GSIZE_FORMAT " cache_hits=%" G_GSIZE_FORMAT
             " provider_fetches=%" G_GSIZE_FORMAT,
             (g_get_monotonic_time () - started) / 1000,
             (gsize) message_uid_count, cache_hits, provider_fetches);

    return success ? 1 : 0;
}

void
evolution_eds_camel_free_messages (EvolutionEdsCamelMessage *messages,
                                   size_t count)
{
    size_t ii;

    if (!messages)
        return;

    for (ii = 0; ii < count; ii++) {
        g_free (messages[ii].source_uid);
        g_free (messages[ii].folder_name);
        g_free (messages[ii].folder_id);
        g_free (messages[ii].message_uid);
        g_free (messages[ii].raw_message);
        g_free (messages[ii].follow_up);
        g_free (messages[ii].completed_on);
        g_free (messages[ii].due_by);
    }

    g_free (messages);
}

void
evolution_eds_camel_free_folders (EvolutionEdsCamelFolder *folders,
                                  size_t count)
{
    size_t ii;

    if (!folders)
        return;

    for (ii = 0; ii < count; ii++) {
        g_free (folders[ii].source_uid);
        g_free (folders[ii].source_display_name);
        g_free (folders[ii].folder_name);
        g_free (folders[ii].folder_id);
        g_free (folders[ii].display_name);
    }

    g_free (folders);
}

typedef struct {
    EvolutionEdsCamelSubscriptionFolder *folders;
    size_t count;
    size_t capacity;
} SubscriptionFolderContext;

static gboolean
ensure_subscription_folder_capacity (SubscriptionFolderContext *context)
{
    size_t new_capacity;

    if (context->count < context->capacity)
        return TRUE;

    new_capacity = context->capacity ? context->capacity * 2 : 32;
    context->folders = g_realloc_n (
        context->folders, new_capacity, sizeof (*context->folders));
    context->capacity = new_capacity;

    return TRUE;
}

static void
append_subscription_folder_tree (SubscriptionFolderContext *context,
                                 const gchar *source_uid,
                                 const gchar *source_display_name,
                                 CamelFolderInfo *folder_info,
                                 const gchar *parent_folder_name)
{
    CamelFolderInfo *current;

    for (current = folder_info; current != NULL; current = current->next) {
        EvolutionEdsCamelSubscriptionFolder *record;

        if (is_mail_folder (current) &&
            current->full_name && *current->full_name) {
            ensure_subscription_folder_capacity (context);
            record = &context->folders[context->count++];
            record->source_uid = g_strdup (source_uid);
            record->source_display_name = g_strdup (
                source_display_name && *source_display_name
                    ? source_display_name
                    : source_uid);
            record->folder_name = g_strdup (current->full_name);
            record->parent_folder_name = parent_folder_name
                ? g_strdup (parent_folder_name)
                : NULL;
            record->display_name = g_strdup (
                current->display_name ?: current->full_name);
            record->unread_count = current->unread > 0
                ? (guint32) current->unread
                : 0;
            record->total_count = current->total > 0
                ? (guint32) current->total
                : 0;
            record->is_selectable =
                (current->flags & CAMEL_FOLDER_NOSELECT) == 0;
            record->is_subscribed =
                (current->flags & CAMEL_FOLDER_SUBSCRIBED) != 0;
        }

        if (current->child) {
            append_subscription_folder_tree (
                context, source_uid, source_display_name, current->child,
                current->full_name && *current->full_name
                    ? current->full_name
                    : parent_folder_name);
        }
    }
}

void
evolution_eds_camel_free_subscription_folders (
    EvolutionEdsCamelSubscriptionFolder *folders,
    size_t count)
{
    size_t ii;

    if (!folders)
        return;

    for (ii = 0; ii < count; ii++) {
        g_free (folders[ii].source_uid);
        g_free (folders[ii].source_display_name);
        g_free (folders[ii].folder_name);
        g_free (folders[ii].parent_folder_name);
        g_free (folders[ii].display_name);
    }

    g_free (folders);
}

int
evolution_eds_camel_load_subscription_folders (
    const char *source_uid,
    EvolutionEdsCamelSubscriptionFolder **out_folders,
    size_t *out_folder_count,
    int *out_is_subscribable,
    char **out_error,
    EvolutionEdsCamelCancellationCallback cancellation_callback,
    void *cancellation_user_data)
{
    ESourceRegistry *registry = NULL;
    EdsCamelSession *session = NULL;
    ESource *source = NULL;
    CamelStore *store = NULL;
    CamelFolderInfo *folder_info = NULL;
    GCancellable *cancellable = NULL;
    CancellationMonitor *cancellation_monitor = NULL;
    SubscriptionFolderContext context = { 0 };
    GError *error = NULL;
    gboolean success = FALSE;

    if (out_folders)
        *out_folders = NULL;
    if (out_folder_count)
        *out_folder_count = 0;
    if (out_is_subscribable)
        *out_is_subscribable = 0;
    if (out_error)
        *out_error = NULL;

    if (!source_uid || !*source_uid) {
        if (out_error)
            *out_error = g_strdup (
                "Camel-Abonnements benötigen eine genaue EDS-Quellen-ID");
        return 0;
    }
    if (cancellation_requested (cancellation_callback, cancellation_user_data)) {
        if (out_error)
            *out_error = g_strdup ("Camel-Abonnements wurden abgebrochen");
        return 0;
    }

    camel_provider_init ();
    registry = eds_camel_ref_registry (&error);
    if (!registry)
        goto finish;

    source = e_source_registry_ref_source (registry, source_uid);
    if (!source) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_NOT_FOUND,
            "Die EDS-Mailquelle für Abonnements existiert nicht mehr");
        goto finish;
    }
    if (!e_source_get_enabled (source)) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_FAILED,
            "Die EDS-Mailquelle für Abonnements ist deaktiviert");
        goto finish;
    }

    session = eds_camel_ref_session (registry);
    cancellable = g_cancellable_new ();
    cancellation_monitor = cancellation_monitor_start (
        cancellable, cancellation_callback, cancellation_user_data);
    store = open_store_for_source (session, source, cancellable, &error);
    if (!store)
        goto finish;

    if (!CAMEL_IS_SUBSCRIBABLE (store)) {
        /* A successful capability probe is intentionally distinct from a
         * mutation.  Callers get an explicit false capability and cannot
         * mistake an empty regular-folder list for subscription support. */
        success = TRUE;
        goto finish;
    }
    if (out_is_subscribable)
        *out_is_subscribable = 1;

    folder_info = camel_store_get_folder_info_sync (
        store, NULL,
        CAMEL_STORE_FOLDER_INFO_RECURSIVE |
        CAMEL_STORE_FOLDER_INFO_NO_VIRTUAL |
        CAMEL_STORE_FOLDER_INFO_SUBSCRIPTION_LIST,
        cancellable, &error);
    if (!folder_info)
        goto finish;

    if (cancellation_requested (cancellation_callback, cancellation_user_data)) {
        set_cancelled_error (&error);
        goto finish;
    }

    append_subscription_folder_tree (
        &context, source_uid, e_source_get_display_name (source),
        folder_info, NULL);
    success = TRUE;

finish:
    if (!success && out_error) {
        *out_error = g_strdup (error
            ? error->message
            : "Camel-Abonnementsliste konnte nicht geladen werden");
    }
    if (success) {
        if (out_folders) {
            *out_folders = context.folders;
            if (out_folder_count)
                *out_folder_count = context.count;
            context.folders = NULL;
            context.count = 0;
        }
    }

    evolution_eds_camel_free_subscription_folders (
        context.folders, context.count);
    camel_folder_info_free (folder_info);
    g_clear_error (&error);
    cancellation_monitor_free (cancellation_monitor);
    g_clear_object (&cancellable);
    g_clear_object (&store);
    g_clear_object (&session);
    g_clear_object (&source);
    g_clear_object (&registry);

    return success ? 1 : 0;
}

int
evolution_eds_camel_set_folder_subscription (
    const char *source_uid,
    const char *folder_name,
    int subscribed,
    char **out_error,
    EvolutionEdsCamelCancellationCallback cancellation_callback,
    void *cancellation_user_data)
{
    ESourceRegistry *registry = NULL;
    EdsCamelSession *session = NULL;
    ESource *source = NULL;
    CamelStore *store = NULL;
    CamelFolderInfo *folder_info = NULL;
    CamelFolderInfo *matching_folder = NULL;
    GCancellable *cancellable = NULL;
    CancellationMonitor *cancellation_monitor = NULL;
    GError *error = NULL;
    gboolean desired_subscribed = subscribed != 0;
    gboolean success = FALSE;

    if (out_error)
        *out_error = NULL;
    if (!source_uid || !*source_uid || !folder_name || !*folder_name) {
        if (out_error)
            *out_error = g_strdup (
                "Camel-Abonnements benötigen eine genaue Quelle und einen Ordnernamen");
        return 0;
    }
    if (cancellation_requested (cancellation_callback, cancellation_user_data)) {
        if (out_error)
            *out_error = g_strdup ("Camel-Abonnementänderung wurde abgebrochen");
        return 0;
    }

    camel_provider_init ();
    registry = eds_camel_ref_registry (&error);
    if (!registry)
        goto finish;

    source = e_source_registry_ref_source (registry, source_uid);
    if (!source) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_NOT_FOUND,
            "Die EDS-Mailquelle für Abonnements existiert nicht mehr");
        goto finish;
    }
    if (!e_source_get_enabled (source)) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_FAILED,
            "Die EDS-Mailquelle für Abonnements ist deaktiviert");
        goto finish;
    }

    session = eds_camel_ref_session (registry);
    cancellable = g_cancellable_new ();
    cancellation_monitor = cancellation_monitor_start (
        cancellable, cancellation_callback, cancellation_user_data);
    store = open_store_for_source (session, source, cancellable, &error);
    if (!store)
        goto finish;
    if (!CAMEL_IS_SUBSCRIBABLE (store)) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_NOT_SUPPORTED,
            "Diese Camel-Mailquelle unterstützt keine Ordner-Abonnements");
        goto finish;
    }

    folder_info = camel_store_get_folder_info_sync (
        store, NULL,
        CAMEL_STORE_FOLDER_INFO_RECURSIVE |
        CAMEL_STORE_FOLDER_INFO_NO_VIRTUAL |
        CAMEL_STORE_FOLDER_INFO_SUBSCRIPTION_LIST,
        cancellable, &error);
    if (!folder_info)
        goto finish;
    matching_folder = find_folder_info_by_name (folder_info, folder_name);
    if (!matching_folder || !is_mail_folder (matching_folder)) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_NOT_FOUND,
            "Der abonnierbare Camel-Ordner wurde nicht gefunden");
        goto finish;
    }
    if ((matching_folder->flags & CAMEL_FOLDER_NOSELECT) != 0) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_NOT_SUPPORTED,
            "Dieser Camel-Ordner ist nicht abonnierbar");
        goto finish;
    }

    if (((matching_folder->flags & CAMEL_FOLDER_SUBSCRIBED) != 0) !=
        desired_subscribed) {
        if (desired_subscribed) {
            if (!camel_subscribable_subscribe_folder_sync (
                    CAMEL_SUBSCRIBABLE (store), folder_name,
                    cancellable, &error))
                goto finish;
        } else if (!camel_subscribable_unsubscribe_folder_sync (
                       CAMEL_SUBSCRIBABLE (store), folder_name,
                       cancellable, &error)) {
            goto finish;
        }
    }

    camel_folder_info_free (folder_info);
    folder_info = camel_store_get_folder_info_sync (
        store, NULL,
        CAMEL_STORE_FOLDER_INFO_RECURSIVE |
        CAMEL_STORE_FOLDER_INFO_NO_VIRTUAL |
        CAMEL_STORE_FOLDER_INFO_SUBSCRIPTION_LIST,
        cancellable, &error);
    if (!folder_info)
        goto finish;
    matching_folder = find_folder_info_by_name (folder_info, folder_name);
    if (!matching_folder ||
        (((matching_folder->flags & CAMEL_FOLDER_SUBSCRIBED) != 0) !=
         desired_subscribed)) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_FAILED,
            "Der Camel-Server hat die angeforderte Abonnementänderung nicht bestätigt");
        goto finish;
    }
    if (cancellation_requested (cancellation_callback, cancellation_user_data)) {
        set_cancelled_error (&error);
        goto finish;
    }
    success = TRUE;

finish:
    if (!success && out_error) {
        *out_error = g_strdup (error
            ? error->message
            : "Camel-Abonnementänderung konnte nicht bestätigt werden");
    }
    camel_folder_info_free (folder_info);
    g_clear_error (&error);
    cancellation_monitor_free (cancellation_monitor);
    g_clear_object (&cancellable);
    g_clear_object (&store);
    g_clear_object (&session);
    g_clear_object (&source);
    g_clear_object (&registry);

    return success ? 1 : 0;
}

static CamelFolderInfo *
load_folder_mutation_tree (CamelStore *store,
                           GCancellable *cancellable,
                           GError **error)
{
    return camel_store_get_folder_info_sync (
        store, NULL,
        CAMEL_STORE_FOLDER_INFO_RECURSIVE |
        CAMEL_STORE_FOLDER_INFO_NO_VIRTUAL |
        CAMEL_STORE_FOLDER_INFO_REFRESH,
        cancellable, error);
}

static gboolean
folder_is_protected_for_mutation (CamelFolderInfo *folder_info)
{
    CamelFolderInfoFlags flags;

    if (!folder_info)
        return TRUE;

    flags = folder_info->flags;
    return (flags & (CAMEL_FOLDER_SYSTEM |
                     CAMEL_FOLDER_VIRTUAL |
                     CAMEL_FOLDER_VTRASH |
                     CAMEL_FOLDER_READONLY)) != 0;
}

static gboolean
folder_can_refresh (CamelFolderInfo *folder_info)
{
    return is_mail_folder (folder_info) &&
        folder_info->full_name && *folder_info->full_name &&
        (folder_info->flags & (CAMEL_FOLDER_NOSELECT |
                               CAMEL_FOLDER_VIRTUAL |
                               CAMEL_FOLDER_VTRASH)) == 0;
}

static gboolean
folder_can_edit (CamelStore *store,
                 CamelFolderInfo *folder_info)
{
    return folder_can_refresh (folder_info) &&
        !folder_is_protected_for_mutation (folder_info) &&
        (camel_store_get_flags (store) & CAMEL_STORE_CAN_EDIT_FOLDERS) != 0;
}

static void
set_folder_mutation_capabilities (CamelStore *store,
                                  CamelFolderInfo *folder_info,
                                  EvolutionEdsCamelFolderMutationCapabilities *capabilities)
{
    gboolean editable;

    memset (capabilities, 0, sizeof (*capabilities));
    if (!folder_info)
        return;

    editable = folder_can_edit (store, folder_info);
    capabilities->can_create_child = editable &&
        (folder_info->flags & CAMEL_FOLDER_NOINFERIORS) == 0;
    capabilities->can_rename = editable;
    capabilities->can_delete = editable &&
        (!folder_info->child ||
         (camel_store_get_flags (store) &
          CAMEL_STORE_CAN_DELETE_FOLDERS_AT_ONCE) != 0);
    /* Refreshing an Inbox or other system folder is safe and follows the
     * original folder action; editing it remains disabled above. */
    capabilities->can_refresh = folder_can_refresh (folder_info);
    /* Camel exposes message transfer, not an atomic folder copy/move API.
     * Do not claim recursive folder transfer support here. */
    capabilities->can_copy = 0;
    capabilities->can_move = 0;
}

static gboolean
folder_mutation_target_is_leaf (const gchar *target_name)
{
    return target_name && *target_name &&
        !strchr (target_name, '/') &&
        strcmp (target_name, ".") != 0 &&
        strcmp (target_name, "..") != 0;
}

static gchar *
folder_mutation_child_name (const gchar *folder_name,
                            const gchar *target_name)
{
    const gchar *separator;

    separator = strrchr (folder_name, '/');
    if (!separator)
        return g_strdup (target_name);

    return g_strdup_printf (
        "%.*s%s", (gint) (separator - folder_name + 1),
        folder_name, target_name);
}

int
evolution_eds_camel_get_folder_mutation_capabilities (
    const char *source_uid,
    const char *folder_name,
    EvolutionEdsCamelFolderMutationCapabilities *out_capabilities,
    char **out_error,
    EvolutionEdsCamelCancellationCallback cancellation_callback,
    void *cancellation_user_data)
{
    ESourceRegistry *registry = NULL;
    EdsCamelSession *session = NULL;
    ESource *source = NULL;
    CamelStore *store = NULL;
    CamelFolderInfo *folder_info = NULL;
    CamelFolderInfo *matching_folder = NULL;
    GCancellable *cancellable = NULL;
    CancellationMonitor *cancellation_monitor = NULL;
    GError *error = NULL;
    gboolean success = FALSE;

    if (out_capabilities)
        memset (out_capabilities, 0, sizeof (*out_capabilities));
    if (out_error)
        *out_error = NULL;
    if (!out_capabilities || !source_uid || !*source_uid ||
        !folder_name || !*folder_name) {
        if (out_error)
            *out_error = g_strdup (
                "Camel-Ordneraktionen benötigen eine genaue Quelle und einen Ordnernamen");
        return 0;
    }
    if (cancellation_requested (cancellation_callback, cancellation_user_data)) {
        if (out_error)
            *out_error = g_strdup ("Camel-Ordneraktionen wurden abgebrochen");
        return 0;
    }

    camel_provider_init ();
    registry = eds_camel_ref_registry (&error);
    if (!registry)
        goto finish;
    source = e_source_registry_ref_source (registry, source_uid);
    if (!source) {
        g_set_error_literal (&error, G_IO_ERROR, G_IO_ERROR_NOT_FOUND,
            "Die EDS-Mailquelle für Ordneraktionen existiert nicht mehr");
        goto finish;
    }
    if (!e_source_get_enabled (source)) {
        g_set_error_literal (&error, G_IO_ERROR, G_IO_ERROR_FAILED,
            "Die EDS-Mailquelle für Ordneraktionen ist deaktiviert");
        goto finish;
    }
    session = eds_camel_ref_session (registry);
    cancellable = g_cancellable_new ();
    cancellation_monitor = cancellation_monitor_start (
        cancellable, cancellation_callback, cancellation_user_data);
    store = open_store_for_source (session, source, cancellable, &error);
    if (!store)
        goto finish;
    folder_info = load_folder_mutation_tree (store, cancellable, &error);
    if (!folder_info)
        goto finish;
    matching_folder = find_folder_info_by_name (folder_info, folder_name);
    if (!matching_folder) {
        g_set_error_literal (&error, G_IO_ERROR, G_IO_ERROR_NOT_FOUND,
            "Der genaue Camel-Ordner für die Aktion wurde nicht gefunden");
        goto finish;
    }
    if (cancellation_requested (cancellation_callback, cancellation_user_data)) {
        set_cancelled_error (&error);
        goto finish;
    }
    set_folder_mutation_capabilities (store, matching_folder, out_capabilities);
    success = TRUE;

finish:
    if (!success && out_error) {
        *out_error = g_strdup (error
            ? error->message
            : "Camel-Ordneraktionen konnten nicht geprüft werden");
    }
    camel_folder_info_free (folder_info);
    g_clear_error (&error);
    cancellation_monitor_free (cancellation_monitor);
    g_clear_object (&cancellable);
    g_clear_object (&store);
    g_clear_object (&session);
    g_clear_object (&source);
    g_clear_object (&registry);

    return success ? 1 : 0;
}

int
evolution_eds_camel_apply_folder_mutation (
    EvolutionEdsCamelFolderMutationKind kind,
    const char *source_uid,
    const char *folder_name,
    const char *target_name,
    char **out_error,
    EvolutionEdsCamelCancellationCallback cancellation_callback,
    void *cancellation_user_data)
{
    ESourceRegistry *registry = NULL;
    EdsCamelSession *session = NULL;
    ESource *source = NULL;
    CamelStore *store = NULL;
    CamelFolder *folder = NULL;
    CamelFolderInfo *folder_info = NULL;
    CamelFolderInfo *subscription_info = NULL;
    CamelFolderInfo *matching_folder = NULL;
    CamelFolderInfo *verified_folder = NULL;
    GCancellable *cancellable = NULL;
    CancellationMonitor *cancellation_monitor = NULL;
    EvolutionEdsCamelFolderMutationCapabilities capabilities;
    gchar *expected_name = NULL;
    GError *error = NULL;
    gboolean success = FALSE;

    if (out_error)
        *out_error = NULL;
    if (!source_uid || !*source_uid || !folder_name || !*folder_name) {
        if (out_error)
            *out_error = g_strdup (
                "Camel-Ordneraktionen benötigen eine genaue Quelle und einen Ordnernamen");
        return 0;
    }
    if ((kind == EVOLUTION_EDS_CAMEL_FOLDER_CREATE_CHILD ||
         kind == EVOLUTION_EDS_CAMEL_FOLDER_RENAME) &&
        !folder_mutation_target_is_leaf (target_name)) {
        if (out_error)
            *out_error = g_strdup (
                "Camel-Ordnernamen müssen ein einzelnes nicht-leeres Segment sein");
        return 0;
    }
    if (kind != EVOLUTION_EDS_CAMEL_FOLDER_CREATE_CHILD &&
        kind != EVOLUTION_EDS_CAMEL_FOLDER_RENAME &&
        kind != EVOLUTION_EDS_CAMEL_FOLDER_DELETE &&
        kind != EVOLUTION_EDS_CAMEL_FOLDER_REFRESH) {
        if (out_error)
            *out_error = g_strdup (
                "Camel bietet keine sichere atomare Ordner-Kopier- oder Verschiebeoperation");
        return 0;
    }
    if (cancellation_requested (cancellation_callback, cancellation_user_data)) {
        if (out_error)
            *out_error = g_strdup ("Camel-Ordneraktion wurde abgebrochen");
        return 0;
    }

    camel_provider_init ();
    registry = eds_camel_ref_registry (&error);
    if (!registry)
        goto finish;
    source = e_source_registry_ref_source (registry, source_uid);
    if (!source) {
        g_set_error_literal (&error, G_IO_ERROR, G_IO_ERROR_NOT_FOUND,
            "Die EDS-Mailquelle für Ordneraktionen existiert nicht mehr");
        goto finish;
    }
    if (!e_source_get_enabled (source)) {
        g_set_error_literal (&error, G_IO_ERROR, G_IO_ERROR_FAILED,
            "Die EDS-Mailquelle für Ordneraktionen ist deaktiviert");
        goto finish;
    }
    session = eds_camel_ref_session (registry);
    cancellable = g_cancellable_new ();
    cancellation_monitor = cancellation_monitor_start (
        cancellable, cancellation_callback, cancellation_user_data);
    store = open_store_for_source (session, source, cancellable, &error);
    if (!store)
        goto finish;
    folder_info = load_folder_mutation_tree (store, cancellable, &error);
    if (!folder_info)
        goto finish;
    matching_folder = find_folder_info_by_name (folder_info, folder_name);
    if (!matching_folder) {
        g_set_error_literal (&error, G_IO_ERROR, G_IO_ERROR_NOT_FOUND,
            "Der genaue Camel-Ordner für die Aktion wurde nicht gefunden");
        goto finish;
    }
    set_folder_mutation_capabilities (store, matching_folder, &capabilities);

    switch (kind) {
    case EVOLUTION_EDS_CAMEL_FOLDER_CREATE_CHILD:
        if (!capabilities.can_create_child) {
            g_set_error_literal (&error, G_IO_ERROR, G_IO_ERROR_NOT_SUPPORTED,
                "Dieser Camel-Ordner unterstützt kein Erstellen von Unterordnern");
            goto finish;
        }
        expected_name = g_strdup_printf ("%s/%s", folder_name, target_name);
        if (find_folder_info_by_name (folder_info, expected_name)) {
            g_set_error_literal (&error, G_IO_ERROR, G_IO_ERROR_EXISTS,
                "Ein Camel-Ordner mit diesem Namen existiert bereits");
            goto finish;
        }
        verified_folder = camel_store_create_folder_sync (
            store, folder_name, target_name, cancellable, &error);
        if (!verified_folder)
            goto finish;
        camel_folder_info_free (verified_folder);
        verified_folder = NULL;
        if (CAMEL_IS_SUBSCRIBABLE (store) &&
            !camel_subscribable_subscribe_folder_sync (
                CAMEL_SUBSCRIBABLE (store), expected_name,
                cancellable, &error))
            goto finish;
        break;
    case EVOLUTION_EDS_CAMEL_FOLDER_RENAME:
        if (!capabilities.can_rename) {
            g_set_error_literal (&error, G_IO_ERROR, G_IO_ERROR_NOT_SUPPORTED,
                "Dieser Camel-Ordner unterstützt kein Umbenennen");
            goto finish;
        }
        expected_name = folder_mutation_child_name (folder_name, target_name);
        if (g_str_equal (expected_name, folder_name)) {
            g_set_error_literal (&error, G_IO_ERROR, G_IO_ERROR_INVALID_ARGUMENT,
                "Der neue Camel-Ordnername unterscheidet sich nicht vom bestehenden Namen");
            goto finish;
        }
        if (find_folder_info_by_name (folder_info, expected_name)) {
            g_set_error_literal (&error, G_IO_ERROR, G_IO_ERROR_EXISTS,
                "Ein Camel-Ordner mit diesem Namen existiert bereits");
            goto finish;
        }
        if (!camel_store_rename_folder_sync (
                store, folder_name, expected_name, cancellable, &error))
            goto finish;
        break;
    case EVOLUTION_EDS_CAMEL_FOLDER_DELETE:
        if (!capabilities.can_delete) {
            g_set_error_literal (&error, G_IO_ERROR, G_IO_ERROR_NOT_SUPPORTED,
                "Dieser Camel-Ordner unterstützt kein sicheres Löschen");
            goto finish;
        }
        if (!camel_store_delete_folder_sync (
                store, folder_name, cancellable, &error))
            goto finish;
        break;
    case EVOLUTION_EDS_CAMEL_FOLDER_REFRESH:
        if (!capabilities.can_refresh) {
            g_set_error_literal (&error, G_IO_ERROR, G_IO_ERROR_NOT_SUPPORTED,
                "Dieser Camel-Ordner unterstützt keine Aktualisierung");
            goto finish;
        }
        folder = camel_store_get_folder_sync (
            store, folder_name, CAMEL_STORE_FOLDER_NONE, cancellable, &error);
        if (!folder)
            goto finish;
        if (!camel_folder_refresh_info_sync (folder, cancellable, &error))
            goto finish;
        expected_name = g_strdup (folder_name);
        break;
    default:
        g_assert_not_reached ();
    }

    camel_folder_info_free (folder_info);
    folder_info = load_folder_mutation_tree (store, cancellable, &error);
    if (!folder_info)
        goto finish;
    if (kind == EVOLUTION_EDS_CAMEL_FOLDER_DELETE) {
        if (find_folder_info_by_name (folder_info, folder_name)) {
            g_set_error_literal (&error, G_IO_ERROR, G_IO_ERROR_FAILED,
                "Der Camel-Server hat das Löschen des Ordners nicht bestätigt");
            goto finish;
        }
    } else {
        verified_folder = find_folder_info_by_name (folder_info, expected_name);
        if (!verified_folder) {
            g_set_error_literal (&error, G_IO_ERROR, G_IO_ERROR_FAILED,
                "Der Camel-Server hat die Ordneränderung nicht bestätigt");
            goto finish;
        }
        if (kind == EVOLUTION_EDS_CAMEL_FOLDER_RENAME &&
            find_folder_info_by_name (folder_info, folder_name)) {
            g_set_error_literal (&error, G_IO_ERROR, G_IO_ERROR_FAILED,
                "Der Camel-Server hat das Umbenennen des Ordners nicht bestätigt");
            goto finish;
        }
    }
    if (kind == EVOLUTION_EDS_CAMEL_FOLDER_CREATE_CHILD &&
        CAMEL_IS_SUBSCRIBABLE (store)) {
        subscription_info = camel_store_get_folder_info_sync (
            store, NULL,
            CAMEL_STORE_FOLDER_INFO_RECURSIVE |
            CAMEL_STORE_FOLDER_INFO_NO_VIRTUAL |
            CAMEL_STORE_FOLDER_INFO_REFRESH |
            CAMEL_STORE_FOLDER_INFO_SUBSCRIPTION_LIST,
            cancellable, &error);
        if (!subscription_info)
            goto finish;
        verified_folder = find_folder_info_by_name (
            subscription_info, expected_name);
        if (!verified_folder ||
            (verified_folder->flags & CAMEL_FOLDER_SUBSCRIBED) == 0) {
            g_set_error_literal (&error, G_IO_ERROR, G_IO_ERROR_FAILED,
                "Der Camel-Server hat das Abonnieren des neuen Ordners nicht bestätigt");
            goto finish;
        }
    }
    if (cancellation_requested (cancellation_callback, cancellation_user_data)) {
        set_cancelled_error (&error);
        goto finish;
    }
    success = TRUE;

finish:
    if (!success && out_error) {
        *out_error = g_strdup (error
            ? error->message
            : "Camel-Ordneraktion konnte nicht bestätigt werden");
    }
    g_free (expected_name);
    camel_folder_info_free (subscription_info);
    camel_folder_info_free (folder_info);
    g_clear_object (&folder);
    g_clear_error (&error);
    cancellation_monitor_free (cancellation_monitor);
    g_clear_object (&cancellable);
    g_clear_object (&store);
    g_clear_object (&session);
    g_clear_object (&source);
    g_clear_object (&registry);

    return success ? 1 : 0;
}

typedef struct {
    EvolutionEdsCamelWatcher *watcher;
    CamelFolder *folder;
    gulong changed_id;
    gulong deleted_id;
    gulong renamed_id;
    gchar *source_uid;
    gchar *folder_name;
} CamelWatcherFolder;

typedef struct {
    EvolutionEdsCamelChangeEvent event;
} CamelWatcherEvent;

struct _EvolutionEdsCamelWatcher {
    GMutex lock;
    GCond condition;
    gboolean stopping;
    guint callback_count;
    GQueue *events;
    GPtrArray *folders;
    GPtrArray *stores;
    ESourceRegistry *registry;
    EdsCamelSession *session;
    gulong source_added_id;
    gulong source_removed_id;
    gulong source_enabled_id;
    gulong source_disabled_id;
};

static char **
copy_uid_array (GPtrArray *uids,
                size_t *out_count)
{
    char **copy;
    guint ii;

    if (out_count)
        *out_count = 0;
    if (!uids || uids->len == 0)
        return NULL;

    copy = g_new0 (char *, uids->len);
    for (ii = 0; ii < uids->len; ii++)
        copy[ii] = g_strdup (g_ptr_array_index (uids, ii));
    if (out_count)
        *out_count = uids->len;

    return copy;
}

static void
free_uid_array (char **uids,
                size_t count)
{
    size_t ii;

    if (!uids)
        return;
    for (ii = 0; ii < count; ii++)
        g_free (uids[ii]);
    g_free (uids);
}

static void
free_watcher_event (CamelWatcherEvent *queued_event)
{
    if (!queued_event)
        return;

    g_free (queued_event->event.source_uid);
    g_free (queued_event->event.folder_name);
    g_free (queued_event->event.old_folder_name);
    g_free (queued_event->event.source_display_name);
    free_uid_array (
        queued_event->event.added_uids,
        queued_event->event.added_uid_count);
    free_uid_array (
        queued_event->event.removed_uids,
        queued_event->event.removed_uid_count);
    free_uid_array (
        queued_event->event.changed_uids,
        queued_event->event.changed_uid_count);
    free_uid_array (
        queued_event->event.recent_uids,
        queued_event->event.recent_uid_count);
    g_free (queued_event);
}

static void
free_watcher_folder (CamelWatcherFolder *watch_folder)
{
    if (!watch_folder)
        return;

    g_clear_object (&watch_folder->folder);
    g_free (watch_folder->source_uid);
    g_free (watch_folder->folder_name);
    g_free (watch_folder);
}

static gboolean
watcher_callback_begin (CamelWatcherFolder *watch_folder)
{
    EvolutionEdsCamelWatcher *watcher = watch_folder->watcher;
    gboolean active;

    g_mutex_lock (&watcher->lock);
    active = !watcher->stopping;
    if (active)
        watcher->callback_count++;
    g_mutex_unlock (&watcher->lock);

    return active;
}

static void
watcher_callback_end (CamelWatcherFolder *watch_folder)
{
    EvolutionEdsCamelWatcher *watcher = watch_folder->watcher;

    g_mutex_lock (&watcher->lock);
    if (watcher->callback_count > 0)
        watcher->callback_count--;
    if (watcher->callback_count == 0)
        g_cond_broadcast (&watcher->condition);
    g_mutex_unlock (&watcher->lock);
}

static void
queue_watcher_event (CamelWatcherFolder *watch_folder,
                     EvolutionEdsCamelChangeKind kind,
                     CamelFolderChangeInfo *changes,
                     const gchar *old_folder_name)
{
    EvolutionEdsCamelWatcher *watcher = watch_folder->watcher;
    CamelWatcherEvent *queued_event;

    g_mutex_lock (&watcher->lock);
    if (watcher->stopping) {
        g_mutex_unlock (&watcher->lock);
        return;
    }

    queued_event = g_new0 (CamelWatcherEvent, 1);
    queued_event->event.kind = kind;
    queued_event->event.source_uid = g_strdup (watch_folder->source_uid);
    queued_event->event.folder_name = g_strdup (kind ==
        EVOLUTION_EDS_CAMEL_FOLDER_RENAMED
        ? camel_folder_get_full_name (watch_folder->folder)
        : watch_folder->folder_name);
    queued_event->event.old_folder_name = g_strdup (old_folder_name);

    if (changes) {
        queued_event->event.added_count = changes->uid_added
            ? changes->uid_added->len : 0;
        queued_event->event.removed_count = changes->uid_removed
            ? changes->uid_removed->len : 0;
        queued_event->event.changed_count = changes->uid_changed
            ? changes->uid_changed->len : 0;
        queued_event->event.recent_count = changes->uid_recent
            ? changes->uid_recent->len : 0;
        queued_event->event.added_uids = copy_uid_array (
            changes->uid_added, &queued_event->event.added_uid_count);
        queued_event->event.removed_uids = copy_uid_array (
            changes->uid_removed, &queued_event->event.removed_uid_count);
        queued_event->event.changed_uids = copy_uid_array (
            changes->uid_changed, &queued_event->event.changed_uid_count);
        queued_event->event.recent_uids = copy_uid_array (
            changes->uid_recent, &queued_event->event.recent_uid_count);
    }

    g_queue_push_tail (watcher->events, queued_event);
    g_cond_signal (&watcher->condition);
    g_mutex_unlock (&watcher->lock);
}

static void
watcher_folder_changed_cb (CamelFolder *folder,
                            CamelFolderChangeInfo *changes,
                            gpointer user_data)
{
    CamelWatcherFolder *watch_folder = user_data;

    (void) folder;
    if (!watcher_callback_begin (watch_folder))
        return;

    queue_watcher_event (
        watch_folder, EVOLUTION_EDS_CAMEL_CHANGE, changes, NULL);
    watcher_callback_end (watch_folder);
}

static void
watcher_folder_deleted_cb (CamelFolder *folder,
                            gpointer user_data)
{
    CamelWatcherFolder *watch_folder = user_data;

    (void) folder;
    if (!watcher_callback_begin (watch_folder))
        return;

    queue_watcher_event (
        watch_folder, EVOLUTION_EDS_CAMEL_FOLDER_DELETED, NULL, NULL);
    watcher_callback_end (watch_folder);
}

static void
watcher_folder_renamed_cb (CamelFolder *folder,
                            const gchar *old_folder_name,
                            gpointer user_data)
{
    CamelWatcherFolder *watch_folder = user_data;

    (void) folder;
    if (!watcher_callback_begin (watch_folder))
        return;

    queue_watcher_event (
        watch_folder, EVOLUTION_EDS_CAMEL_FOLDER_RENAMED,
        NULL, old_folder_name);
    watcher_callback_end (watch_folder);
}

static void
watcher_add_folder (EvolutionEdsCamelWatcher *watcher,
                     CamelFolder *folder,
                     const gchar *source_uid,
                     const gchar *folder_name)
{
    CamelWatcherFolder *watch_folder;

    watch_folder = g_new0 (CamelWatcherFolder, 1);
    watch_folder->watcher = watcher;
    watch_folder->folder = g_object_ref (folder);
    watch_folder->source_uid = g_strdup (source_uid);
    watch_folder->folder_name = g_strdup (folder_name);
    watch_folder->changed_id = g_signal_connect (
        folder, "changed", G_CALLBACK (watcher_folder_changed_cb),
        watch_folder);
    watch_folder->deleted_id = g_signal_connect (
        folder, "deleted", G_CALLBACK (watcher_folder_deleted_cb),
        watch_folder);
    watch_folder->renamed_id = g_signal_connect (
        folder, "renamed", G_CALLBACK (watcher_folder_renamed_cb),
        watch_folder);

    g_ptr_array_add (watcher->folders, watch_folder);
}

static void
watcher_add_folder_tree (EvolutionEdsCamelWatcher *watcher,
                          CamelStore *store,
                          const gchar *source_uid,
                          CamelFolderInfo *folder_info,
                          GCancellable *cancellable)
{
    CamelFolderInfo *current;

    for (current = folder_info; current != NULL; current = current->next) {
        if (is_mail_folder (current) &&
            (current->flags & CAMEL_FOLDER_NOSELECT) == 0 &&
            current->full_name && *current->full_name) {
            CamelFolder *folder;
            GError *error = NULL;

            folder = camel_store_get_folder_sync (
                store, current->full_name, CAMEL_STORE_FOLDER_NONE,
                cancellable, &error);
            if (folder) {
                watcher_add_folder (
                    watcher, folder, source_uid, current->full_name);
                g_object_unref (folder);
            }
            g_clear_error (&error);
        }

        if (current->child)
            watcher_add_folder_tree (
                watcher, store, source_uid, current->child, cancellable);
    }
}

static gboolean
watcher_backend_is_remote (ESource *source)
{
    ESourceBackend *backend;
    const gchar *backend_name;

    backend = E_SOURCE_BACKEND (
        e_source_get_extension (source, E_SOURCE_EXTENSION_MAIL_ACCOUNT));
    backend_name = backend ? e_source_backend_get_backend_name (backend) : NULL;

    if (!backend_name || !*backend_name)
        return FALSE;

    return !g_str_equal (backend_name, "maildir") &&
           !g_str_equal (backend_name, "mbox") &&
           !g_str_equal (backend_name, "mh") &&
           !g_str_equal (backend_name, "spool") &&
           !g_str_equal (backend_name, "vfolder");
}

static void
watcher_remember_error (gchar **first_error,
                        const gchar *scope,
                        GError *error)
{
    if (!*first_error && error)
        *first_error = g_strdup_printf (
            "%s: %s", scope ? scope : "EDS/Camel", error->message);
}

static void
queue_watcher_source_event (EvolutionEdsCamelWatcher *watcher,
                            EvolutionEdsCamelChangeKind kind,
                            const gchar *source_uid,
                            const gchar *source_display_name)
{
    CamelWatcherEvent *queued_event;

    if (!watcher)
        return;

    g_mutex_lock (&watcher->lock);
    if (watcher->stopping) {
        g_mutex_unlock (&watcher->lock);
        return;
    }

    queued_event = g_new0 (CamelWatcherEvent, 1);
    queued_event->event.kind = kind;
    queued_event->event.source_uid = g_strdup (source_uid);
    queued_event->event.source_display_name = g_strdup (source_display_name);

    g_queue_push_tail (watcher->events, queued_event);
    g_cond_signal (&watcher->condition);
    g_mutex_unlock (&watcher->lock);
}

static void
watcher_source_added_cb (ESourceRegistry *registry,
                         ESource *source,
                         gpointer user_data)
{
    EvolutionEdsCamelWatcher *watcher = user_data;
    (void) registry;
    if (!watcher || watcher->stopping)
        return;
    if (!e_source_has_extension (source, E_SOURCE_EXTENSION_MAIL_ACCOUNT))
        return;
    if (!e_source_get_enabled (source))
        return;
    queue_watcher_source_event (
        watcher, EVOLUTION_EDS_CAMEL_SOURCE_ADDED,
        e_source_get_uid (source), e_source_get_display_name (source));
}

static void
watcher_source_removed_cb (ESourceRegistry *registry,
                           ESource *source,
                           gpointer user_data)
{
    EvolutionEdsCamelWatcher *watcher = user_data;
    (void) registry;
    if (!watcher || watcher->stopping)
        return;
    if (!e_source_has_extension (source, E_SOURCE_EXTENSION_MAIL_ACCOUNT))
        return;
    queue_watcher_source_event (
        watcher, EVOLUTION_EDS_CAMEL_SOURCE_REMOVED,
        e_source_get_uid (source), e_source_get_display_name (source));
}

static void
watcher_source_enabled_cb (ESourceRegistry *registry,
                           ESource *source,
                           gpointer user_data)
{
    EvolutionEdsCamelWatcher *watcher = user_data;
    (void) registry;
    if (!watcher || watcher->stopping)
        return;
    if (!e_source_has_extension (source, E_SOURCE_EXTENSION_MAIL_ACCOUNT))
        return;
    queue_watcher_source_event (
        watcher, EVOLUTION_EDS_CAMEL_SOURCE_ENABLED,
        e_source_get_uid (source), e_source_get_display_name (source));
}

static void
watcher_source_disabled_cb (ESourceRegistry *registry,
                            ESource *source,
                            gpointer user_data)
{
    EvolutionEdsCamelWatcher *watcher = user_data;
    (void) registry;
    if (!watcher || watcher->stopping)
        return;
    if (!e_source_has_extension (source, E_SOURCE_EXTENSION_MAIL_ACCOUNT))
        return;
    queue_watcher_source_event (
        watcher, EVOLUTION_EDS_CAMEL_SOURCE_DISABLED,
        e_source_get_uid (source), e_source_get_display_name (source));
}

int
evolution_eds_camel_watcher_new (const char *source_uid,
                                 EvolutionEdsCamelWatcher **out_watcher,
                                 size_t *out_configured_sources,
                                 size_t *out_successful_sources,
                                 size_t *out_failed_sources,
                                 char **out_error)
{
    EvolutionEdsCamelWatcher *watcher;
    GList *sources = NULL;
    GList *link;
    GCancellable *cancellable = NULL;
    gchar *first_error = NULL;
    size_t configured_sources = 0;
    size_t successful_sources = 0;
    size_t failed_sources = 0;
    GError *error = NULL;

    if (out_watcher)
        *out_watcher = NULL;
    if (out_configured_sources)
        *out_configured_sources = 0;
    if (out_successful_sources)
        *out_successful_sources = 0;
    if (out_failed_sources)
        *out_failed_sources = 0;
    if (out_error)
        *out_error = NULL;

    if (!out_watcher)
        return 0;

    camel_provider_init ();
    watcher = g_new0 (EvolutionEdsCamelWatcher, 1);
    g_mutex_init (&watcher->lock);
    g_cond_init (&watcher->condition);
    watcher->events = g_queue_new ();
    watcher->folders = g_ptr_array_new_with_free_func (
        (GDestroyNotify) free_watcher_folder);
    watcher->stores = g_ptr_array_new_with_free_func (
        (GDestroyNotify) g_object_unref);

    watcher->registry = eds_camel_ref_registry (&error);
    if (!watcher->registry) {
        if (out_error && error)
            *out_error = g_strdup (error->message);
        g_clear_error (&error);
        evolution_eds_camel_watcher_free (watcher);
        return 0;
    }

    watcher->session = eds_camel_ref_session (watcher->registry);
    cancellable = g_cancellable_new ();
    sources = e_source_registry_list_enabled (
        watcher->registry, E_SOURCE_EXTENSION_MAIL_ACCOUNT);

    for (link = sources; link != NULL; link = link->next) {
        ESource *source = E_SOURCE (link->data);
        CamelStore *store = NULL;
        CamelFolderInfo *folder_info = NULL;

        if (source_uid &&
            !g_str_equal (source_uid, e_source_get_uid (source)))
            continue;
        if (!watcher_backend_is_remote (source))
            continue;
        if (is_source_foreign_or_shared (source))
            continue;

        configured_sources++;
        store = open_store_for_source (
            watcher->session, source, cancellable, &error);
        if (!store) {
            failed_sources++;
            watcher_remember_error (
                &first_error, e_source_get_display_name (source), error);
            g_clear_error (&error);
            continue;
        }

        folder_info = camel_store_get_folder_info_sync (
            store, NULL,
            CAMEL_STORE_FOLDER_INFO_RECURSIVE |
            CAMEL_STORE_FOLDER_INFO_REFRESH,
            cancellable, &error);
        if (!folder_info) {
            failed_sources++;
            watcher_remember_error (
                &first_error, e_source_get_display_name (source), error);
            g_clear_error (&error);
            g_object_unref (store);
            continue;
        }

        successful_sources++;
        g_ptr_array_add (watcher->stores, store);
        watcher_add_folder_tree (
            watcher, store, e_source_get_uid (source), folder_info,
            cancellable);
        camel_folder_info_free (folder_info);
    }

    if (out_configured_sources)
        *out_configured_sources = configured_sources;
    if (out_successful_sources)
        *out_successful_sources = successful_sources;
    if (out_failed_sources)
        *out_failed_sources = failed_sources;
    if (out_error)
        *out_error = first_error;
    else
        g_free (first_error);

    watcher->source_added_id = g_signal_connect (
        watcher->registry, "source-added",
        G_CALLBACK (watcher_source_added_cb), watcher);
    watcher->source_removed_id = g_signal_connect (
        watcher->registry, "source-removed",
        G_CALLBACK (watcher_source_removed_cb), watcher);
    watcher->source_enabled_id = g_signal_connect (
        watcher->registry, "source-enabled",
        G_CALLBACK (watcher_source_enabled_cb), watcher);
    watcher->source_disabled_id = g_signal_connect (
        watcher->registry, "source-disabled",
        G_CALLBACK (watcher_source_disabled_cb), watcher);

    g_clear_object (&cancellable);
    g_list_free_full (sources, g_object_unref);
    *out_watcher = watcher;
    return 1;
}

int
evolution_eds_camel_watcher_next_event (
    EvolutionEdsCamelWatcher *watcher,
    int timeout_milliseconds,
    EvolutionEdsCamelChangeEvent *out_event)
{
    CamelWatcherEvent *queued_event = NULL;
    gint64 deadline;

    if (!watcher || !out_event)
        return -1;

    memset (out_event, 0, sizeof (*out_event));
    deadline = g_get_monotonic_time () +
        (timeout_milliseconds > 0 ? timeout_milliseconds * 1000 : 0);

    g_mutex_lock (&watcher->lock);
    while (!watcher->stopping && g_queue_is_empty (watcher->events)) {
        if (timeout_milliseconds < 0) {
            g_cond_wait (&watcher->condition, &watcher->lock);
        } else if (timeout_milliseconds == 0 ||
                   !g_cond_wait_until (
                       &watcher->condition, &watcher->lock, deadline)) {
            break;
        }
    }

    if (!g_queue_is_empty (watcher->events))
        queued_event = g_queue_pop_head (watcher->events);
    else if (watcher->stopping) {
        g_mutex_unlock (&watcher->lock);
        return -1;
    }
    g_mutex_unlock (&watcher->lock);

    if (!queued_event)
        return 0;

    *out_event = queued_event->event;
    g_free (queued_event);
    return 1;
}

void
evolution_eds_camel_free_change_event (EvolutionEdsCamelChangeEvent *event)
{
    if (!event)
        return;

    g_free (event->source_uid);
    g_free (event->folder_name);
    g_free (event->old_folder_name);
    g_free (event->source_display_name);
    free_uid_array (event->added_uids, event->added_uid_count);
    free_uid_array (event->removed_uids, event->removed_uid_count);
    free_uid_array (event->changed_uids, event->changed_uid_count);
    free_uid_array (event->recent_uids, event->recent_uid_count);
    memset (event, 0, sizeof (*event));
}

void
evolution_eds_camel_watcher_free (EvolutionEdsCamelWatcher *watcher)
{
    guint ii;

    if (!watcher)
        return;

    g_mutex_lock (&watcher->lock);
    watcher->stopping = TRUE;
    g_cond_broadcast (&watcher->condition);
    g_mutex_unlock (&watcher->lock);

    if (watcher->source_added_id && watcher->registry)
        g_signal_handler_disconnect (watcher->registry, watcher->source_added_id);
    if (watcher->source_removed_id && watcher->registry)
        g_signal_handler_disconnect (watcher->registry, watcher->source_removed_id);
    if (watcher->source_enabled_id && watcher->registry)
        g_signal_handler_disconnect (watcher->registry, watcher->source_enabled_id);
    if (watcher->source_disabled_id && watcher->registry)
        g_signal_handler_disconnect (watcher->registry, watcher->source_disabled_id);
    watcher->source_added_id = 0;
    watcher->source_removed_id = 0;
    watcher->source_enabled_id = 0;
    watcher->source_disabled_id = 0;

    for (ii = 0; ii < watcher->folders->len; ii++) {
        CamelWatcherFolder *watch_folder = g_ptr_array_index (
            watcher->folders, ii);

        if (watch_folder->changed_id)
            g_signal_handler_disconnect (
                watch_folder->folder, watch_folder->changed_id);
        if (watch_folder->deleted_id)
            g_signal_handler_disconnect (
                watch_folder->folder, watch_folder->deleted_id);
        if (watch_folder->renamed_id)
            g_signal_handler_disconnect (
                watch_folder->folder, watch_folder->renamed_id);
        watch_folder->changed_id = 0;
        watch_folder->deleted_id = 0;
        watch_folder->renamed_id = 0;
    }

    g_mutex_lock (&watcher->lock);
    while (watcher->callback_count > 0)
        g_cond_wait (&watcher->condition, &watcher->lock);
    g_mutex_unlock (&watcher->lock);

    while (!g_queue_is_empty (watcher->events)) {
        CamelWatcherEvent *queued_event = g_queue_pop_head (watcher->events);
        free_watcher_event (queued_event);
    }
    g_queue_free (watcher->events);
    g_ptr_array_free (watcher->folders, TRUE);
    g_ptr_array_free (watcher->stores, TRUE);
    g_clear_object (&watcher->session);
    g_clear_object (&watcher->registry);
    g_cond_clear (&watcher->condition);
    g_mutex_clear (&watcher->lock);
    g_free (watcher);
}

static gboolean
folder_matches_type_by_name (CamelFolderInfo *info, CamelFolderInfoFlags type)
{
    if (!info)
        return FALSE;

    const gchar *display = info->display_name ? info->display_name : "";
    const gchar *full = info->full_name ? info->full_name : "";

    switch (type) {
    case CAMEL_FOLDER_TYPE_DRAFTS:
        return g_ascii_strcasecmp (display, "Drafts") == 0 ||
               g_ascii_strcasecmp (display, "Entwürfe") == 0 ||
               g_ascii_strcasecmp (display, "Entwurf") == 0 ||
               g_ascii_strcasecmp (display, "Brouillons") == 0 ||
               g_ascii_strcasecmp (display, "Borradores") == 0 ||
               g_ascii_strcasecmp (display, "Bozze") == 0 ||
               g_str_has_suffix (full, "/Drafts") ||
               g_str_has_suffix (full, "/Entwürfe") ||
               g_str_has_suffix (full, "/Entwurf") ||
               g_ascii_strcasecmp (full, "Drafts") == 0 ||
               g_ascii_strcasecmp (full, "Entwürfe") == 0;

    case CAMEL_FOLDER_TYPE_SENT:
        return g_ascii_strcasecmp (display, "Sent") == 0 ||
               g_ascii_strcasecmp (display, "Sent Items") == 0 ||
               g_ascii_strcasecmp (display, "Sent Messages") == 0 ||
               g_ascii_strcasecmp (display, "Gesendet") == 0 ||
               g_ascii_strcasecmp (display, "Gesendete Elemente") == 0 ||
               g_ascii_strcasecmp (display, "Gesendete Objekte") == 0 ||
               g_ascii_strcasecmp (display, "Éléments envoyés") == 0 ||
               g_str_has_suffix (full, "/Sent") ||
               g_str_has_suffix (full, "/Sent Items") ||
               g_str_has_suffix (full, "/Gesendet") ||
               g_str_has_suffix (full, "/Gesendete Elemente") ||
               g_ascii_strcasecmp (full, "Sent") == 0 ||
               g_ascii_strcasecmp (full, "Gesendete Elemente") == 0;

    case CAMEL_FOLDER_TYPE_TRASH:
        return g_ascii_strcasecmp (display, "Trash") == 0 ||
               g_ascii_strcasecmp (display, "Deleted Items") == 0 ||
               g_ascii_strcasecmp (display, "Deleted Messages") == 0 ||
               g_ascii_strcasecmp (display, "Papierkorb") == 0 ||
               g_ascii_strcasecmp (display, "Gelöschte Elemente") == 0 ||
               g_ascii_strcasecmp (display, "Gelöschte Objekte") == 0 ||
               g_str_has_suffix (full, "/Trash") ||
               g_str_has_suffix (full, "/Deleted Items") ||
               g_str_has_suffix (full, "/Papierkorb") ||
               g_str_has_suffix (full, "/Gelöschte Elemente") ||
               g_ascii_strcasecmp (full, "Trash") == 0 ||
               g_ascii_strcasecmp (full, "Deleted Items") == 0;

    case CAMEL_FOLDER_TYPE_JUNK:
        return g_ascii_strcasecmp (display, "Junk") == 0 ||
               g_ascii_strcasecmp (display, "Junk Email") == 0 ||
               g_ascii_strcasecmp (display, "Junk E-Mail") == 0 ||
               g_ascii_strcasecmp (display, "Spam") == 0 ||
               g_ascii_strcasecmp (display, "Unerwünschte E-Mails") == 0 ||
               g_str_has_suffix (full, "/Junk") ||
               g_str_has_suffix (full, "/Junk Email") ||
               g_str_has_suffix (full, "/Spam") ||
               g_ascii_strcasecmp (full, "Junk") == 0 ||
               g_ascii_strcasecmp (full, "Spam") == 0;

    case CAMEL_FOLDER_TYPE_INBOX:
        return g_ascii_strcasecmp (display, "Inbox") == 0 ||
               g_ascii_strcasecmp (display, "Posteingang") == 0 ||
               g_ascii_strcasecmp (display, "Boîte de réception") == 0 ||
               g_str_has_suffix (full, "/Inbox") ||
               g_str_has_suffix (full, "/Posteingang") ||
               g_ascii_strcasecmp (full, "Inbox") == 0 ||
               g_ascii_strcasecmp (full, "INBOX") == 0;

    default:
        return FALSE;
    }
}

static const gchar *
find_folder_name_by_type_pass (CamelFolderInfo *folder_info,
                               CamelFolderInfoFlags type,
                               gboolean match_name_fallback)
{
    CamelFolderInfo *current;

    for (current = folder_info; current != NULL; current = current->next) {
        if (is_mail_folder (current) &&
            (current->flags & CAMEL_FOLDER_NOSELECT) == 0 &&
            current->full_name && *current->full_name) {
            if ((current->flags & CAMEL_FOLDER_TYPE_MASK) == type)
                return current->full_name;
            if (match_name_fallback && folder_matches_type_by_name (current, type))
                return current->full_name;
        }

        if (current->child) {
            const gchar *child_name = find_folder_name_by_type_pass (
                current->child, type, match_name_fallback);
            if (child_name)
                return child_name;
        }
    }

    return NULL;
}

static const gchar *
find_folder_name_by_type (CamelFolderInfo *folder_info,
                          CamelFolderInfoFlags type)
{
    const gchar *found = find_folder_name_by_type_pass (folder_info, type, FALSE);
    if (!found)
        found = find_folder_name_by_type_pass (folder_info, type, TRUE);
    return found;
}

static GPtrArray *
single_message_uid (const gchar *message_uid)
{
    GPtrArray *uids = g_ptr_array_new_with_free_func (g_free);

    g_ptr_array_add (uids, g_strdup (message_uid));
    return uids;
}

static gboolean
synchronize_message_flags (CamelFolder *folder,
                           const gchar *message_uid,
                           guint32 mask,
                           guint32 set,
                           GCancellable *cancellable,
                           GError **error)
{
    if (!camel_folder_set_message_flags (folder, message_uid, mask, set)) {
        g_set_error_literal (
            error, G_IO_ERROR, G_IO_ERROR_FAILED,
            "Camel refused to change the message flags");
        return FALSE;
    }

    return camel_folder_synchronize_message_sync (
        folder, message_uid, cancellable, error);
}

static gboolean
transfer_one_message (CamelFolder *source,
                      CamelFolder *destination,
                      const gchar *message_uid,
                      gboolean delete_original,
                      GCancellable *cancellable,
                      GError **error)
{
    GPtrArray *uids;
    GPtrArray *transferred_uids = NULL;
    gboolean success;

    uids = single_message_uid (message_uid);
    success = camel_folder_transfer_messages_to_sync (
        source, uids, destination, delete_original,
        &transferred_uids, cancellable, error);

    if (success)
        success = camel_folder_synchronize_sync (
            source, FALSE, cancellable, error);
    if (success && destination != source)
        success = camel_folder_synchronize_sync (
            destination, FALSE, cancellable, error);

    g_clear_pointer (&transferred_uids, g_ptr_array_unref);
    g_ptr_array_unref (uids);
    return success;
}

static gboolean
message_exists (CamelFolder *folder,
                const gchar *message_uid)
{
    CamelMessageInfo *message_info;

    message_info = camel_folder_get_message_info (folder, message_uid);
    if (!message_info)
        return FALSE;

    g_object_unref (message_info);
    return TRUE;
}

static gboolean
message_ids_equal (const gchar *left,
                   const gchar *right)
{
    gchar *left_copy;
    gchar *right_copy;
    gsize left_length;
    gsize right_length;
    gboolean equal;

    if (!left || !right || !*left || !*right)
        return FALSE;

    left_copy = g_strdup (left);
    right_copy = g_strdup (right);
    g_strstrip (left_copy);
    g_strstrip (right_copy);

    left_length = strlen (left_copy);
    if (left_length >= 2 && left_copy[0] == '<' &&
        left_copy[left_length - 1] == '>') {
        memmove (left_copy, left_copy + 1, left_length - 2);
        left_copy[left_length - 2] = '\0';
    }

    right_length = strlen (right_copy);
    if (right_length >= 2 && right_copy[0] == '<' &&
        right_copy[right_length - 1] == '>') {
        memmove (right_copy, right_copy + 1, right_length - 2);
        right_copy[right_length - 2] = '\0';
    }

    equal = g_ascii_strcasecmp (left_copy, right_copy) == 0;
    g_free (left_copy);
    g_free (right_copy);
    return equal;
}

static gchar *
find_message_uid_by_id (CamelFolder *folder,
                        const gchar *message_id,
                        GCancellable *cancellable)
{
    GPtrArray *uids;
    gchar *match = NULL;

    if (!message_id || !*message_id)
        return NULL;

    #if EDS_BRIDGE_HAS_DUP_UIDS
    uids = camel_folder_dup_uids (folder);
    #else
    uids = camel_folder_get_uids (folder);
    #endif
    if (!uids)
        return NULL;

    for (guint index = 0; index < uids->len; index++) {
        const gchar *uid = g_ptr_array_index (uids, index);
        CamelMimeMessage *message;
        GError *local_error = NULL;

        if (!uid || !*uid)
            continue;

        message = camel_folder_get_message_sync (
            folder, uid, cancellable, &local_error);
        if (!message) {
            g_clear_error (&local_error);
            continue;
        }

        if (message_ids_equal (
                camel_mime_message_get_message_id (message), message_id))
            match = g_strdup (uid);

        g_object_unref (message);
        if (match)
            break;
    }

    #if EDS_BRIDGE_HAS_DUP_UIDS
    g_ptr_array_unref (uids);
    #else
    camel_folder_free_uids (folder, uids);
    #endif
    return match;
}

static void
set_conflict_error (GError **error,
                    const gchar *message)
{
    g_set_error (
        error, G_IO_ERROR, G_IO_ERROR_FAILED,
        "conflict: %s", message);
}

static gboolean
verify_message_flags (CamelFolder *folder,
                      const gchar *message_uid,
                      guint32 mask,
                      guint32 expected,
                      GError **error)
{
    CamelMessageInfo *message_info;
    guint32 actual;

    message_info = camel_folder_get_message_info (folder, message_uid);
    if (!message_info) {
        set_conflict_error (
            error, "Die Nachricht wurde während der Änderung entfernt");
        return FALSE;
    }

    actual = camel_message_info_get_flags (message_info);
    g_object_unref (message_info);

    if ((actual & mask) != (expected & mask)) {
        set_conflict_error (
            error, "Der Serverzustand der Nachricht hat sich gleichzeitig geändert");
        return FALSE;
    }

    return TRUE;
}

static gboolean
verify_message_follow_up (CamelFolder *folder,
                          const gchar *message_uid,
                          gint expected_status,
                          const gchar *expected_follow_up,
                          const gchar *expected_completed_on,
                          const gchar *expected_due_by,
                          GError **error)
{
    CamelMessageInfo *message_info;
    const gchar *follow_up;
    const gchar *completed_on;
    const gchar *due_by;
    gint actual_status;

    message_info = camel_folder_get_message_info (folder, message_uid);
    if (!message_info) {
        set_conflict_error (
            error, "Die Nachricht wurde während der Follow-up-Änderung entfernt");
        return FALSE;
    }

    follow_up = camel_message_info_get_user_tag (
        message_info, EVOLUTION_FOLLOW_UP_TAG);
    completed_on = camel_message_info_get_user_tag (
        message_info, EVOLUTION_COMPLETED_ON_TAG);
    due_by = camel_message_info_get_user_tag (
        message_info, EVOLUTION_DUE_BY_TAG);
    actual_status = follow_up && *follow_up
        ? (completed_on && *completed_on
            ? EVOLUTION_FOLLOW_UP_COMPLETED
            : EVOLUTION_FOLLOW_UP_ACTIVE)
        : EVOLUTION_FOLLOW_UP_NONE;

    if (actual_status != expected_status ||
        g_strcmp0 (follow_up && *follow_up ? follow_up : NULL,
                   expected_follow_up) != 0 ||
        g_strcmp0 (completed_on && *completed_on ? completed_on : NULL,
                   expected_completed_on) != 0 ||
        g_strcmp0 (due_by && *due_by ? due_by : NULL,
                   expected_due_by) != 0) {
        set_conflict_error (
            error, "Der Serverzustand der Follow-up-Tags hat sich gleichzeitig geändert");
        g_object_unref (message_info);
        return FALSE;
    }

    g_object_unref (message_info);
    return TRUE;
}

static gboolean
synchronize_message_follow_up (CamelFolder *folder,
                               const gchar *message_uid,
                               gint follow_up_status,
                               const gchar *follow_up_override,
                               const gchar *due_by_override,
                               GCancellable *cancellable,
                               EvolutionEdsCamelCancellationCallback cancellation_callback,
                               void *cancellation_user_data,
                               GError **error)
{
    CamelMessageInfo *message_info = NULL;
    const gchar *existing_follow_up;
    const gchar *existing_due_by;
    gchar *follow_up_value = NULL;
    gchar *due_by_value = NULL;
    gchar *completed_on_value = NULL;
    gboolean success = FALSE;

    if (cancellation_requested (
            cancellation_callback, cancellation_user_data))
        return set_cancelled_error (error);

    message_info = camel_folder_get_message_info (folder, message_uid);
    if (!message_info) {
        set_conflict_error (
            error, "Die Nachricht existiert im Quellordner nicht mehr");
        return FALSE;
    }

    existing_follow_up = camel_message_info_get_user_tag (
        message_info, EVOLUTION_FOLLOW_UP_TAG);
    existing_due_by = camel_message_info_get_user_tag (
        message_info, EVOLUTION_DUE_BY_TAG);

    switch (follow_up_status) {
    case EVOLUTION_FOLLOW_UP_NONE:
        break;
    case EVOLUTION_FOLLOW_UP_ACTIVE:
        follow_up_value = g_strdup (follow_up_override && *follow_up_override
            ? follow_up_override
            : (existing_follow_up && *existing_follow_up
                ? existing_follow_up
                : EVOLUTION_FOLLOW_UP_DEFAULT));
        due_by_value = g_strdup (due_by_override
            ? (*due_by_override ? due_by_override : NULL)
            : (existing_due_by && *existing_due_by ? existing_due_by : NULL));
        break;
    case EVOLUTION_FOLLOW_UP_COMPLETED:
        follow_up_value = g_strdup (follow_up_override && *follow_up_override
            ? follow_up_override
            : (existing_follow_up && *existing_follow_up
                ? existing_follow_up
                : EVOLUTION_FOLLOW_UP_DEFAULT));
        completed_on_value = camel_header_format_date (time (NULL), 0);
        due_by_value = g_strdup (due_by_override
            ? (*due_by_override ? due_by_override : NULL)
            : (existing_due_by && *existing_due_by ? existing_due_by : NULL));
        break;
    default:
        g_set_error_literal (
            error, G_IO_ERROR, G_IO_ERROR_INVALID_ARGUMENT,
            "Unbekannter Evolution-Follow-up-Status");
        g_object_unref (message_info);
        return FALSE;
    }

    if (cancellation_requested (
            cancellation_callback, cancellation_user_data)) {
        g_object_unref (message_info);
        g_free (follow_up_value);
        g_free (due_by_value);
        g_free (completed_on_value);
        return set_cancelled_error (error);
    }

    camel_message_info_freeze_notifications (message_info);
    if (follow_up_status == EVOLUTION_FOLLOW_UP_NONE) {
        camel_message_info_set_user_tag (
            message_info, EVOLUTION_FOLLOW_UP_TAG, NULL);
        camel_message_info_set_user_tag (
            message_info, EVOLUTION_DUE_BY_TAG, NULL);
        camel_message_info_set_user_tag (
            message_info, EVOLUTION_COMPLETED_ON_TAG, NULL);
    } else {
        camel_message_info_set_user_tag (
            message_info, EVOLUTION_FOLLOW_UP_TAG, follow_up_value);
        camel_message_info_set_user_tag (
            message_info, EVOLUTION_DUE_BY_TAG, due_by_value);
        camel_message_info_set_user_tag (
            message_info, EVOLUTION_COMPLETED_ON_TAG, completed_on_value);
    }
    camel_message_info_thaw_notifications (message_info);
    g_object_unref (message_info);

    if (cancellation_requested (
            cancellation_callback, cancellation_user_data)) {
        set_cancelled_error (error);
        goto finish;
    }

    success = camel_folder_synchronize_message_sync (
        folder, message_uid, cancellable, error);
    if (success)
        success = verify_message_follow_up (
            folder, message_uid, follow_up_status,
            follow_up_status == EVOLUTION_FOLLOW_UP_NONE
                ? NULL : follow_up_value,
            follow_up_status == EVOLUTION_FOLLOW_UP_COMPLETED
                ? completed_on_value : NULL,
            follow_up_status == EVOLUTION_FOLLOW_UP_NONE
                ? NULL : due_by_value,
            error);

finish:
    g_free (follow_up_value);
    g_free (due_by_value);
    g_free (completed_on_value);
    return success;
}

static gboolean
verify_message_moved (CamelFolder *source,
                      const gchar *message_uid,
                      GError **error)
{
    if (message_exists (source, message_uid)) {
        set_conflict_error (
            error, "Die Nachricht ist nach der Serveränderung noch im Quellordner");
        return FALSE;
    }

    return TRUE;
}

int
evolution_eds_camel_apply_operation (
    const EvolutionEdsCamelOperation *operation,
    char **out_error,
    EvolutionEdsCamelCancellationCallback cancellation_callback,
    void *cancellation_user_data)
{
    ESourceRegistry *registry = NULL;
    EdsCamelSession *session = NULL;
    ESource *source = NULL;
    CamelStore *store = NULL;
    CamelFolder *folder = NULL;
    CamelFolder *destination = NULL;
    CamelFolder *restore_source = NULL;
    CamelFolder *trash = NULL;
    CamelFolderInfo *folder_info = NULL;
    GCancellable *cancellable = NULL;
    CancellationMonitor *cancellation_monitor = NULL;
    GError *error = NULL;
    const gchar *destination_name = NULL;
    const gchar *message_uid;
    gchar *resolved_message_uid = NULL;
    gboolean success = FALSE;
    gboolean conflict = FALSE;

    if (out_error)
        *out_error = NULL;

    if (!operation || !operation->source_uid ||
        !*operation->source_uid || !operation->folder_name ||
        !*operation->folder_name || !operation->message_uid ||
        !*operation->message_uid) {
        if (out_error)
            *out_error = g_strdup (
                "EDS/Camel operation is missing a source, folder or message UID");
        return 0;
    }

    if (cancellation_requested (cancellation_callback, cancellation_user_data)) {
        if (out_error)
            *out_error = g_strdup ("EDS/Camel-Änderung wurde abgebrochen");
        return 0;
    }

    message_uid = operation->message_uid;

    camel_provider_init ();
    registry = eds_camel_ref_registry (&error);
    if (!registry)
        goto finish;

    source = e_source_registry_ref_source (registry, operation->source_uid);
    if (!source) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_NOT_FOUND,
            "EDS source for the message no longer exists");
        goto finish;
    }

    if (!e_source_get_enabled (source)) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_FAILED,
            "EDS source for the message is disabled");
        goto finish;
    }

    session = eds_camel_ref_session (registry);
    cancellable = g_cancellable_new ();
    cancellation_monitor = cancellation_monitor_start (
        cancellable, cancellation_callback, cancellation_user_data);
    store = open_store_for_source (session, source, cancellable, &error);
    if (!store)
        goto finish;

    folder = camel_store_get_folder_sync (
        store, operation->folder_name, CAMEL_STORE_FOLDER_NONE,
        cancellable, &error);
    if (!folder)
        goto finish;

    if (operation->kind != EVOLUTION_EDS_CAMEL_RESTORE_DELETED &&
        operation->kind != EVOLUTION_EDS_CAMEL_RESTORE_ARCHIVED &&
        !message_exists (folder, message_uid)) {
        resolved_message_uid = find_message_uid_by_id (
            folder, operation->message_id, cancellable);
        if (resolved_message_uid)
            message_uid = resolved_message_uid;
        if (!message_exists (folder, message_uid)) {
            set_conflict_error (
                &error, "Die Nachricht existiert im Quellordner nicht mehr");
            conflict = TRUE;
            goto finish;
        }
    }

    switch (operation->kind) {
    case EVOLUTION_EDS_CAMEL_SET_MESSAGE_STATE: {
        guint32 mask = CAMEL_MESSAGE_SEEN | CAMEL_MESSAGE_FLAGGED;
        guint32 set = 0;

        if (!operation->is_unread)
            set |= CAMEL_MESSAGE_SEEN;
        if (operation->is_flagged)
            set |= CAMEL_MESSAGE_FLAGGED;

        success = synchronize_message_flags (
            folder, message_uid, mask, set,
            cancellable, &error);
        if (success)
            success = verify_message_flags (
                folder, message_uid, mask, set, &error);
        break;
    }
    case EVOLUTION_EDS_CAMEL_SET_JUNK: {
        guint32 set = operation->is_junk
            ? CAMEL_MESSAGE_JUNK
            : CAMEL_MESSAGE_NOTJUNK;

        success = synchronize_message_flags (
            folder, message_uid,
            CAMEL_MESSAGE_JUNK | CAMEL_MESSAGE_NOTJUNK,
            set, cancellable, &error);
        if (success)
            success = verify_message_flags (
                folder, message_uid,
                CAMEL_MESSAGE_JUNK | CAMEL_MESSAGE_NOTJUNK,
                set, &error);
        break;
    }
    case EVOLUTION_EDS_CAMEL_SET_IMPORTANT:
        success = synchronize_message_flags (
            folder, message_uid,
            CAMEL_MESSAGE_FLAGGED,
            operation->is_important ? CAMEL_MESSAGE_FLAGGED : 0,
            cancellable, &error);
        if (success)
            success = verify_message_flags (
                folder, message_uid,
                CAMEL_MESSAGE_FLAGGED,
                operation->is_important ? CAMEL_MESSAGE_FLAGGED : 0,
                &error);
        break;
    case EVOLUTION_EDS_CAMEL_SET_FOLLOW_UP:
        success = synchronize_message_follow_up (
            folder, message_uid, operation->follow_up_status,
            NULL, operation->due_by, cancellable,
            cancellation_callback, cancellation_user_data, &error);
        break;
    case EVOLUTION_EDS_CAMEL_MOVE_MESSAGE:
    case EVOLUTION_EDS_CAMEL_ARCHIVE_MESSAGE:
        if (operation->kind == EVOLUTION_EDS_CAMEL_MOVE_MESSAGE) {
            destination_name = operation->destination_folder;
            if (!destination_name || !*destination_name) {
                g_set_error_literal (
                    &error, G_IO_ERROR, G_IO_ERROR_INVALID_ARGUMENT,
                    "Move operation has no destination folder");
                break;
            }
        } else {
            folder_info = camel_store_get_folder_info_sync (
                store, NULL,
                CAMEL_STORE_FOLDER_INFO_RECURSIVE |
                CAMEL_STORE_FOLDER_INFO_NO_VIRTUAL |
                CAMEL_STORE_FOLDER_INFO_REFRESH,
                cancellable, &error);
            if (!folder_info)
                break;
            destination_name = find_folder_name_by_type (
                folder_info, CAMEL_FOLDER_TYPE_ARCHIVE);
            if (!destination_name) {
                g_set_error_literal (
                    &error, G_IO_ERROR, G_IO_ERROR_NOT_FOUND,
                    "No Archive folder is configured for this EDS account");
                break;
            }
        }

        destination = camel_store_get_folder_sync (
            store, destination_name, CAMEL_STORE_FOLDER_NONE,
            cancellable, &error);
        if (!destination)
            break;

        success = transfer_one_message (
            folder, destination, message_uid, TRUE,
            cancellable, &error);
        if (success)
            success = verify_message_moved (
                folder, message_uid, &error);
        break;
    case EVOLUTION_EDS_CAMEL_DELETE_MESSAGE:
        trash = camel_store_get_trash_folder_sync (
            store, cancellable, &error);
        if (trash) {
            const gchar *folder_name = camel_folder_get_full_name (folder);
            const gchar *trash_name = camel_folder_get_full_name (trash);

            if (g_strcmp0 (folder_name, trash_name) != 0) {
                success = transfer_one_message (
                    folder, trash, message_uid, TRUE,
                    cancellable, &error);
                if (success)
                    success = verify_message_moved (
                        folder, message_uid, &error);
                break;
            }
        } else {
            /* Some providers have no trash folder; fall back to a real
             * deleted flag and expunge rather than reporting a false success. */
            g_clear_error (&error);
        }

        if (camel_folder_set_message_flags (
                folder, message_uid,
                CAMEL_MESSAGE_DELETED | CAMEL_MESSAGE_SEEN,
                CAMEL_MESSAGE_DELETED | CAMEL_MESSAGE_SEEN)) {
            success = camel_folder_synchronize_sync (
                folder, TRUE, cancellable, &error);
            if (success)
                success = verify_message_moved (
                    folder, message_uid, &error);
        } else {
            g_set_error_literal (
                &error, G_IO_ERROR, G_IO_ERROR_FAILED,
                "Camel refused to delete the message");
        }
        break;
    case EVOLUTION_EDS_CAMEL_RESTORE_DELETED:
        restore_source = camel_store_get_trash_folder_sync (
            store, cancellable, &error);
        if (!restore_source)
            break;
        if (g_strcmp0 (
                camel_folder_get_full_name (restore_source),
                camel_folder_get_full_name (folder)) == 0) {
            g_set_error_literal (
                &error, G_IO_ERROR, G_IO_ERROR_INVALID_ARGUMENT,
                "Deleted message cannot be restored into the same Trash folder");
            break;
        }
        g_clear_pointer (&resolved_message_uid, g_free);
        message_uid = operation->message_uid;
        if (!message_exists (restore_source, message_uid))
            resolved_message_uid = find_message_uid_by_id (
                restore_source, operation->message_id, cancellable);
        if (resolved_message_uid)
            message_uid = resolved_message_uid;
        if (!message_exists (restore_source, message_uid)) {
            set_conflict_error (
                &error, "Die gelöschte Nachricht existiert im Papierkorb nicht mehr");
            conflict = TRUE;
            break;
        }
        success = transfer_one_message (
            restore_source, folder, message_uid, TRUE,
            cancellable, &error);
        if (success)
            success = verify_message_moved (
                restore_source, message_uid, &error);
        break;
    case EVOLUTION_EDS_CAMEL_RESTORE_ARCHIVED:
        folder_info = camel_store_get_folder_info_sync (
            store, NULL,
            CAMEL_STORE_FOLDER_INFO_RECURSIVE |
            CAMEL_STORE_FOLDER_INFO_NO_VIRTUAL |
            CAMEL_STORE_FOLDER_INFO_REFRESH,
            cancellable, &error);
        if (!folder_info)
            break;
        destination_name = find_folder_name_by_type (
            folder_info, CAMEL_FOLDER_TYPE_ARCHIVE);
        if (!destination_name) {
            g_set_error_literal (
                &error, G_IO_ERROR, G_IO_ERROR_NOT_FOUND,
                "No Archive folder is configured for this EDS account");
            break;
        }
        restore_source = camel_store_get_folder_sync (
            store, destination_name, CAMEL_STORE_FOLDER_NONE,
            cancellable, &error);
        if (!restore_source)
            break;
        if (g_strcmp0 (
                camel_folder_get_full_name (restore_source),
                camel_folder_get_full_name (folder)) == 0) {
            g_set_error_literal (
                &error, G_IO_ERROR, G_IO_ERROR_INVALID_ARGUMENT,
                "Archived message cannot be restored into the same Archive folder");
            break;
        }
        g_clear_pointer (&resolved_message_uid, g_free);
        message_uid = operation->message_uid;
        if (!message_exists (restore_source, message_uid))
            resolved_message_uid = find_message_uid_by_id (
                restore_source, operation->message_id, cancellable);
        if (resolved_message_uid)
            message_uid = resolved_message_uid;
        if (!message_exists (restore_source, message_uid)) {
            set_conflict_error (
                &error, "Die archivierte Nachricht existiert im Archiv nicht mehr");
            conflict = TRUE;
            break;
        }
        success = transfer_one_message (
            restore_source, folder, message_uid, TRUE,
            cancellable, &error);
        if (success)
            success = verify_message_moved (
                restore_source, message_uid, &error);
        break;
    default:
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_NOT_SUPPORTED,
            "Unsupported EDS/Camel mail operation");
        break;
    }

finish:
    if (!success && cancellation_requested (
            cancellation_callback, cancellation_user_data) && !error)
        set_cancelled_error (&error);
    conflict = conflict || (error && error->message &&
        g_str_has_prefix (error->message, "conflict:"));
    if (!success && out_error && error)
        *out_error = g_strdup (error->message);
    g_clear_error (&error);
    camel_folder_info_free (folder_info);
    g_clear_object (&trash);
    g_clear_object (&restore_source);
    g_clear_pointer (&resolved_message_uid, g_free);
    g_clear_object (&destination);
    g_clear_object (&folder);
    g_clear_object (&store);
    cancellation_monitor_free (cancellation_monitor);
    g_clear_object (&cancellable);
    g_clear_object (&session);
    g_clear_object (&source);
    g_clear_object (&registry);

    return success ? 1 : (conflict ? 2 : 0);
}

static CamelFolder *
open_special_folder (CamelStore *store,
                     ESource *identity_source,
                     ESource *account_source,
                     CamelFolderInfoFlags folder_type,
                     gboolean drafts,
                     GCancellable *cancellable,
                     GError **error)
{
    ESourceMailComposition *composition_extension = NULL;
    ESourceMailSubmission *submission_extension = NULL;
    CamelFolderInfo *folder_info = NULL;
    CamelFolder *folder = NULL;
    gchar *configured_folder = NULL;
    const gchar *folder_name = NULL;

    if (drafts) {
        if (identity_source)
            composition_extension = (ESourceMailComposition *) e_source_get_extension (
                identity_source, E_SOURCE_EXTENSION_MAIL_COMPOSITION);
        if (!composition_extension && account_source)
            composition_extension = (ESourceMailComposition *) e_source_get_extension (
                account_source, E_SOURCE_EXTENSION_MAIL_COMPOSITION);
        if (composition_extension)
            configured_folder = e_source_mail_composition_dup_drafts_folder (
                composition_extension);
    } else {
        if (identity_source)
            submission_extension = (ESourceMailSubmission *) e_source_get_extension (
                identity_source, E_SOURCE_EXTENSION_MAIL_SUBMISSION);
        if (!submission_extension && account_source)
            submission_extension = (ESourceMailSubmission *) e_source_get_extension (
                account_source, E_SOURCE_EXTENSION_MAIL_SUBMISSION);
        if (submission_extension)
            configured_folder = e_source_mail_submission_dup_sent_folder (
                submission_extension);
    }

    /* EDS stores this as a client-defined identifier.  Evolution commonly
     * uses folder:// URIs, while CamelStore expects the provider's full
     * folder name.  Try both the configured value and its final path part. */
    if (configured_folder && *configured_folder) {
        folder = camel_store_get_folder_sync (
            store, configured_folder, CAMEL_STORE_FOLDER_NONE,
            cancellable, error);
        if (!folder) {
            g_clear_error (error);
            const gchar *last_separator = strrchr (configured_folder, '/');
            if (last_separator && *(last_separator + 1))
                folder = camel_store_get_folder_sync (
                    store, last_separator + 1, CAMEL_STORE_FOLDER_NONE,
                    cancellable, error);
        }
        if (folder)
            goto finish;
        g_clear_error (error);
    }

    folder_info = camel_store_get_folder_info_sync (
        store, NULL,
        CAMEL_STORE_FOLDER_INFO_RECURSIVE |
        CAMEL_STORE_FOLDER_INFO_NO_VIRTUAL |
        CAMEL_STORE_FOLDER_INFO_REFRESH,
        cancellable, error);
    if (!folder_info)
        goto finish;

    folder_name = find_folder_name_by_type (folder_info, folder_type);
    if (!folder_name) {
        g_set_error_literal (
            error, G_IO_ERROR, G_IO_ERROR_NOT_FOUND,
            drafts
                ? "Für dieses EDS-Konto ist kein Entwurfsordner vorhanden"
                : "Für dieses EDS-Konto ist kein Gesendet-Ordner vorhanden");
        goto finish;
    }

    folder = camel_store_get_folder_sync (
        store, folder_name, CAMEL_STORE_FOLDER_NONE,
        cancellable, error);

finish:
    camel_folder_info_free (folder_info);
    g_free (configured_folder);
    return folder;
}

static void
parse_email_address_parts (const gchar *input_address,
                            gchar **out_name,
                            gchar **out_email)
{
    if (out_name) *out_name = NULL;
    if (out_email) *out_email = NULL;

    if (!input_address || !*input_address)
        return;

    gchar *trimmed = g_strdup (input_address);
    g_strstrip (trimmed);

    gchar *angle_start = strchr (trimmed, '<');
    gchar *angle_end = strrchr (trimmed, '>');

    if (angle_start && angle_end && angle_end > angle_start) {
        *angle_end = '\0';
        gchar *email_part = g_strdup (angle_start + 1);
        g_strstrip (email_part);

        *angle_start = '\0';
        gchar *name_part = g_strdup (trimmed);
        g_strstrip (name_part);
        gsize name_len = strlen (name_part);
        if (name_len >= 2 && name_part[0] == '"' && name_part[name_len - 1] == '"') {
            name_part[name_len - 1] = '\0';
            memmove (name_part, name_part + 1, name_len - 1);
            g_strstrip (name_part);
        }

        if (out_email) *out_email = email_part; else g_free (email_part);
        if (out_name && *name_part) *out_name = name_part; else g_free (name_part);
    } else {
        if (out_email) *out_email = g_strdup (trimmed);
    }

    g_free (trimmed);
}

static gboolean
set_camel_internet_address (CamelInternetAddress *cia,
                             const gchar *provided_name,
                             const gchar *provided_address)
{
    if (!cia || !provided_address || !*provided_address)
        return FALSE;

    gchar *parsed_name = NULL;
    gchar *parsed_email = NULL;

    parse_email_address_parts (provided_address, &parsed_name, &parsed_email);

    const gchar *final_email = (parsed_email && *parsed_email) ? parsed_email : provided_address;
    const gchar *final_name = (provided_name && *provided_name)
        ? provided_name
        : (parsed_name && *parsed_name ? parsed_name : NULL);

    gint index = camel_internet_address_add (cia, final_name, final_email);
    gboolean success = (index >= 0);

    g_free (parsed_name);
    g_free (parsed_email);
    return success;
}

static gboolean
set_message_identity (CamelMimeMessage *message,
                       ESource *identity_source,
                       const gchar *source_uid,
                       GError **error)
{
    ESourceMailIdentity *identity_extension;
    CamelInternetAddress *from = NULL;
    gchar *address = NULL;
    gchar *name = NULL;
    gboolean success = FALSE;

    identity_extension = identity_source
        ? (ESourceMailIdentity *) e_source_get_extension (
            identity_source, E_SOURCE_EXTENSION_MAIL_IDENTITY)
        : NULL;
    address = identity_extension
        ? e_source_mail_identity_dup_address (identity_extension)
        : NULL;
    name = identity_extension
        ? e_source_mail_identity_dup_name (identity_extension)
        : NULL;

    if (!address || !*address) {
        if (identity_source && e_source_has_extension (identity_source, "GNOME Online Accounts")) {
            ESourceExtension *goa = e_source_get_extension (identity_source, "GNOME Online Accounts");
            address = e_source_goa_dup_address (E_SOURCE_GOA (goa));
        }
    }

    if (!address || !*address) {
        g_set_error_literal (
            error, G_IO_ERROR, G_IO_ERROR_INVALID_ARGUMENT,
            "Die EDS-Mail-Identität enthält keine Absenderadresse");
        goto finish;
    }

    from = camel_internet_address_new ();
    if (!set_camel_internet_address (
            from, name && *name ? name : NULL, address)) {
        g_set_error_literal (
            error, G_IO_ERROR, G_IO_ERROR_INVALID_ARGUMENT,
            "Die EDS-Absenderadresse konnte nicht als Internetadresse gelesen werden");
        goto finish;
    }

    camel_medium_remove_header (CAMEL_MEDIUM (message), "From");
    camel_mime_message_set_from (message, from);
    camel_mime_message_set_source (message, source_uid);
    success = TRUE;

finish:
    g_clear_object (&from);
    g_free (address);
    g_free (name);
    return success;
}

static CamelMimeMessage *
parse_raw_message (const guint8 *raw_message,
                   gsize raw_message_len,
                   GCancellable *cancellable,
                   GError **error)
{
    CamelMimeMessage *message;
    CamelStream *stream;

    message = camel_mime_message_new ();
    stream = camel_stream_mem_new_with_buffer (
        (const gchar *) raw_message, raw_message_len);
    if (!camel_data_wrapper_construct_from_stream_sync (
            CAMEL_DATA_WRAPPER (message), stream, cancellable, error)) {
        g_clear_object (&stream);
        g_clear_object (&message);
        return NULL;
    }

    g_clear_object (&stream);
    return message;
}

int
evolution_eds_camel_save_message (
    const char *source_uid,
    const uint8_t *raw_message,
    size_t raw_message_len,
    EvolutionEdsCamelSaveFolder folder,
    char **out_error)
{
    ESourceRegistry *registry = NULL;
    EdsCamelSession *session = NULL;
    ESource *source = NULL;
    ESource *identity_source = NULL;
    CamelStore *store = NULL;
    CamelFolder *target_folder = NULL;
    CamelMimeMessage *message = NULL;
    GCancellable *cancellable = NULL;
    GError *error = NULL;
    ESourceMailAccount *account_extension;
    gchar *identity_uid = NULL;
    gboolean drafts;
    gboolean success = FALSE;
    gchar *appended_uid = NULL;

    if (out_error)
        *out_error = NULL;

    if (!source_uid || !*source_uid || !raw_message || raw_message_len == 0) {
        if (out_error)
            *out_error = g_strdup (
                "EDS-Speicherung benötigt ein Konto und eine RFC822-Nachricht");
        return 0;
    }
    if (folder != EVOLUTION_EDS_CAMEL_SAVE_DRAFT &&
        folder != EVOLUTION_EDS_CAMEL_SAVE_SENT) {
        if (out_error)
            *out_error = g_strdup ("Unbekannter EDS-Mailordner");
        return 0;
    }
    drafts = folder == EVOLUTION_EDS_CAMEL_SAVE_DRAFT;

    camel_provider_init ();
    registry = eds_camel_ref_registry (&error);
    if (!registry)
        goto finish;
    source = e_source_registry_ref_source (registry, source_uid);
    if (!source) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_NOT_FOUND,
            "Das EDS-Mailkonto existiert nicht mehr");
        goto finish;
    }
    if (!e_source_get_enabled (source)) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_FAILED,
            "Das EDS-Mailkonto ist deaktiviert");
        goto finish;
    }

    account_extension = (ESourceMailAccount *) e_source_get_extension (
        source, E_SOURCE_EXTENSION_MAIL_ACCOUNT);
    identity_uid = account_extension
        ? e_source_mail_account_dup_identity_uid (account_extension)
        : NULL;
    if (!identity_uid || !*identity_uid) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_NOT_FOUND,
            "Für das EDS-Mailkonto ist keine Mail-Identität konfiguriert");
        goto finish;
    }
    identity_source = e_source_registry_ref_source (registry, identity_uid);
    if (!identity_source) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_NOT_FOUND,
            "Die Mail-Identität des EDS-Kontos existiert nicht mehr");
        goto finish;
    }

    session = eds_camel_ref_session (registry);
    cancellable = g_cancellable_new ();
    store = open_store_for_source (session, source, cancellable, &error);
    if (!store)
        goto finish;
    message = parse_raw_message (
        raw_message, raw_message_len, cancellable, &error);
    if (!message)
        goto finish;
    if (!set_message_identity (message, identity_source, source_uid, &error))
        goto finish;

    target_folder = open_special_folder (
        store, identity_source, source,
        drafts ? CAMEL_FOLDER_TYPE_DRAFTS : CAMEL_FOLDER_TYPE_SENT,
        drafts, cancellable, &error);
    if (!target_folder)
        goto finish;

    success = camel_folder_append_message_sync (
        target_folder, message, NULL, &appended_uid,
        cancellable, &error);

finish:
    if (!success && out_error && error)
        *out_error = g_strdup (error->message);
    g_clear_error (&error);
    g_free (appended_uid);
    g_clear_object (&target_folder);
    g_clear_object (&message);
    g_clear_object (&store);
    g_clear_object (&cancellable);
    g_clear_object (&session);
    g_clear_object (&identity_source);
    g_clear_object (&source);
    g_clear_pointer (&identity_uid, g_free);
    g_clear_object (&registry);

    return success ? 1 : 0;
}

int
evolution_eds_camel_send_message (
    const char *source_uid,
    const uint8_t *raw_message,
    size_t raw_message_len,
    int *out_sent_message_saved,
    char **out_error)
{
    ESourceRegistry *registry = NULL;
    EdsCamelSession *session = NULL;
    ESource *source = NULL;
    ESource *identity_source = NULL;
    ESource *transport_source = NULL;
    CamelService *transport_service = NULL;
    CamelStore *sent_store = NULL;
    CamelFolder *sent_folder = NULL;
    CamelMimeMessage *message = NULL;
    CamelStream *stream = NULL;
    CamelInternetAddress *from = NULL;
    CamelInternetAddress *recipients = NULL;
    GCancellable *cancellable = NULL;
    GError *error = NULL;
    ESourceMailAccount *account_extension;
    ESourceMailIdentity *identity_extension;
    ESourceMailSubmission *submission_extension;
    ESourceBackend *transport_backend;
    gchar *identity_uid = NULL;
    gchar *transport_uid = NULL;
    gchar *identity_address = NULL;
    gchar *identity_name = NULL;
    const gchar *backend_name;
    gboolean sent_message_saved = FALSE;
    gboolean success = FALSE;

    if (out_error)
        *out_error = NULL;
    if (out_sent_message_saved)
        *out_sent_message_saved = 0;

    if (!source_uid || !*source_uid || !raw_message || raw_message_len == 0) {
        if (out_error)
            *out_error = g_strdup (
                "Versand benötigt ein EDS-Konto und eine RFC822-Nachricht");
        return 0;
    }

    camel_provider_init ();
    registry = e_source_registry_new_sync (NULL, &error);
    if (!registry)
        goto finish;

    source = e_source_registry_ref_source (registry, source_uid);
    if (!source) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_NOT_FOUND,
            "Das ausgewählte EDS-Mailkonto existiert nicht mehr");
        goto finish;
    }
    if (!e_source_get_enabled (source)) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_FAILED,
            "Das ausgewählte EDS-Mailkonto ist deaktiviert");
        goto finish;
    }

    account_extension = (ESourceMailAccount *) e_source_get_extension (
        source, E_SOURCE_EXTENSION_MAIL_ACCOUNT);
    identity_uid = account_extension
        ? e_source_mail_account_dup_identity_uid (account_extension)
        : NULL;
    if (identity_uid && *identity_uid) {
        identity_source = e_source_registry_ref_source (registry, identity_uid);
    }
    if (!identity_source) {
        identity_source = g_object_ref (source);
    }

    identity_extension = (ESourceMailIdentity *) e_source_get_extension (
        identity_source, E_SOURCE_EXTENSION_MAIL_IDENTITY);
    identity_address = identity_extension
        ? e_source_mail_identity_dup_address (identity_extension)
        : NULL;
    identity_name = identity_extension
        ? e_source_mail_identity_dup_name (identity_extension)
        : NULL;

    if (!identity_address || !*identity_address) {
        if (e_source_has_extension (identity_source, "GNOME Online Accounts")) {
            ESourceExtension *goa = e_source_get_extension (identity_source, "GNOME Online Accounts");
            identity_address = e_source_goa_dup_address (E_SOURCE_GOA (goa));
        } else if (e_source_has_extension (source, "GNOME Online Accounts")) {
            ESourceExtension *goa = e_source_get_extension (source, "GNOME Online Accounts");
            identity_address = e_source_goa_dup_address (E_SOURCE_GOA (goa));
        }
    }

    if (!identity_address || !*identity_address) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_INVALID_ARGUMENT,
            "Die ausgewählte EDS-Mail-Identität enthält keine Absenderadresse");
        goto finish;
    }

    submission_extension = (ESourceMailSubmission *) e_source_get_extension (
        identity_source, E_SOURCE_EXTENSION_MAIL_SUBMISSION);
    if (!submission_extension)
        submission_extension = (ESourceMailSubmission *) e_source_get_extension (
            source, E_SOURCE_EXTENSION_MAIL_SUBMISSION);
    transport_uid = submission_extension
        ? e_source_mail_submission_dup_transport_uid (submission_extension)
        : NULL;
    if (!transport_uid || !*transport_uid) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_NOT_FOUND,
            "Für die EDS-Mail-Identität ist kein Mail-Transport konfiguriert");
        goto finish;
    }

    if (g_strcmp0 (transport_uid, source_uid) == 0)
        transport_source = g_object_ref (source);
    else
        transport_source = e_source_registry_ref_source (registry, transport_uid);
    if (!transport_source) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_NOT_FOUND,
            "Der konfigurierte EDS-Mail-Transport existiert nicht mehr");
        goto finish;
    }

    transport_backend = (ESourceBackend *) e_source_get_extension (
        transport_source, E_SOURCE_EXTENSION_MAIL_TRANSPORT);
    backend_name = transport_backend
        ? e_source_backend_get_backend_name (transport_backend)
        : NULL;
    if (!backend_name || !*backend_name) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_NOT_SUPPORTED,
            "Der EDS-Mail-Transport enthält keinen Camel-Backendnamen");
        goto finish;
    }

    session = eds_camel_ref_session (registry);
    cancellable = g_cancellable_new ();
    transport_service = camel_session_ref_service (
        CAMEL_SESSION (session), transport_uid);
    if (!transport_service) {
        transport_service = camel_session_add_service (
            CAMEL_SESSION (session), transport_uid, backend_name,
            CAMEL_PROVIDER_TRANSPORT, &error);
    }
    if (!transport_service)
        goto finish;

    configure_ews_service_credentials_and_settings (session, transport_source, transport_service, backend_name, transport_uid);

    if (!camel_service_get_password (transport_service) && session->registry) {
        ESourceCredentialsProvider *provider = session->credentials_provider
            ? g_object_ref (session->credentials_provider)
            : e_source_credentials_provider_new (session->registry);
        if (provider) {
            ENamedParameters *credentials = NULL;
            if (!e_source_credentials_provider_lookup_sync (provider, transport_source, NULL, &credentials, NULL) || !credentials) {
                const gchar *parent_uid = e_source_get_parent (transport_source);
                if (parent_uid && *parent_uid) {
                    ESource *parent = e_source_registry_ref_source (session->registry, parent_uid);
                    if (parent) {
                        e_source_credentials_provider_lookup_sync (provider, parent, NULL, &credentials, NULL);
                        g_object_unref (parent);
                    }
                }
            }
            if (credentials) {
                const gchar *pwd = e_named_parameters_get (credentials, E_SOURCE_CREDENTIAL_PASSWORD);
                if (pwd && *pwd) {
                    camel_service_set_password (transport_service, pwd);
                    g_printerr ("[EDS] Loaded transport password from CredentialsProvider for %s\n", transport_uid);
                }
                e_named_parameters_free (credentials);
            }
            g_object_unref (provider);
        }
    }

    if (camel_service_get_connection_status (transport_service) != CAMEL_SERVICE_CONNECTED) {
        if (!camel_service_connect_sync (
                transport_service, cancellable, &error))
            goto finish;
    }
    if (!CAMEL_IS_TRANSPORT (transport_service)) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_NOT_SUPPORTED,
            "Der EDS-Mail-Transport ist kein Camel-Transport");
        goto finish;
    }

    message = camel_mime_message_new ();
    stream = camel_stream_mem_new_with_buffer (
        (const gchar *) raw_message, raw_message_len);
    if (!camel_data_wrapper_construct_from_stream_sync (
            CAMEL_DATA_WRAPPER (message), stream, cancellable, &error))
        goto finish;

    from = camel_internet_address_new ();
    if (!set_camel_internet_address (
            from,
            identity_name && *identity_name ? identity_name : NULL,
            identity_address)) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_INVALID_ARGUMENT,
            "Die EDS-Absenderadresse konnte nicht als Internetadresse gelesen werden");
        goto finish;
    }
    camel_medium_remove_header (CAMEL_MEDIUM (message), "From");
    camel_mime_message_set_from (message, from);
    camel_mime_message_set_source (message, source_uid);

    recipients = camel_internet_address_new ();
    CamelInternetAddress *part = camel_mime_message_get_recipients (
        message, CAMEL_RECIPIENT_TYPE_TO);
    if (part)
        camel_address_cat (CAMEL_ADDRESS (recipients), CAMEL_ADDRESS (part));
    part = camel_mime_message_get_recipients (
        message, CAMEL_RECIPIENT_TYPE_CC);
    if (part)
        camel_address_cat (CAMEL_ADDRESS (recipients), CAMEL_ADDRESS (part));
    part = camel_mime_message_get_recipients (
        message, CAMEL_RECIPIENT_TYPE_BCC);
    if (part)
        camel_address_cat (CAMEL_ADDRESS (recipients), CAMEL_ADDRESS (part));
    if (camel_address_length (CAMEL_ADDRESS (recipients)) == 0) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_INVALID_ARGUMENT,
            "Die RFC822-Nachricht enthält keine gültigen Empfänger");
        goto finish;
    }

    /* Camel converts 8-bit parts to a transport-safe representation when
     * necessary; this also keeps locally generated UTF-8 HTML valid for SMTP
     * providers that do not advertise 8BITMIME. */
    camel_mime_message_encode_8bit_parts (message);
    success = camel_transport_send_to_sync (
        CAMEL_TRANSPORT (transport_service), message,
        CAMEL_ADDRESS (from), CAMEL_ADDRESS (recipients),
        &sent_message_saved, cancellable, &error);

    /* Some Camel transports report a successful SMTP submission without
     * storing a Sent copy.  Honor the EDS submission setting and append the
     * exact transport-ready message when Camel did not do so itself.  A Sent
     * copy failure must remain a visible status detail, not turn a message
     * which was already accepted by SMTP into a false send failure. */
    if (success && !sent_message_saved &&
        (!submission_extension ||
         e_source_mail_submission_get_use_sent_folder (submission_extension))) {
        GError *sent_error = NULL;

        sent_store = open_store_for_source (
            session, source, cancellable, &sent_error);
        if (sent_store)
            sent_folder = open_special_folder (
                sent_store, identity_source, source,
                CAMEL_FOLDER_TYPE_SENT, FALSE,
                cancellable, &sent_error);
        if (sent_folder) {
            gchar *appended_uid = NULL;

            sent_message_saved = camel_folder_append_message_sync (
                sent_folder, message, NULL, &appended_uid,
                cancellable, &sent_error);
            g_free (appended_uid);
        }
        if (!sent_message_saved && sent_error) {
            g_warning ("EDS Sent-Kopie konnte nicht gespeichert werden: %s",
                       sent_error->message);
        }
        g_clear_error (&sent_error);
    }

finish:
    if (!success && out_error && error)
        *out_error = g_strdup (error->message);
    if (out_sent_message_saved)
        *out_sent_message_saved = sent_message_saved ? 1 : 0;
    g_clear_error (&error);
    g_clear_pointer (&identity_uid, g_free);
    g_clear_pointer (&transport_uid, g_free);
    g_clear_pointer (&identity_address, g_free);
    g_clear_pointer (&identity_name, g_free);
    g_clear_object (&recipients);
    g_clear_object (&from);
    g_clear_object (&stream);
    g_clear_object (&message);
    g_clear_object (&sent_folder);
    g_clear_object (&sent_store);
    g_clear_object (&transport_service);
    g_clear_object (&transport_source);
    g_clear_object (&identity_source);
    g_clear_object (&cancellable);
    g_clear_object (&session);
    g_clear_object (&source);
    g_clear_object (&registry);

    return success ? 1 : 0;
}

void
evolution_eds_camel_free_error (char *error_message)
{
    g_free (error_message);
}

int
evolution_eds_camel_store_password (const char *source_uid,
                                    const char *password,
                                    char **out_error)
{
    if (!source_uid || !*source_uid || !password) {
        if (out_error)
            *out_error = g_strdup ("Ungültige Quell-UID oder leeres Passwort");
        return 0;
    }

    GError *err = NULL;
    ESourceRegistry *registry = eds_camel_ref_registry (&err);
    if (!registry) {
        if (out_error)
            *out_error = g_strdup (err ? err->message : "Verbindung zum EDS Source Registry fehlgeschlagen");
        if (err)
            g_clear_error (&err);
        return 0;
    }

    EdsCamelSession *session = eds_camel_ref_session (registry);
    ESourceCredentialsProvider *provider = session && session->credentials_provider
        ? g_object_ref (session->credentials_provider)
        : e_source_credentials_provider_new (registry);

    ESource *source = e_source_registry_ref_source (registry, source_uid);
    if (!source) {
        if (out_error)
            *out_error = g_strdup_printf ("Quelle '%s' nicht in EDS gefunden", source_uid);
        g_clear_object (&provider);
        g_clear_object (&session);
        g_clear_object (&registry);
        return 0;
    }

    ENamedParameters *credentials = e_named_parameters_new ();
    e_named_parameters_set (credentials, E_SOURCE_CREDENTIAL_PASSWORD, password);

    gboolean ok = e_source_credentials_provider_store_sync (
        provider, source, credentials, TRUE, NULL, &err);

    e_named_parameters_free (credentials);
    g_clear_object (&source);
    g_clear_object (&provider);
    g_clear_object (&session);
    g_clear_object (&registry);

    if (!ok) {
        if (out_error)
            *out_error = g_strdup (err ? err->message : "Passwort konnte nicht im Schlüsselbund hinterlegt werden");
        if (err)
            g_clear_error (&err);
        return 0;
    }

    g_printerr ("[EDS] Password successfully stored via credentials provider for %s\n", source_uid);
    return 1;
}

int
evolution_eds_camel_search_folder (
    const char *source_uid,
    const char *folder_name,
    const char *query_sexpr,
    char ***out_uids,
    size_t *out_count,
    char **out_error,
    EvolutionEdsCamelCancellationCallback cancellation_callback,
    void *cancellation_user_data)
{
    ESourceRegistry *registry = NULL;
    EdsCamelSession *session = NULL;
    ESource *source = NULL;
    CamelStore *store = NULL;
    CamelFolder *folder = NULL;
    GCancellable *cancellable = NULL;
    CancellationMonitor *cancellation_monitor = NULL;
    GPtrArray *uids = NULL;
    GError *error = NULL;
    gboolean success = FALSE;

    if (out_uids)
        *out_uids = NULL;
    if (out_count)
        *out_count = 0;
    if (out_error)
        *out_error = NULL;

    if (!source_uid || !*source_uid || !folder_name || !*folder_name) {
        if (out_error)
            *out_error = g_strdup ("Suche benötigt Konto-UID und Ordnernamen");
        return 0;
    }

    camel_provider_init ();

    registry = eds_camel_ref_registry (&error);
    if (!registry)
        goto finish;

    source = e_source_registry_ref_source (registry, source_uid);
    if (!source) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_NOT_FOUND,
            "EDS source for Camel search no longer exists");
        goto finish;
    }
    if (!e_source_get_enabled (source)) {
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_FAILED,
            "EDS source for Camel search is disabled");
        goto finish;
    }

    session = eds_camel_ref_session (registry);
    cancellable = g_cancellable_new ();
    cancellation_monitor = cancellation_monitor_start (
        cancellable, cancellation_callback, cancellation_user_data);

    store = open_store_for_source (session, source, cancellable, &error);
    if (!store)
        goto finish;

    folder = camel_store_get_folder_sync (
        store, folder_name, CAMEL_STORE_FOLDER_NONE,
        cancellable, &error);
    if (!folder)
        goto finish;

    CamelFolderSummary *summary = camel_folder_get_folder_summary (folder);
    if (summary) {
        camel_folder_summary_prepare_fetch_all (summary, NULL);
    }

    const gchar *sexpr = (query_sexpr && *query_sexpr) ? query_sexpr : "(match-all)";
    success = camel_folder_search_sync (folder, sexpr, &uids, cancellable, &error);

finish:
    if (cancellation_monitor)
        cancellation_monitor_free (cancellation_monitor);

    if (success && uids) {
        char **result_uids = g_new0 (char *, uids->len + 1);
        for (guint i = 0; i < uids->len; i++) {
            result_uids[i] = g_strdup (g_ptr_array_index (uids, i));
        }
        if (out_uids)
            *out_uids = result_uids;
        if (out_count)
            *out_count = uids->len;
        g_ptr_array_unref (uids);
    } else if (success) {
        if (out_uids)
            *out_uids = g_new0 (char *, 1);
        if (out_count)
            *out_count = 0;
    } else {
        if (out_error && error)
            *out_error = g_strdup (error->message);
    }

    g_clear_error (&error);
    g_clear_object (&folder);
    g_clear_object (&store);
    g_clear_object (&cancellable);
    g_clear_object (&session);
    g_clear_object (&source);
    g_clear_object (&registry);

    return success ? 1 : 0;
}

void
evolution_eds_camel_free_uids (char **uids, size_t count)
{
    if (!uids)
        return;
    for (size_t i = 0; i < count; i++) {
        g_free (uids[i]);
    }
    g_free (uids);
}

