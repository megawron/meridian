/* SPDX-License-Identifier: GPL-3.0-or-later */

#include "eds_camel_bridge.h"

#include <camel/camel.h>
#include <gio/gio.h>
#include <glib.h>

/* Keep the bridge's C dependency surface small.  EDS marks its public
 * headers as include-through-libedataserver.h, so define the same umbrella
 * marker while including only the source/registry APIs used here. */
#define __LIBEDATASERVER_H_INSIDE__
#include <libedataserver/e-source.h>
#include <libedataserver/e-source-backend.h>
#include <libedataserver/e-source-camel.h>
#include <libedataserver/e-source-credentials-provider.h>
#include <libedataserver/e-source-mail-account.h>
#include <libedataserver/e-source-registry.h>
#undef __LIBEDATASERVER_H_INSIDE__

#include <string.h>

#define DEFAULT_MESSAGES_PER_FOLDER 100

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

    if (!session->credentials_provider) {
        g_set_error_literal (
            error, G_IO_ERROR, G_IO_ERROR_FAILED,
            "EDS credential provider is not available");
        g_object_unref (source);
        return NULL;
    }

    if (!e_source_credentials_provider_lookup_sync (
            session->credentials_provider, source, NULL,
            &credentials, &local_error)) {
        if (error && local_error)
            *error = local_error;
        else
            g_clear_error (&local_error);
    } else if (credentials) {
        const gchar *stored_password = e_named_parameters_get (
            credentials, E_SOURCE_CREDENTIAL_PASSWORD);

        /* GOA OAuth2 accounts intentionally return an empty credential set.
         * Camel then uses get_oauth2_access_token_sync() for XOAUTH2. */
        password = g_strdup (stored_password);
    }

    if (credentials)
        e_named_parameters_free (credentials);
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
    gboolean success;

    source = eds_camel_session_ref_source (session, service);
    if (!source) {
        g_set_error_literal (
            error, G_IO_ERROR, G_IO_ERROR_NOT_FOUND,
            "No EDS source exists for the Camel service");
        return FALSE;
    }

    success = e_source_get_oauth2_access_token_sync (
        source, cancellable, out_access_token, out_expires_in, error);

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

    (void) base_session;

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

static EdsCamelSession *
eds_camel_session_new (ESourceRegistry *registry)
{
    EdsCamelSession *session;
    gchar *data_dir;
    gchar *cache_dir;

    data_dir = g_build_filename (
        g_get_user_data_dir (), "evolution-gtk4", "camel", NULL);
    cache_dir = g_build_filename (
        g_get_user_cache_dir (), "evolution-gtk4", "camel", NULL);

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
    size_t folders_seen;
    size_t messages_loaded;
    size_t processed_sources;
} LoadContext;

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

    if (message_info) {
        guint32 flags = camel_message_info_get_flags (message_info);

        record->is_unread = (flags & CAMEL_MESSAGE_SEEN) == 0;
        record->is_flagged = (flags & CAMEL_MESSAGE_FLAGGED) != 0;
    }

    context->messages_loaded++;
    g_object_unref (output);
    return TRUE;
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

        append_folder (
            context, source_uid, source_display_name, current);

        if (is_mail_folder (current)) {
            context->folders_seen++;
            emit_progress (
                context, EVOLUTION_EDS_CAMEL_SYNC_LOADING_MESSAGES,
                source_uid, source_display_name, current->full_name);
        }

        if (is_mail_folder (current) &&
            (current->flags & CAMEL_FOLDER_NOSELECT) == 0 &&
            (!folder_name ||
             (current->full_name &&
              g_str_equal (current->full_name, folder_name)))) {
            folder = camel_store_get_folder_sync (
                store, current->full_name, CAMEL_STORE_FOLDER_NONE,
                cancellable, &error);
            if (!folder) {
                remember_error (context, current->full_name, error);
                g_clear_error (&error);
            } else {
                if (!camel_folder_refresh_info_sync (
                        folder, cancellable, &error)) {
                    remember_error (context, current->full_name, error);
                    g_clear_error (&error);
                }

                #if EDS_BRIDGE_HAS_DUP_UIDS
                uids = camel_folder_dup_uids (folder);
                #else
                uids = camel_folder_get_uids (folder);
                #endif
                if (uids) {
                    gsize start = uids->len;
                    gsize skipped = 0;
                    gsize loaded = 0;

                    while (start > 0 &&
                           skipped < (gsize) context->message_offset) {
                        start--;
                        skipped++;
                    }

                    while (start > 0 &&
                           loaded < (gsize) context->max_messages_per_folder) {
                        const gchar *message_uid;
                        CamelMessageInfo *message_info;
                        CamelMimeMessage *message;

                        start--;
                        message_uid = g_ptr_array_index (uids, start);
                        if (!message_uid)
                            continue;

                        message_info = camel_folder_get_message_info (
                            folder, message_uid);
                        message = camel_folder_get_message_sync (
                            folder, message_uid, cancellable, &error);
                        if (!message) {
                            remember_error (context, current->full_name, error);
                            g_clear_error (&error);
                            if (message_info)
                                g_object_unref (message_info);
                            continue;
                        }

                        if (append_message (
                                context, source_uid, current, message_uid,
                                message_info, message, cancellable))
                            loaded++;

                        g_object_unref (message);
                        if (message_info)
                            g_object_unref (message_info);
                    }

                    #if EDS_BRIDGE_HAS_DUP_UIDS
                    g_ptr_array_unref (uids);
                    #else
                    camel_folder_free_uids (folder, uids);
                    #endif
                }

                g_object_unref (folder);
            }
        }

        if (current->child)
            load_folder (
                context, store, source_uid, source_display_name,
                folder_name, current->child, cancellable);
    }
}

