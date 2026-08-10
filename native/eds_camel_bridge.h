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

typedef struct {
    char *source_uid;
    char *folder_name;
    char *folder_id;
    char *display_name;
    uint32_t unread_count;
    uint32_t total_count;
    int is_selectable;
} EvolutionEdsCamelFolder;

typedef enum {
    EVOLUTION_EDS_CAMEL_SET_MESSAGE_STATE = 1,
    EVOLUTION_EDS_CAMEL_DELETE_MESSAGE = 2,
    EVOLUTION_EDS_CAMEL_MOVE_MESSAGE = 3,
    EVOLUTION_EDS_CAMEL_ARCHIVE_MESSAGE = 4,
    EVOLUTION_EDS_CAMEL_SET_JUNK = 5,
    EVOLUTION_EDS_CAMEL_SET_IMPORTANT = 6,
} EvolutionEdsCamelOperationKind;

typedef struct {
    EvolutionEdsCamelOperationKind kind;
    const char *source_uid;
    const char *folder_name;
    const char *message_uid;
    const char *destination_folder;
    int is_unread;
    int is_flagged;
    int is_junk;
    int is_important;
} EvolutionEdsCamelOperation;

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
    int message_offset,
    int max_messages_per_folder,
    EvolutionEdsCamelMessage **out_messages,
    size_t *out_count,
    EvolutionEdsCamelFolder **out_folders,
    size_t *out_folder_count,
    size_t *out_configured_sources,
    char **out_error);

void evolution_eds_camel_free_messages(
    EvolutionEdsCamelMessage *messages,
    size_t count);

void evolution_eds_camel_free_folders(
    EvolutionEdsCamelFolder *folders,
    size_t count);

int evolution_eds_camel_apply_operation(
    const EvolutionEdsCamelOperation *operation,
    char **out_error);

void evolution_eds_camel_free_error(char *error_message);

#ifdef __cplusplus
}
#endif

#endif /* EVOLUTION_EDS_CAMEL_BRIDGE_H */
