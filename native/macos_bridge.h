// SPDX-License-Identifier: GPL-3.0-or-later
//
// macOS Native Bridge Header
// Provides C-compatible interface to EventKit and Contacts.framework

#ifndef MACOS_BRIDGE_H
#define MACOS_BRIDGE_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// Status codes:
// 0: NotDetermined
// 1: Restricted
// 2: Denied
// 3: Authorized / FullAccess
// 4: WriteOnly (macOS 14+)

int macos_check_calendar_permission(void);
int macos_check_reminders_permission(void);
int macos_check_contacts_permission(void);

typedef void (*macos_permission_callback_t)(int granted, void *user_data);

void macos_request_calendar_permission(macos_permission_callback_t callback, void *user_data);
void macos_request_reminders_permission(macos_permission_callback_t callback, void *user_data);
void macos_request_contacts_permission(macos_permission_callback_t callback, void *user_data);

// Export functions:
// Returns null-terminated heap-allocated UTF-8 string or NULL on error.
// Must be freed using macos_free_string().
char *macos_export_calendar_events_ics(void);
char *macos_export_reminders_ics(void);
char *macos_export_contacts_vcf(void);

/* List public EventKit calendars/reminder lists and Contacts containers
 * without asking for permission or reading records.  The returned UTF-8 JSON
 * follows schema version 1 and is released with macos_free_string(). */
char *macos_list_system_sources_json(void);

/* Load per-source EventKit and Contacts records when the app already has read
 * permission.  This never prompts; unavailable, write-only or limited access
 * is represented in the JSON errors array.  Reminder loading must be called
 * from a worker thread because EventKit completes it asynchronously. */
char *macos_load_system_data_json(void);

/* Open System Settings' public Internet Accounts deep link. */
void macos_open_internet_accounts(void);

/* Monotonically increases after EventKit or Contacts reports a local/system
 * data change.  Registering observers does not enumerate sources or records. */
uint64_t macos_system_change_generation(void);

// Open macOS System Settings pane (e.g. "Privacy_Calendars", "Privacy_Contacts", "Privacy_Reminders")
void macos_open_privacy_settings(const char *pane);

// Free memory allocated by macos bridge
void macos_free_string(char *ptr);

// Keychain Services (Security.framework)
// Default service identifier: "org.gnome.Evolution4"
int macos_keychain_set_password(const char *service, const char *account, const char *password);
char *macos_keychain_get_password(const char *service, const char *account);
int macos_keychain_delete_password(const char *service, const char *account);

#ifdef __cplusplus
}
#endif

#endif // MACOS_BRIDGE_H
