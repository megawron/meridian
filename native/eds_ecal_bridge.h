/* SPDX-License-Identifier: GPL-3.0-or-later */

#ifndef EVOLUTION_EDS_ECAL_BRIDGE_H
#define EVOLUTION_EDS_ECAL_BRIDGE_H

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef enum {
    EVOLUTION_EDS_ECAL_EVENTS = 0,
    EVOLUTION_EDS_ECAL_TASKS = 1,
    EVOLUTION_EDS_ECAL_MEMOS = 2,
} EvolutionEdsECalSourceType;

typedef struct {
    char *source_uid;
    char *source_display_name;
    char *component;
} EvolutionEdsECalComponent;

typedef enum {
    EVOLUTION_EDS_ECAL_CREATE = 1,
    EVOLUTION_EDS_ECAL_MODIFY = 2,
    EVOLUTION_EDS_ECAL_REMOVE = 3,
} EvolutionEdsECalOperationKind;

/* Load all enabled EDS calendar objects of one source type.  The component
 * strings are owned by the caller and must be released with
 * evolution_eds_ecal_free_components(). */
int evolution_eds_ecal_load_components(
    EvolutionEdsECalSourceType source_type,
    EvolutionEdsECalComponent **out_components,
    size_t *out_count,
    size_t *out_configured_sources,
    size_t *out_successful_sources,
    size_t *out_failed_sources,
    char **out_error);

void evolution_eds_ecal_free_components(
    EvolutionEdsECalComponent *components,
    size_t count);

/* Apply one ECal mutation.  Returns 1 on success, 0 on a regular backend
 * error and 2 when the backend rejected the operation as a conflict.  For
 * create/modify, out_component contains the component confirmed by EDS. */
int evolution_eds_ecal_apply_operation(
    const char *source_uid,
    EvolutionEdsECalSourceType source_type,
    EvolutionEdsECalOperationKind operation,
    const char *component_string,
    const char *component_uid,
    char **out_uid,
    char **out_component,
    char **out_error);

void evolution_eds_ecal_free_error(char *error_message);

#ifdef __cplusplus
}
#endif

#endif /* EVOLUTION_EDS_ECAL_BRIDGE_H */
