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
    gint message_offset;
    gint max_messages_per_folder;
    gchar *first_error;
} LoadContext;

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
               CamelFolderInfo *folder_info)
{
    EvolutionEdsCamelFolder *record;

    if (!is_mail_folder (folder_info) ||
        !folder_info->full_name || !*folder_info->full_name)
        return;

    ensure_folder_capacity (context);
    record = &context->folders[context->folder_count++];
    record->source_uid = g_strdup (source_uid);
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

    g_object_unref (output);
    return TRUE;
}

static void
load_folder (LoadContext *context,
             CamelStore *store,
             const gchar *source_uid,
             CamelFolderInfo *folder_info,
             GCancellable *cancellable)
{
    CamelFolderInfo *current;

    for (current = folder_info; current != NULL; current = current->next) {
        CamelFolder *folder = NULL;
        GPtrArray *uids = NULL;
        GError *error = NULL;

        append_folder (context, source_uid, current);

        if (is_mail_folder (current) &&
            (current->flags & CAMEL_FOLDER_NOSELECT) == 0) {
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
                context, store, source_uid, current->child, cancellable);
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

    store = open_store_for_source (session, source, cancellable, &error);
    if (!store) {
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
        gchar *scope = g_strdup_printf (
            "%s: Ordner konnten nicht geladen werden",
            e_source_get_display_name (source));
        remember_error (context, scope, error);
        g_free (scope);
        g_clear_error (&error);
    } else {
        load_folder (
            context, store, source_uid,
            folder_info, cancellable);
        camel_folder_info_free (folder_info);
        success = TRUE;
    }

    g_object_unref (store);
    return success;
}

int
evolution_eds_camel_load_messages (const char *source_uid,
                                   int message_offset,
                                   int max_messages_per_folder,
                                   EvolutionEdsCamelMessage **out_messages,
                                   size_t *out_count,
                                   EvolutionEdsCamelFolder **out_folders,
                                   size_t *out_folder_count,
                                   size_t *out_configured_sources,
                                   char **out_error)
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
    if (out_error)
        *out_error = NULL;

    context.message_offset = message_offset > 0 ? message_offset : 0;
    context.max_messages_per_folder = max_messages_per_folder > 0
        ? max_messages_per_folder
        : DEFAULT_MESSAGES_PER_FOLDER;

    camel_provider_init ();

    registry = e_source_registry_new_sync (NULL, &error);
    if (!registry) {
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

        load_source ( &context, session, source, cancellable);
    }

    if (out_configured_sources)
        *out_configured_sources = context.configured_sources;

    if (context.count > 0 || context.folder_count > 0) {
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
    } else if (context.configured_sources > 0 || context.first_error) {
        result = 0;
    } else {
        /* No enabled remote EDS mail account is a normal state; the Rust
         * caller will use its local Evolution mailbox fallback. */
        result = 1;
    }

    if (out_error && context.first_error)
        *out_error = g_strdup (context.first_error);

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
        g_free (folders[ii].folder_name);
        g_free (folders[ii].folder_id);
        g_free (folders[ii].display_name);
    }

    g_free (folders);
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
        break;
    }
    case EVOLUTION_EDS_CAMEL_SET_IMPORTANT:
        success = synchronize_message_flags (
            folder, operation->message_uid,
            CAMEL_MESSAGE_FLAGGED,
            operation->is_important ? CAMEL_MESSAGE_FLAGGED : 0,
            cancellable, &error);
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

    return success ? 1 : 0;
}

void
evolution_eds_camel_free_error (char *error_message)
{
    g_free (error_message);
}
