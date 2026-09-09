/* SPDX-License-Identifier: GPL-3.0-or-later */
#ifndef EVOLUTION_EDS_ECAL_SNAPSHOT_CACHE_H
#define EVOLUTION_EDS_ECAL_SNAPSHOT_CACHE_H

/* ESource/ECalClient types and source accessors are supplied by the bridge.
 * The isolated lifetime/cache contract is also exercised with GObject test doubles. */
typedef struct {
    gchar *key;
    gchar *source_uid;
    gchar *source_configuration;
    EvolutionEdsECalSourceType source_type;
    ECalClient *client;
    gulong backend_died_handler_id;
    GCancellable *backend_dead;
} ECalSnapshotClient;

/* Snapshot loads run separately for events, tasks and memos.  Keep the
 * opened clients alive across those calls, as Evolution's EClientCache does,
 * so a reload can use locally synchronized data without waiting for a
 * provider connection again. */
static GMutex ecal_snapshot_cache_lock;
static GHashTable *ecal_snapshot_clients;

static void
free_ecal_snapshot_client (ECalSnapshotClient *entry)
{
    if (!entry)
        return;

    if (entry->client && entry->backend_died_handler_id)
        g_signal_handler_disconnect (
            entry->client, entry->backend_died_handler_id);
    g_clear_object (&entry->client);
    g_clear_object (&entry->backend_dead);
    g_free (entry->key);
    g_free (entry->source_uid);
    g_free (entry->source_configuration);
    g_free (entry);
}

/* Remove an entry without running its signal-disconnect and object-unref
 * destructors while the cache lock is held. */
static ECalSnapshotClient *
ecal_steal_snapshot_client_locked (const gchar *key,
                                   ECalClient *expected_client)
{
    ECalSnapshotClient *entry;
    gpointer stolen_key = NULL;
    gpointer stolen_value = NULL;

    if (!ecal_snapshot_clients)
        return NULL;
    entry = g_hash_table_lookup (ecal_snapshot_clients, key);
    if (!entry || (expected_client && entry->client != expected_client))
        return NULL;
    if (!g_hash_table_steal_extended (
            ecal_snapshot_clients, key, &stolen_key, &stolen_value))
        return NULL;
    g_free (stolen_key);
    return stolen_value;
}

/* The signal closure owns its own token reference. It never touches a cache
 * entry, which another thread may already have evicted/disconnected. */
static void
ecal_snapshot_client_backend_died_cb (EClient *client, GCancellable *dead)
{
    (void) client;
    g_cancellable_cancel (dead);
}

static void
ecal_snapshot_dead_token_unref (gpointer token, GClosure *closure)
{
    (void) closure;
    g_object_unref (token);
}

static gchar *
ecal_snapshot_client_key (ESource *source,
                          EvolutionEdsECalSourceType source_type)
{
    return g_strdup_printf (
        "%d:%s", (int) source_type, e_source_get_uid (source));
}

static ECalClient *
ecal_ref_cached_snapshot_client (ESource *source,
                                 EvolutionEdsECalSourceType source_type)
{
    ECalSnapshotClient *entry;
    ECalSnapshotClient *stale_entry = NULL;
    ECalClient *client = NULL;
    gchar *key = ecal_snapshot_client_key (source, source_type);
    gchar *source_configuration = e_source_to_string (source, NULL);

    g_mutex_lock (&ecal_snapshot_cache_lock);
    entry = ecal_snapshot_clients
        ? g_hash_table_lookup (ecal_snapshot_clients, key) : NULL;
    if (entry && (g_cancellable_is_cancelled (entry->backend_dead) || g_strcmp0 (
            entry->source_configuration, source_configuration) != 0)) {
        stale_entry = ecal_steal_snapshot_client_locked (key, NULL);
        entry = NULL;
    }
    if (entry)
        client = g_object_ref (entry->client);
    g_mutex_unlock (&ecal_snapshot_cache_lock);

    free_ecal_snapshot_client (stale_entry);
    g_free (key);
    g_free (source_configuration);
    return client;
}

static void
ecal_evict_snapshot_client (ESource *source,
                            EvolutionEdsECalSourceType source_type,
                            ECalClient *expected_client)
{
    ECalSnapshotClient *entry;
    gchar *key = ecal_snapshot_client_key (source, source_type);

    g_mutex_lock (&ecal_snapshot_cache_lock);
    entry = ecal_steal_snapshot_client_locked (key, expected_client);
    g_mutex_unlock (&ecal_snapshot_cache_lock);

    free_ecal_snapshot_client (entry);
    g_free (key);
}

/* Source lists come from a fresh registry each load.  Prune vanished sources
 * and changed serialized source configurations before reusing any client. */