static CamelStore *
open_store_for_source (EdsCamelSession *session,
                       ESource *source,
                       GCancellable *cancellable,
                       GError **error)
{
    ESourceBackend *backend;
    const gchar *source_uid;
    const gchar *backend_name;
    CamelService *service;

    source_uid = e_source_get_uid (source);
    backend = E_SOURCE_BACKEND (
        e_source_get_extension (source, E_SOURCE_EXTENSION_MAIL_ACCOUNT));
    backend_name = backend ? e_source_backend_get_backend_name (backend) : NULL;

    if (!source_uid || !backend_name || !*backend_name) {
        g_set_error_literal (
            error, G_IO_ERROR, G_IO_ERROR_NOT_SUPPORTED,
            "EDS mail source has no Camel backend");
        return NULL;
    }

    service = camel_session_add_service (
        CAMEL_SESSION (session), source_uid, backend_name,
        CAMEL_PROVIDER_STORE, error);
    if (!service)
        return NULL;

    e_source_camel_configure_service (source, service);

    if (!camel_service_connect_sync (service, cancellable, error)) {
        g_object_unref (service);
        return NULL;
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

    source_uid = e_source_get_uid (source);
    backend = E_SOURCE_BACKEND (
        e_source_get_extension (source, E_SOURCE_EXTENSION_MAIL_ACCOUNT));
    backend_name = backend ? e_source_backend_get_backend_name (backend) : NULL;

    if (!source_uid || !backend_name || !*backend_name)
        return FALSE;

    /* The local provider is already handled by the read-only local parser;
     * network sources are opened through the original EDS/Camel provider. */
    if (g_str_equal (backend_name, "maildir") ||
        g_str_equal (backend_name, "mbox") ||
        g_str_equal (backend_name, "mh") ||
        g_str_equal (backend_name, "spool") ||
        g_str_equal (backend_name, "vfolder"))
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

    folder_info = camel_store_get_folder_info_sync (
        store, NULL,
        CAMEL_STORE_FOLDER_INFO_RECURSIVE |
        CAMEL_STORE_FOLDER_INFO_NO_VIRTUAL |
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
                                   void *progress_user_data)
{
    ESourceRegistry *registry = NULL;
    EdsCamelSession *session = NULL;
    GList *sources = NULL;
    GList *link;
    GCancellable *cancellable = NULL;
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
    context.max_messages_per_folder = max_messages_per_folder > 0
        ? max_messages_per_folder
        : DEFAULT_MESSAGES_PER_FOLDER;
    context.progress_callback = progress_callback;
    context.progress_user_data = progress_user_data;

    emit_progress (
        &context, EVOLUTION_EDS_CAMEL_SYNC_DISCOVERING, NULL, NULL, NULL);

    camel_provider_init ();

    registry = e_source_registry_new_sync (NULL, &error);
    if (!registry) {
        emit_progress (
            &context, EVOLUTION_EDS_CAMEL_SYNC_FAILED, NULL, NULL, NULL);
        if (out_error && error)
            *out_error = g_strdup (error->message);
        g_clear_error (&error);
        return 0;
    }

    session = eds_camel_session_new (registry);
    cancellable = g_cancellable_new ();
    sources = e_source_registry_list_enabled (
        registry, E_SOURCE_EXTENSION_MAIL_ACCOUNT);

    for (link = sources; link != NULL; link = link->next) {
        ESource *source = E_SOURCE (link->data);

        if (source_uid &&
            !g_str_equal (source_uid, e_source_get_uid (source)))
            continue;

        load_source (&context, session, source, folder_name, cancellable);
    }

    if (out_configured_sources)
        *out_configured_sources = context.configured_sources;
    if (out_successful_sources)
        *out_successful_sources = context.successful_sources;
    if (out_failed_sources)
        *out_failed_sources = context.failed_sources;

    if (context.successful_sources > 0 || context.configured_sources == 0) {
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
        context.failed_sources > 0 && context.successful_sources == 0
            ? EVOLUTION_EDS_CAMEL_SYNC_FAILED
            : EVOLUTION_EDS_CAMEL_SYNC_COMPLETE,
        NULL, NULL, NULL);

    g_free (context.first_error);
    evolution_eds_camel_free_messages (context.messages, context.count);
    evolution_eds_camel_free_folders (context.folders, context.folder_count);
    g_clear_object (&cancellable);
    g_list_free_full (sources, g_object_unref);
    g_clear_object (&session);
    g_clear_object (&registry);

    return result;
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
};

static void
free_watcher_event (CamelWatcherEvent *queued_event)
{
    if (!queued_event)
        return;

    g_free (queued_event->event.source_uid);
    g_free (queued_event->event.folder_name);
    g_free (queued_event->event.old_folder_name);
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

    watcher->registry = e_source_registry_new_sync (NULL, &error);
    if (!watcher->registry) {
        if (out_error && error)
            *out_error = g_strdup (error->message);
        g_clear_error (&error);
        evolution_eds_camel_watcher_free (watcher);
        return 0;
    }

    watcher->session = eds_camel_session_new (watcher->registry);
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
            CAMEL_STORE_FOLDER_INFO_NO_VIRTUAL |
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

    g_ptr_array_set_size (watcher->folders, 0);
    g_ptr_array_set_size (watcher->stores, 0);
    g_clear_object (&watcher->session);
    g_clear_object (&watcher->registry);

    while (!g_queue_is_empty (watcher->events))
        free_watcher_event (g_queue_pop_head (watcher->events));
    g_queue_free (watcher->events);
    g_ptr_array_unref (watcher->folders);
    g_ptr_array_unref (watcher->stores);
    g_cond_clear (&watcher->condition);
    g_mutex_clear (&watcher->lock);
    g_free (watcher);
}

static const gchar *
find_folder_name_by_type (CamelFolderInfo *folder_info,
                          CamelFolderInfoFlags type)
{
    CamelFolderInfo *current;

    for (current = folder_info; current != NULL; current = current->next) {
        if (is_mail_folder (current) &&
            (current->flags & CAMEL_FOLDER_TYPE_MASK) == type &&
            (current->flags & CAMEL_FOLDER_NOSELECT) == 0 &&
            current->full_name && *current->full_name)
            return current->full_name;

        if (current->child) {
            const gchar *child_name = find_folder_name_by_type (
                current->child, type);
            if (child_name)
                return child_name;
        }
    }

    return NULL;
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
    char **out_error)
{
    ESourceRegistry *registry = NULL;
    EdsCamelSession *session = NULL;
    ESource *source = NULL;
    CamelStore *store = NULL;
    CamelFolder *folder = NULL;
    CamelFolder *destination = NULL;
    CamelFolder *trash = NULL;
    CamelFolderInfo *folder_info = NULL;
    GCancellable *cancellable = NULL;
    GError *error = NULL;
    const gchar *destination_name = NULL;
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

    camel_provider_init ();
    registry = e_source_registry_new_sync (NULL, &error);
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

    session = eds_camel_session_new (registry);
    cancellable = g_cancellable_new ();
    store = open_store_for_source (session, source, cancellable, &error);
    if (!store)
        goto finish;

    folder = camel_store_get_folder_sync (
        store, operation->folder_name, CAMEL_STORE_FOLDER_NONE,
        cancellable, &error);
    if (!folder)
        goto finish;

    if (!message_exists (folder, operation->message_uid)) {
        set_conflict_error (
            &error, "Die Nachricht existiert im Quellordner nicht mehr");
        conflict = TRUE;
        goto finish;
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
            folder, operation->message_uid, mask, set,
            cancellable, &error);
        if (success)
            success = verify_message_flags (
                folder, operation->message_uid, mask, set, &error);
        break;
    }
    case EVOLUTION_EDS_CAMEL_SET_JUNK: {
        guint32 set = operation->is_junk
            ? CAMEL_MESSAGE_JUNK
            : CAMEL_MESSAGE_NOTJUNK;

        success = synchronize_message_flags (
            folder, operation->message_uid,
            CAMEL_MESSAGE_JUNK | CAMEL_MESSAGE_NOTJUNK,
            set, cancellable, &error);
        if (success)
            success = verify_message_flags (
                folder, operation->message_uid,
                CAMEL_MESSAGE_JUNK | CAMEL_MESSAGE_NOTJUNK,
                set, &error);
        break;
    }
    case EVOLUTION_EDS_CAMEL_SET_IMPORTANT:
        success = synchronize_message_flags (
            folder, operation->message_uid,
            CAMEL_MESSAGE_FLAGGED,
            operation->is_important ? CAMEL_MESSAGE_FLAGGED : 0,
            cancellable, &error);
        if (success)
            success = verify_message_flags (
                folder, operation->message_uid,
                CAMEL_MESSAGE_FLAGGED,
                operation->is_important ? CAMEL_MESSAGE_FLAGGED : 0,
                &error);
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
            folder, destination, operation->message_uid, TRUE,
            cancellable, &error);
        if (success)
            success = verify_message_moved (
                folder, operation->message_uid, &error);
        break;
    case EVOLUTION_EDS_CAMEL_DELETE_MESSAGE:
        trash = camel_store_get_trash_folder_sync (
            store, cancellable, &error);
        if (trash) {
            const gchar *folder_name = camel_folder_get_full_name (folder);
            const gchar *trash_name = camel_folder_get_full_name (trash);

            if (g_strcmp0 (folder_name, trash_name) != 0) {
                success = transfer_one_message (
                    folder, trash, operation->message_uid, TRUE,
                    cancellable, &error);
                if (success)
                    success = verify_message_moved (
                        folder, operation->message_uid, &error);
                break;
            }
        } else {
            /* Some providers have no trash folder; fall back to a real
             * deleted flag and expunge rather than reporting a false success. */
            g_clear_error (&error);
        }

        if (camel_folder_set_message_flags (
                folder, operation->message_uid,
                CAMEL_MESSAGE_DELETED | CAMEL_MESSAGE_SEEN,
                CAMEL_MESSAGE_DELETED | CAMEL_MESSAGE_SEEN)) {
            success = camel_folder_synchronize_sync (
                folder, TRUE, cancellable, &error);
            if (success)
                success = verify_message_moved (
                    folder, operation->message_uid, &error);
        } else {
            g_set_error_literal (
                &error, G_IO_ERROR, G_IO_ERROR_FAILED,
                "Camel refused to delete the message");
        }
        break;
    default:
        g_set_error_literal (
            &error, G_IO_ERROR, G_IO_ERROR_NOT_SUPPORTED,
            "Unsupported EDS/Camel mail operation");
        break;
    }

finish:
    conflict = conflict || (error && error->message &&
        g_str_has_prefix (error->message, "conflict:"));
    if (!success && out_error && error)
        *out_error = g_strdup (error->message);
    g_clear_error (&error);
    camel_folder_info_free (folder_info);
    g_clear_object (&trash);
    g_clear_object (&destination);
    g_clear_object (&folder);
    g_clear_object (&store);
    g_clear_object (&cancellable);
    g_clear_object (&session);
    g_clear_object (&source);
    g_clear_object (&registry);

    return success ? 1 : (conflict ? 2 : 0);
}

void
evolution_eds_camel_free_error (char *error_message)
{
    g_free (error_message);
}
