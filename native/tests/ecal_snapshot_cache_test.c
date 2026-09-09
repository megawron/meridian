/* SPDX-License-Identifier: GPL-3.0-or-later */
#include <gio/gio.h>
#include "../eds_ecal_bridge.h"

typedef GObject ESource;
typedef GObject EClient;
typedef GObject ECalClient;
#define E_SOURCE(value) ((ESource *) (value))
static const gchar *e_source_get_uid (ESource *source) {
    return g_object_get_data (source, "uid");
}
static gchar *e_source_to_string (ESource *source, gsize *length) {
    const gchar *config = g_object_get_data (source, "config");
    if (length) *length = strlen (config);
    return g_strdup (config);
}
#include "../eds_ecal_snapshot_cache.h"

static ESource *new_source (const gchar *uid, const gchar *config) {
    ESource *source = g_object_new (G_TYPE_OBJECT, NULL);
    g_object_set_data_full (source, "uid", g_strdup (uid), g_free);
    g_object_set_data_full (source, "config", g_strdup (config), g_free);
    return source;
}
static ECalClient *new_client (void) {
    return g_object_new (G_TYPE_OBJECT, NULL);
}
static void test_reuse_and_config_change (void) {
    ESource *source = new_source ("one", "original");
    ECalClient *client = ecal_cache_snapshot_client (source, EVOLUTION_EDS_ECAL_EVENTS, new_client ());
    ECalClient *cached = ecal_ref_cached_snapshot_client (source, EVOLUTION_EDS_ECAL_EVENTS);
    g_assert_true (cached == client);
    g_object_unref (cached);
    g_assert_null (ecal_ref_cached_snapshot_client (source, EVOLUTION_EDS_ECAL_TASKS));
    g_object_set_data_full (source, "config", g_strdup ("changed"), g_free);
    g_assert_null (ecal_ref_cached_snapshot_client (source, EVOLUTION_EDS_ECAL_EVENTS));
    g_object_unref (client);
    g_object_unref (source);
}
static void test_backend_death_and_eviction (void) {
    ESource *source = new_source ("two", "original");
    ECalClient *client = ecal_cache_snapshot_client (source, EVOLUTION_EDS_ECAL_EVENTS, new_client ());
    g_signal_emit_by_name (client, "backend-died");
    g_assert_null (ecal_ref_cached_snapshot_client (source, EVOLUTION_EDS_ECAL_EVENTS));
    /* The caller still owns a reference after eviction; late signals are safe. */
    g_signal_emit_by_name (client, "backend-died");
    g_object_unref (client);
    client = ecal_cache_snapshot_client (source, EVOLUTION_EDS_ECAL_EVENTS, new_client ());
    ecal_evict_snapshot_client (source, EVOLUTION_EDS_ECAL_EVENTS, client);
    g_assert_null (ecal_ref_cached_snapshot_client (source, EVOLUTION_EDS_ECAL_EVENTS));
    g_object_unref (client);
    g_object_unref (source);
}
static void test_removed_source_is_pruned (void) {
    ESource *source = new_source ("three", "original");
    ECalClient *client = ecal_cache_snapshot_client (source, EVOLUTION_EDS_ECAL_EVENTS, new_client ());
    ECalClient *tasks = ecal_cache_snapshot_client (source, EVOLUTION_EDS_ECAL_TASKS, new_client ());
    ecal_prune_snapshot_clients (NULL, EVOLUTION_EDS_ECAL_EVENTS);
    g_assert_null (ecal_ref_cached_snapshot_client (source, EVOLUTION_EDS_ECAL_EVENTS));
    ECalClient *cached = ecal_ref_cached_snapshot_client (source, EVOLUTION_EDS_ECAL_TASKS);
    g_assert_true (cached == tasks);
    g_object_unref (cached);
    ecal_evict_snapshot_client (source, EVOLUTION_EDS_ECAL_TASKS, tasks);
    g_object_unref (client);
    g_object_unref (tasks);
    g_object_unref (source);
}
typedef struct { ECalClient *client; gint stop; } DeathEmitter;
static gpointer emit_death_until_stopped (gpointer data) {
    DeathEmitter *emitter = data;
    while (!g_atomic_int_get (&emitter->stop))
        g_signal_emit_by_name (emitter->client, "backend-died");
    return NULL;
}
static void test_signal_races_eviction (void) {
    ESource *source = new_source ("race", "original");
    ECalClient *client = new_client ();
    DeathEmitter emitter = { client, 0 };
    GThread *thread = g_thread_new ("backend-death", emit_death_until_stopped, &emitter);
    for (guint ii = 0; ii < 1000; ii++) {
        ECalClient *owned = ecal_cache_snapshot_client (
            source, EVOLUTION_EDS_ECAL_EVENTS, g_object_ref (client));
        ecal_evict_snapshot_client (source, EVOLUTION_EDS_ECAL_EVENTS, owned);
        g_object_unref (owned);
    }
    g_atomic_int_set (&emitter.stop, 1);
    g_thread_join (thread);
    g_object_unref (client);
    g_object_unref (source);
}
int main (int argc, char **argv) {
    g_test_init (&argc, &argv, NULL);
    g_signal_new ("backend-died", G_TYPE_OBJECT, G_SIGNAL_RUN_LAST, 0, NULL, NULL, NULL, G_TYPE_NONE, 0);
    g_test_add_func ("/ecal-cache/reuse-config", test_reuse_and_config_change);
    g_test_add_func ("/ecal-cache/backend-death", test_backend_death_and_eviction);
    g_test_add_func ("/ecal-cache/source-removal", test_removed_source_is_pruned);
    g_test_add_func ("/ecal-cache/signal-eviction-race", test_signal_races_eviction);
    int result = g_test_run ();
    g_clear_pointer (&ecal_snapshot_clients, g_hash_table_destroy);
    return result;
}