static void
ecal_prune_snapshot_clients (GList *sources,
                             EvolutionEdsECalSourceType source_type)
{
    GHashTable *current_sources;
    GHashTable *stale_keys;
    GHashTableIter iter;
    gpointer value;
    GList *link;

    current_sources = g_hash_table_new_full (
        g_str_hash, g_str_equal, g_free, g_free);
    for (link = sources; link != NULL; link = link->next) {
        ESource *source = E_SOURCE (link->data);
        const gchar *uid = e_source_get_uid (source);

        if (uid && *uid) {
            g_hash_table_replace (
                current_sources, g_strdup (uid),
                e_source_to_string (source, NULL));
        }
    }

    stale_keys = g_hash_table_new_full (g_str_hash, g_str_equal, g_free, NULL);
    g_mutex_lock (&ecal_snapshot_cache_lock);
    if (ecal_snapshot_clients) {
        g_hash_table_iter_init (&iter, ecal_snapshot_clients);
        while (g_hash_table_iter_next (&iter, NULL, &value)) {
            ECalSnapshotClient *entry = value;
            const gchar *configuration;

            if (entry->source_type != source_type)
                continue;
            configuration = g_hash_table_lookup (
                current_sources, entry->source_uid);
            if (!configuration || g_strcmp0 (
                    configuration, entry->source_configuration) != 0)
                g_hash_table_add (stale_keys, g_strdup (entry->key));
        }
    }
    g_mutex_unlock (&ecal_snapshot_cache_lock);

    g_hash_table_iter_init (&iter, stale_keys);
    while (g_hash_table_iter_next (&iter, &value, NULL)) {
        ECalSnapshotClient *entry;
        const gchar *configuration;

        g_mutex_lock (&ecal_snapshot_cache_lock);
        entry = ecal_snapshot_clients
            ? g_hash_table_lookup (ecal_snapshot_clients, value) : NULL;
        configuration = entry ? g_hash_table_lookup (
            current_sources, entry->source_uid) : NULL;
        if (entry && (!configuration || g_strcmp0 (
                configuration, entry->source_configuration) != 0))
            entry = ecal_steal_snapshot_client_locked (value, entry->client);
        else
            entry = NULL;
        g_mutex_unlock (&ecal_snapshot_cache_lock);
        free_ecal_snapshot_client (entry);
    }
    g_hash_table_destroy (stale_keys);
    g_hash_table_destroy (current_sources);
}

/* Takes the caller's reference to @client and returns a cache-held client
 * reference.  A concurrent opener loses its redundant client safely. */
static ECalClient *
ecal_cache_snapshot_client (ESource *source,
                            EvolutionEdsECalSourceType source_type,
                            ECalClient *client)
{
    ECalSnapshotClient *entry;
    ECalSnapshotClient *stale_entry = NULL;
    ECalClient *result;
    gchar *key;
    gchar *source_configuration;

    if (!client)
        return NULL;

    key = ecal_snapshot_client_key (source, source_type);
    source_configuration = e_source_to_string (source, NULL);
    g_mutex_lock (&ecal_snapshot_cache_lock);
    if (!ecal_snapshot_clients) {
        ecal_snapshot_clients = g_hash_table_new_full (
            g_str_hash, g_str_equal, g_free,
            (GDestroyNotify) free_ecal_snapshot_client);
    }
    entry = g_hash_table_lookup (ecal_snapshot_clients, key);
    if (entry) {
        if (!g_cancellable_is_cancelled (entry->backend_dead) &&
            g_strcmp0 (entry->source_configuration, source_configuration) == 0) {
            result = g_object_ref (entry->client);
            g_mutex_unlock (&ecal_snapshot_cache_lock);
            g_free (key);
            g_free (source_configuration);
            g_object_unref (client);
            return result;
        }
        stale_entry = ecal_steal_snapshot_client_locked (key, NULL);
    }

    entry = g_new0 (ECalSnapshotClient, 1);
    entry->key = key;
    entry->source_uid = g_strdup (e_source_get_uid (source));
    entry->source_configuration = source_configuration;
    entry->source_type = source_type;
    entry->client = g_object_ref (client);
    entry->backend_dead = g_cancellable_new ();
    entry->backend_died_handler_id = g_signal_connect_data (
        client, "backend-died",
        G_CALLBACK (ecal_snapshot_client_backend_died_cb),
        g_object_ref (entry->backend_dead), ecal_snapshot_dead_token_unref, 0);
    g_hash_table_insert (
        ecal_snapshot_clients, g_strdup (entry->key), entry);
    result = g_object_ref (client);
    g_mutex_unlock (&ecal_snapshot_cache_lock);

    free_ecal_snapshot_client (stale_entry);
    g_object_unref (client);
    return result;
}


#endif
