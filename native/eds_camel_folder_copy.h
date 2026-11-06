/* SPDX-License-Identifier: GPL-3.0-or-later */
#ifndef EVOLUTION_EDS_CAMEL_FOLDER_COPY_H
#define EVOLUTION_EDS_CAMEL_FOLDER_COPY_H

/* Upstream copies messages folder-by-folder, without a transaction. Keep the
 * immutable source tree alive throughout traversal; never enumerate children
 * from the destination being created. The caller reports partial results and
 * must not retry automatically (a retry can duplicate copied messages). */
static gboolean
copy_folder_tree_sync (CamelStore *source_store,
                       CamelStore *destination_store,
                       CamelFolderInfo *source_root,
                       const gchar *destination_root,
                       gboolean delete_originals,
                       GCancellable *cancellable,
                       gboolean *started,
                       guint *completed_folders,
                       guint64 *completed_messages,
                       guint *removed_folders,
                       GError **error)
{
    GQueue pending = G_QUEUE_INIT;
    GPtrArray *deleting = g_ptr_array_new ();
    gboolean success = TRUE;
    gsize prefix_length = strlen (source_root->full_name);

    g_queue_push_tail (&pending, source_root);
    while (!g_queue_is_empty (&pending)) {
        CamelFolderInfo *info = g_queue_pop_head (&pending);
        CamelFolder *from_folder = NULL, *to_folder = NULL;
        GPtrArray *uids = NULL;
        gchar *target = NULL;

        if (g_cancellable_set_error_if_cancelled (cancellable, error)) {
            success = FALSE;
            break;
        }
        if (!info->full_name || !g_str_has_prefix (info->full_name, source_root->full_name) ||
            (info != source_root && info->full_name[prefix_length] != '/') ||
            !is_mail_folder (info) ||
            (info->flags & (CAMEL_FOLDER_VIRTUAL | CAMEL_FOLDER_VTRASH)) != 0) {
            g_set_error_literal (error, G_IO_ERROR, G_IO_ERROR_NOT_SUPPORTED,
                "The source tree contains a folder that cannot be transferred");
            success = FALSE;
            break;
        }
        if (delete_originals && (info->flags & (CAMEL_FOLDER_READONLY | CAMEL_FOLDER_SYSTEM)) != 0) {
            g_set_error_literal (error, G_IO_ERROR, G_IO_ERROR_PERMISSION_DENIED,
                "A protected source folder must not be moved");
            success = FALSE;
            break;
        }
        for (CamelFolderInfo *child = info->child; child; child = child->next)
            g_queue_push_tail (&pending, child);
        target = g_strconcat (destination_root, info->full_name + prefix_length, NULL);

        if ((info->flags & CAMEL_FOLDER_NOSELECT) != 0) {
            /* Structural parents have no messages. Create them explicitly so
             * empty non-selectable branches are retained as well. */
            CamelFolderInfo *existing = camel_store_get_folder_info_sync (
                destination_store, target, CAMEL_STORE_FOLDER_INFO_FAST,
                cancellable, NULL);
            if (!existing) {
                gchar *slash = strrchr (target, '/');
                gchar *parent = slash ? g_strndup (target, slash - target) : g_strdup ("");
                *started = TRUE;
                existing = camel_store_create_folder_sync (
                    destination_store, parent, slash ? slash + 1 : target,
                    cancellable, error);
                g_free (parent);
            }
            success = existing != NULL;
            camel_folder_info_free (existing);
        } else {
            from_folder = camel_store_get_folder_sync (
                source_store, info->full_name, CAMEL_STORE_FOLDER_NONE, cancellable, error);
            success = from_folder && camel_folder_refresh_info_sync (from_folder, cancellable, error);
            if (success) {
                *started = TRUE;
                to_folder = camel_store_get_folder_sync (
                    destination_store, target, CAMEL_STORE_FOLDER_CREATE, cancellable, error);
                success = to_folder != NULL;
            }
            if (success) {
                uids = camel_folder_dup_uids (from_folder);
                success = camel_folder_transfer_messages_to_sync (
                    from_folder, uids, to_folder, delete_originals, NULL, cancellable, error);
                if (success)
                    *completed_messages += uids->len;
                if (success && delete_originals)
                    success = camel_folder_synchronize_sync (from_folder, TRUE, cancellable, error);
            }
        }
        if (success) {
            (*completed_folders)++;
            if (delete_originals)
                g_ptr_array_add (deleting, info);
            if (CAMEL_IS_SUBSCRIBABLE (destination_store) &&
                !camel_subscribable_folder_is_subscribed (CAMEL_SUBSCRIBABLE (destination_store), target))
                success = camel_subscribable_subscribe_folder_sync (
                    CAMEL_SUBSCRIBABLE (destination_store), target, cancellable, error);
        }
        if (uids)
            g_ptr_array_unref (uids);
        g_clear_object (&from_folder);
        g_clear_object (&to_folder);
        g_free (target);
        if (!success)
            break;
    }
    /* Like upstream, remove source folders only after the complete traversal,
     * children before parents. Unlike an optimistic rollback, any failure
     * retains the remaining source tree and reports the confirmed prefix. */
    for (guint remaining = deleting->len; success && remaining > 0; remaining--) {
        CamelFolderInfo *info = g_ptr_array_index (deleting, remaining - 1);
        if (g_cancellable_set_error_if_cancelled (cancellable, error)) {
            success = FALSE;
            break;
        }
        if ((info->flags & CAMEL_FOLDER_NOSELECT) == 0) {
            CamelFolder *folder = camel_store_get_folder_sync (
                source_store, info->full_name, CAMEL_STORE_FOLDER_NONE, cancellable, error);
            success = folder && camel_folder_refresh_info_sync (folder, cancellable, error);
            if (success) {
                GPtrArray *remaining_uids = camel_folder_dup_uids (folder);
                if (remaining_uids->len > 0) {
                    g_set_error_literal (error, G_IO_ERROR, G_IO_ERROR_BUSY,
                        "New or untransferred messages in the source folder; folder will not be deleted");
                    success = FALSE;
                }
                g_ptr_array_unref (remaining_uids);
            }
            g_clear_object (&folder);
        }
        if (success && CAMEL_IS_SUBSCRIBABLE (source_store))
            success = camel_subscribable_unsubscribe_folder_sync (
                CAMEL_SUBSCRIBABLE (source_store), info->full_name, cancellable, error);
        if (success)
            success = camel_store_delete_folder_sync (source_store, info->full_name, cancellable, error);
        if (success)
            (*removed_folders)++;
    }
    g_ptr_array_unref (deleting);
    g_queue_clear (&pending);
    return success;
}

#endif
