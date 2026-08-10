/* SPDX-License-Identifier: GPL-3.0-or-later */

#ifndef EVOLUTION_EDS_CAMEL_BRIDGE_H
#define EVOLUTION_EDS_CAMEL_BRIDGE_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct {
    char *source_uid;
    char *folder_name;
    char *folder_id;
    char *message_uid;
    uint8_t *raw_message;
    size_t raw_message_len;
    int is_unread;
    int is_flagged;
} EvolutionEdsCamelMessage;

/*
 * Load messages from the enabled EDS mail-account sources.  The returned
 * records contain owned RFC 822 bytes and must be released with
 * evolution_eds_camel_free_messages().
 *
 * source_uid may be NULL to load every enabled mail-account source.
 * max_messages_per_folder <= 0 means the implementation default.
 */
int evolution_eds_camel_load_messages(
    const char *source_uid,
    int max_messages_per_folder,
    EvolutionEdsCamelMessage **out_messages,
    size_t *out_count,
    size_t *out_configured_sources,
    char **out_error);

void evolution_eds_camel_free_messages(
    EvolutionEdsCamelMessage *messages,
    size_t count);

void evolution_eds_camel_free_error(char *error_message);

#ifdef __cplusplus
}
#endif

#endif /* EVOLUTION_EDS_CAMEL_BRIDGE_H */
