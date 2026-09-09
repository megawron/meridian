/* SPDX-License-Identifier: GPL-3.0-or-later */
/* Runs the standalone traversal helper (not yet wired into the bridge) with deterministic Camel call doubles.
 * This verifies orchestration, not EDS ABI or provider behavior. */
#include <gio/gio.h>
#include <string.h>

typedef GObject CamelFolder;
typedef struct { guint transfers, fail_at, creates, refreshes; } CamelStore;
typedef struct _CamelFolderInfo {
    const gchar *full_name;
    guint flags;
    struct _CamelFolderInfo *child, *next;
} CamelFolderInfo;
enum { CAMEL_FOLDER_NOSELECT = 1, CAMEL_FOLDER_VIRTUAL = 2,
       CAMEL_FOLDER_VTRASH = 4, CAMEL_FOLDER_READONLY = 8, CAMEL_FOLDER_SYSTEM = 16,
       CAMEL_STORE_FOLDER_INFO_FAST = 0,
       CAMEL_STORE_FOLDER_NONE = 0, CAMEL_STORE_FOLDER_CREATE = 1 };
#define CAMEL_IS_SUBSCRIBABLE(store) FALSE
#define CAMEL_SUBSCRIBABLE(store) (store)
static gboolean is_mail_folder (CamelFolderInfo *info) { return info != NULL; }
static CamelFolderInfo *camel_store_get_folder_info_sync (
    CamelStore *store, const gchar *name, gint flags, GCancellable *cancel, GError **error)
{ (void) store; (void) name; (void) flags; (void) cancel; (void) error; return NULL; }
static CamelFolderInfo *camel_store_create_folder_sync (
    CamelStore *store, const gchar *parent, const gchar *leaf, GCancellable *cancel, GError **error)
{
    (void) parent; (void) leaf; (void) cancel; (void) error;
    store->creates++;
    return g_new0 (CamelFolderInfo, 1);
}
static void camel_folder_info_free (CamelFolderInfo *info) { g_free (info); }
static CamelFolder *camel_store_get_folder_sync (
    CamelStore *store, const gchar *name, gint flags, GCancellable *cancel, GError **error)
{
    (void) cancel; (void) error;
    CamelFolder *folder = g_object_new (G_TYPE_OBJECT, NULL);
    g_object_set_data (folder, "store", store);
    g_object_set_data_full (folder, "name", g_strdup (name), g_free);
    if (flags == CAMEL_STORE_FOLDER_CREATE)
        store->creates++;
    return folder;
}
static gboolean camel_folder_refresh_info_sync (CamelFolder *folder, GCancellable *cancel, GError **error)
{
    (void) cancel; (void) error;
    ((CamelStore *) g_object_get_data (folder, "store"))->refreshes++;
    return TRUE;
}
static GPtrArray *camel_folder_dup_uids (CamelFolder *folder)
{
    (void) folder;
    GPtrArray *uids = g_ptr_array_new ();
    g_ptr_array_add (uids, (gpointer) "uid-1");
    g_ptr_array_add (uids, (gpointer) "uid-2");
    return uids;
}
static gboolean camel_folder_transfer_messages_to_sync (
    CamelFolder *from, GPtrArray *uids, CamelFolder *to, gboolean delete_originals,
    GPtrArray **new_uids, GCancellable *cancel, GError **error)
{
    (void) uids; (void) new_uids; (void) cancel;
    CamelStore *store = g_object_get_data (from, "store");
    g_assert_false (delete_originals);
    gchar *expected = g_strconcat ("Archive/", g_object_get_data (from, "name"), NULL);
    g_assert_cmpstr (expected, ==, g_object_get_data (to, "name"));
    g_free (expected);
    store->transfers++;
    if (store->fail_at == store->transfers) {
        g_set_error_literal (error, G_IO_ERROR, G_IO_ERROR_FAILED, "partial provider failure");
        return FALSE;
    }
    return TRUE;
}
static gboolean camel_subscribable_folder_is_subscribed (CamelStore *store, const gchar *name)
{ (void) store; (void) name; return TRUE; }
static gboolean camel_subscribable_subscribe_folder_sync (
    CamelStore *store, const gchar *name, GCancellable *cancel, GError **error)
{ (void) store; (void) name; (void) cancel; (void) error; return TRUE; }

static gboolean camel_folder_synchronize_sync (
    CamelFolder *folder, gboolean expunge, GCancellable *cancel, GError **error)
{ (void) folder; (void) expunge; (void) cancel; (void) error; g_assert_not_reached (); return FALSE; }
static gboolean camel_subscribable_unsubscribe_folder_sync (
    CamelStore *store, const gchar *name, GCancellable *cancel, GError **error)
{ (void) store; (void) name; (void) cancel; (void) error; g_assert_not_reached (); return FALSE; }
static gboolean camel_store_delete_folder_sync (
    CamelStore *store, const gchar *name, GCancellable *cancel, GError **error)
{ (void) store; (void) name; (void) cancel; (void) error; g_assert_not_reached (); return FALSE; }

#include "../eds_camel_folder_copy.h"

static void run_copy_case (guint fail_at, gboolean cancel_first, guint child_flags)
{
    CamelFolderInfo child = { "Projects/Child", child_flags, NULL, NULL };
    CamelFolderInfo unrelated = { "Projects-other", 0, NULL, NULL };
    CamelFolderInfo root = { "Projects", 0, &child, &unrelated };
    CamelStore store = { .fail_at = fail_at };
    GCancellable *cancel = g_cancellable_new ();
    GError *error = NULL;
    gboolean started = FALSE;
    guint folders = 0, removed_folders = 0;
    guint64 messages = 0;
    if (cancel_first)
        g_cancellable_cancel (cancel);
    gboolean success = copy_folder_tree_sync (&store, &store, &root, "Archive/Projects",
        FALSE, cancel, &started, &folders, &messages, &removed_folders, &error);
    g_assert_cmpuint (removed_folders, ==, 0);
    if (cancel_first) {
        g_assert_false (success);
        g_assert_false (started);
        g_assert_cmpuint (store.creates, ==, 0);
        g_assert_cmpuint (folders, ==, 0);
        g_assert_error (error, G_IO_ERROR, G_IO_ERROR_CANCELLED);
    } else if (fail_at) {
        g_assert_false (success);
        g_assert_true (started);
        g_assert_cmpuint (folders, ==, fail_at - 1);
        g_assert_cmpuint (messages, ==, (fail_at - 1) * 2);
        g_assert_error (error, G_IO_ERROR, G_IO_ERROR_FAILED);
    } else {
        g_assert_true (success);
        g_assert_no_error (error);
        g_assert_true (started);
        g_assert_cmpuint (folders, ==, 2);
        g_assert_cmpuint (messages, ==, child_flags ? 2 : 4);
        g_assert_cmpuint (store.creates, ==, 2);
    }
    g_clear_error (&error);
    g_object_unref (cancel);
}

int main (void)
{
    run_copy_case (0, FALSE, 0);
    run_copy_case (1, FALSE, 0);
    run_copy_case (2, FALSE, 0);
    run_copy_case (0, TRUE, 0);
    run_copy_case (0, FALSE, CAMEL_FOLDER_NOSELECT);
    g_print ("folder copy: 5 standalone traversal cases passed\n");
    return 0;
}
