// SPDX-License-Identifier: GPL-3.0-or-later
//
// macOS Native Bridge Implementation
// Interfaces with EventKit, Contacts, Foundation, and AppKit

#import <Foundation/Foundation.h>
#import <EventKit/EventKit.h>
#import <Contacts/Contacts.h>
#import <AppKit/AppKit.h>
#import <Security/Security.h>

#include "macos_bridge.h"
#include <stdlib.h>
#include <string.h>
#include <stdatomic.h>

/* EventKit reminder queries are completion-handler based.  The completion
 * retains this object, so a late reply after the caller's bounded wait never
 * writes through a stack pointer or touches a returned JSON response. */
@interface MacosReminderFetchState : NSObject {
@public
    dispatch_semaphore_t semaphore;
    NSArray<EKReminder *> *reminders;
}
@end

@implementation MacosReminderFetchState
@end

static _Atomic(uint64_t) macos_system_generation = 1;
static dispatch_once_t macos_system_stores_once;
static dispatch_once_t macos_system_observers_once;
static EKEventStore *macos_system_event_store;
static CNContactStore *macos_system_contact_store;

static void
macos_note_system_change (NSNotification *notification)
{
    (void) notification;
    atomic_fetch_add_explicit (
        &macos_system_generation, 1, memory_order_relaxed);
}

static void
macos_create_system_stores (void)
{
    macos_system_event_store = [[EKEventStore alloc] init];
    macos_system_contact_store = [[CNContactStore alloc] init];
}

static void
macos_register_system_change_observers (void)
{
    dispatch_once (&macos_system_observers_once, ^{
        void (^register_on_main)(void) = ^{
            NSNotificationCenter *center = [NSNotificationCenter defaultCenter];
            [center addObserverForName:EKEventStoreChangedNotification
                                object:nil
                                 queue:nil
                            usingBlock:^(NSNotification *notification) {
                macos_note_system_change (notification);
            }];
            [center addObserverForName:CNContactStoreDidChangeNotification
                                object:nil
                                 queue:nil
                            usingBlock:^(NSNotification *notification) {
                macos_note_system_change (notification);
            }];
        };

        if ([NSThread isMainThread])
            register_on_main ();
        else
            dispatch_async (dispatch_get_main_queue (), register_on_main);
    });
}

static EKEventStore *
macos_shared_event_store (void)
{
    dispatch_once (&macos_system_stores_once, ^{
        macos_create_system_stores ();
    });
    macos_register_system_change_observers ();
    return macos_system_event_store;
}

static CNContactStore *
macos_shared_contact_store (void)
{
    dispatch_once (&macos_system_stores_once, ^{
        macos_create_system_stores ();
    });
    macos_register_system_change_observers ();
    return macos_system_contact_store;
}

uint64_t
macos_system_change_generation (void)
{
    macos_register_system_change_observers ();
    return atomic_load_explicit (&macos_system_generation, memory_order_relaxed);
}

static NSString *format_date_ics(NSDate *date, BOOL allDay) {
    if (!date) return @"";
    NSDateFormatter *formatter = [[NSDateFormatter alloc] init];
    formatter.timeZone = [NSTimeZone timeZoneWithAbbreviation:@"UTC"];
    if (allDay) {
        formatter.dateFormat = @"yyyyMMdd";
    } else {
        formatter.dateFormat = @"yyyyMMdd'T'HHmmss'Z'";
    }
    return [formatter stringFromDate:date];
}

static NSString *escape_ics_text(NSString *text) {
    if (!text) return @"";
    NSString *escaped = [text stringByReplacingOccurrencesOfString:@"\\" withString:@"\\\\"];
    escaped = [escaped stringByReplacingOccurrencesOfString:@";" withString:@"\\;"];
    escaped = [escaped stringByReplacingOccurrencesOfString:@"," withString:@"\\,"];
    escaped = [escaped stringByReplacingOccurrencesOfString:@"\r\n" withString:@"\\n"];
    escaped = [escaped stringByReplacingOccurrencesOfString:@"\n" withString:@"\\n"];
    escaped = [escaped stringByReplacingOccurrencesOfString:@"\r" withString:@"\\n"];
    return escaped;
}

static NSString *hex_color_from_cgcolor(CGColorRef cgColor) {
    if (!cgColor) return nil;
    NSColor *color = [NSColor colorWithCGColor:cgColor];
    if (!color) return nil;
    NSColor *rgb = [color colorUsingColorSpace:[NSColorSpace sRGBColorSpace]];
    if (!rgb) return nil;
    int r = (int)round(rgb.redComponent * 255.0);
    int g = (int)round(rgb.greenComponent * 255.0);
    int b = (int)round(rgb.blueComponent * 255.0);
    return [NSString stringWithFormat:@"#%02x%02x%02x", r, g, b];
}

int macos_check_calendar_permission(void) {
    @autoreleasepool {
        EKAuthorizationStatus status = [EKEventStore authorizationStatusForEntityType:EKEntityTypeEvent];
        return (int)status;
    }
}

int macos_check_reminders_permission(void) {
    @autoreleasepool {
        EKAuthorizationStatus status = [EKEventStore authorizationStatusForEntityType:EKEntityTypeReminder];
        return (int)status;
    }
}

int macos_check_contacts_permission(void) {
    @autoreleasepool {
        CNAuthorizationStatus status = [CNContactStore authorizationStatusForEntityType:CNEntityTypeContacts];
        return (int)status;
    }
}

void macos_request_calendar_permission(macos_permission_callback_t callback, void *user_data) {
    void (^requestBlock)(void) = ^{
        EKEventStore *store = macos_shared_event_store();
        if (@available(macOS 14.0, *)) {
            [store requestFullAccessToEventsWithCompletion:^(BOOL granted, NSError * _Nullable error) {
                if (error) {
                    NSLog(@"[macOS Bridge] Calendar requestFullAccess error: %@", error);
                }
                if (callback) {
                    callback(granted ? 1 : 0, user_data);
                }
            }];
        } else {
            [store requestAccessToEntityType:EKEntityTypeEvent completion:^(BOOL granted, NSError * _Nullable error) {
                if (error) {
                    NSLog(@"[macOS Bridge] Calendar requestAccess error: %@", error);
                }
                if (callback) {
                    callback(granted ? 1 : 0, user_data);
                }
            }];
        }
    };

    if ([NSThread isMainThread]) {
        requestBlock();
    } else {
        dispatch_async(dispatch_get_main_queue(), requestBlock);
    }
}

void macos_request_reminders_permission(macos_permission_callback_t callback, void *user_data) {
    void (^requestBlock)(void) = ^{
        EKEventStore *store = macos_shared_event_store();
        if (@available(macOS 14.0, *)) {
            [store requestFullAccessToRemindersWithCompletion:^(BOOL granted, NSError * _Nullable error) {
                if (error) {
                    NSLog(@"[macOS Bridge] Reminders requestFullAccess error: %@", error);
                }
                if (callback) {
                    callback(granted ? 1 : 0, user_data);
                }
            }];
        } else {
            [store requestAccessToEntityType:EKEntityTypeReminder completion:^(BOOL granted, NSError * _Nullable error) {
                if (error) {
                    NSLog(@"[macOS Bridge] Reminders requestAccess error: %@", error);
                }
                if (callback) {
                    callback(granted ? 1 : 0, user_data);
                }
            }];
        }
    };

    if ([NSThread isMainThread]) {
        requestBlock();
    } else {
        dispatch_async(dispatch_get_main_queue(), requestBlock);
    }
}

void macos_request_contacts_permission(macos_permission_callback_t callback, void *user_data) {
    void (^requestBlock)(void) = ^{
        CNContactStore *store = macos_shared_contact_store();
        [store requestAccessForEntityType:CNEntityTypeContacts completionHandler:^(BOOL granted, NSError * _Nullable error) {
            if (error) {
                NSLog(@"[macOS Bridge] Contacts requestAccess error: %@", error);
            }
            if (callback) {
                callback(granted ? 1 : 0, user_data);
            }
        }];
    };

    if ([NSThread isMainThread]) {
        requestBlock();
    } else {
        dispatch_async(dispatch_get_main_queue(), requestBlock);
    }
}

char *macos_export_calendar_events_ics(void) {
    @autoreleasepool {
        EKEventStore *store = macos_shared_event_store();
        
        // Query events from 1 year past to 2 years future
        NSDate *now = [NSDate date];
        NSCalendar *cal = [NSCalendar currentCalendar];
        NSDate *startDate = [cal dateByAddingUnit:NSCalendarUnitYear value:-1 toDate:now options:0];
        NSDate *endDate = [cal dateByAddingUnit:NSCalendarUnitYear value:2 toDate:now options:0];
        
        if (!startDate) startDate = [now dateByAddingTimeInterval:-365 * 86400];
        if (!endDate) endDate = [now dateByAddingTimeInterval:730 * 86400];
        
        NSPredicate *pred = [store predicateForEventsWithStartDate:startDate endDate:endDate calendars:nil];
        NSArray<EKEvent *> *events = [store eventsMatchingPredicate:pred];
        
        NSMutableString *ics = [NSMutableString string];
        [ics appendString:@"BEGIN:VCALENDAR\r\n"];
        [ics appendString:@"VERSION:2.0\r\n"];
        [ics appendString:@"PRODID:-//Evolution GTK4//macOS EventKit Bridge//EN\r\n"];
        [ics appendString:@"CALSCALE:GREGORIAN\r\n"];
        
        for (EKEvent *event in events) {
            [ics appendString:@"BEGIN:VEVENT\r\n"];
            
            NSString *uid = event.eventIdentifier;
            if (!uid || uid.length == 0) {
                uid = [[NSUUID UUID] UUIDString];
            }
            [ics appendFormat:@"UID:%@\r\n", escape_ics_text(uid)];
            
            if (event.title.length > 0) {
                [ics appendFormat:@"SUMMARY:%@\r\n", escape_ics_text(event.title)];
            } else {
                [ics appendString:@"SUMMARY:(Kein Titel)\r\n"];
            }
            
            if (event.notes.length > 0) {
                [ics appendFormat:@"DESCRIPTION:%@\r\n", escape_ics_text(event.notes)];
            }
            
            if (event.location.length > 0) {
                [ics appendFormat:@"LOCATION:%@\r\n", escape_ics_text(event.location)];
            }
            
            if (event.allDay) {
                [ics appendFormat:@"DTSTART;VALUE=DATE:%@\r\n", format_date_ics(event.startDate, YES)];
                if (event.endDate) {
                    [ics appendFormat:@"DTEND;VALUE=DATE:%@\r\n", format_date_ics(event.endDate, YES)];
                }
            } else {
                if (event.startDate) {
                    [ics appendFormat:@"DTSTART:%@\r\n", format_date_ics(event.startDate, NO)];
                }
                if (event.endDate) {
                    [ics appendFormat:@"DTEND:%@\r\n", format_date_ics(event.endDate, NO)];
                }
            }
            
            if (event.creationDate) {
                [ics appendFormat:@"CREATED:%@\r\n", format_date_ics(event.creationDate, NO)];
            }
            if (event.lastModifiedDate) {
                [ics appendFormat:@"LAST-MODIFIED:%@\r\n", format_date_ics(event.lastModifiedDate, NO)];
            }
            
            if (event.calendar && event.calendar.title.length > 0) {
                [ics appendFormat:@"CATEGORIES:%@\r\n", escape_ics_text(event.calendar.title)];
                [ics appendFormat:@"X-EVOLUTION-SOURCE-NAME:%@\r\n", escape_ics_text(event.calendar.title)];
                NSString *accUid = event.calendar.source.sourceIdentifier ?: @"";
                NSString *srcUid = [NSString stringWithFormat:@"macos:calendar:%lu:%@:%@",
                    (unsigned long)accUid.length,
                    accUid,
                    event.calendar.calendarIdentifier ?: @""];
                [ics appendFormat:@"X-EVOLUTION-SOURCE-UID:%@\r\n", escape_ics_text(srcUid)];
                NSString *hexColor = hex_color_from_cgcolor(event.calendar.CGColor);
                if (hexColor && hexColor.length > 0) {
                    [ics appendFormat:@"COLOR:%@\r\n", hexColor];
                    [ics appendFormat:@"X-APPLE-CALENDAR-COLOR:%@\r\n", hexColor];
                }
            }
            
            [ics appendString:@"STATUS:CONFIRMED\r\n"];
            [ics appendString:@"END:VEVENT\r\n"];
        }
        
        [ics appendString:@"END:VCALENDAR\r\n"];
        
        const char *utf8 = [ics UTF8String];
        return utf8 ? strdup(utf8) : NULL;
    }
}

char *macos_export_reminders_ics(void) {
    @autoreleasepool {
        EKEventStore *store = macos_shared_event_store();
        NSPredicate *pred = [store predicateForRemindersInCalendars:nil];
        MacosReminderFetchState *state;

        if ([NSThread isMainThread] || !pred)
            return strdup("");
        state = [[MacosReminderFetchState alloc] init];
        state->semaphore = dispatch_semaphore_create(0);
        [store fetchRemindersMatchingPredicate:pred completion:^(NSArray<EKReminder *> * _Nullable items) {
            state->reminders = items ?: @[];
            dispatch_semaphore_signal(state->semaphore);
        }];
        dispatch_time_t timeout = dispatch_time(DISPATCH_TIME_NOW, 5 * NSEC_PER_SEC);
        if (dispatch_semaphore_wait(state->semaphore, timeout) != 0)
            return strdup("");
        
        NSMutableString *ics = [NSMutableString string];
        [ics appendString:@"BEGIN:VCALENDAR\r\n"];
        [ics appendString:@"VERSION:2.0\r\n"];
        [ics appendString:@"PRODID:-//Evolution GTK4//macOS EventKit Bridge Reminders//EN\r\n"];
        
        for (EKReminder *reminder in state->reminders) {
            [ics appendString:@"BEGIN:VTODO\r\n"];
            
            NSString *uid = reminder.calendarItemIdentifier;
            if (!uid || uid.length == 0) {
                uid = [[NSUUID UUID] UUIDString];
            }
            [ics appendFormat:@"UID:%@\r\n", escape_ics_text(uid)];
            
            if (reminder.title.length > 0) {
                [ics appendFormat:@"SUMMARY:%@\r\n", escape_ics_text(reminder.title)];
            } else {
                [ics appendString:@"SUMMARY:(Aufgabe)\r\n"];
            }
            
            if (reminder.notes.length > 0) {
                [ics appendFormat:@"DESCRIPTION:%@\r\n", escape_ics_text(reminder.notes)];
            }
            
            if (reminder.dueDateComponents) {
                NSDate *dueDate = [[NSCalendar currentCalendar] dateFromComponents:reminder.dueDateComponents];
                if (dueDate) {
                    [ics appendFormat:@"DUE:%@\r\n", format_date_ics(dueDate, NO)];
                }
            }
            
            if (reminder.completed) {
                [ics appendString:@"STATUS:COMPLETED\r\n"];
                if (reminder.completionDate) {
                    [ics appendFormat:@"COMPLETED:%@\r\n", format_date_ics(reminder.completionDate, NO)];
                }
            } else {
                [ics appendString:@"STATUS:NEEDS-ACTION\r\n"];
            }
            
            if (reminder.calendar && reminder.calendar.title.length > 0) {
                [ics appendFormat:@"CATEGORIES:%@\r\n", escape_ics_text(reminder.calendar.title)];
                [ics appendFormat:@"X-EVOLUTION-SOURCE-NAME:%@\r\n", escape_ics_text(reminder.calendar.title)];
                NSString *accUid = reminder.calendar.source.sourceIdentifier ?: @"";
                NSString *srcUid = [NSString stringWithFormat:@"macos:reminders:%lu:%@:%@",
                    (unsigned long)accUid.length,
                    accUid,
                    reminder.calendar.calendarIdentifier ?: @""];
                [ics appendFormat:@"X-EVOLUTION-SOURCE-UID:%@\r\n", escape_ics_text(srcUid)];
                NSString *hexColor = hex_color_from_cgcolor(reminder.calendar.CGColor);
                if (hexColor && hexColor.length > 0) {
                    [ics appendFormat:@"COLOR:%@\r\n", hexColor];
                    [ics appendFormat:@"X-APPLE-CALENDAR-COLOR:%@\r\n", hexColor];
                }
            }
            
            [ics appendString:@"END:VTODO\r\n"];
        }
        
        [ics appendString:@"END:VCALENDAR\r\n"];
        
        const char *utf8 = [ics UTF8String];
        return utf8 ? strdup(utf8) : NULL;
    }
}

char *macos_export_contacts_vcf(void) {
    @autoreleasepool {
        CNContactStore *store = macos_shared_contact_store();
        
        NSArray<id<CNKeyDescriptor>> *keys = @[
            [CNContactVCardSerialization descriptorForRequiredKeys]
        ];
        
        CNContactFetchRequest *request = [[CNContactFetchRequest alloc] initWithKeysToFetch:keys];
        NSMutableArray<CNContact *> *contacts = [NSMutableArray array];
        
        NSError *fetchError = nil;
        [store enumerateContactsWithFetchRequest:request error:&fetchError usingBlock:^(CNContact * _Nonnull contact, BOOL * _Nonnull stop) {
            (void) stop;
            [contacts addObject:contact];
        }];
        
        if (contacts.count == 0) {
            return strdup("");
        }
        
        NSError *vcardError = nil;
        NSData *vcardData = [CNContactVCardSerialization dataWithContacts:contacts error:&vcardError];
        
        if (vcardData && vcardData.length > 0) {
            NSString *vcardStr = [[NSString alloc] initWithData:vcardData encoding:NSUTF8StringEncoding];
            if (vcardStr) {
                return strdup([vcardStr UTF8String]);
            }
        }
        
        return strdup("");
    }
}

static void
macos_append_error (NSMutableArray<NSString *> *errors, NSString *message)
{
    if (message.length > 0)
        [errors addObject:message];
}

static BOOL
macos_eventkit_has_read_access (EKEntityType entity_type)
{
    EKAuthorizationStatus status = [EKEventStore authorizationStatusForEntityType:entity_type];

    if (@available(macOS 14.0, *))
        return status == EKAuthorizationStatusFullAccess;
    return status == 3; /* EKAuthorizationStatusAuthorized before macOS 14. */
}

static NSString *
macos_eventkit_read_access_error (NSString *kind, EKEntityType entity_type)
{
    EKAuthorizationStatus status = [EKEventStore authorizationStatusForEntityType:entity_type];

    if (@available(macOS 14.0, *)) {
        if (status == EKAuthorizationStatusWriteOnly) {
            return [NSString stringWithFormat:
                @"%@ access is write-only; full read access is required to load system data.", kind];
        }
    }
    if (status == EKAuthorizationStatusNotDetermined) {
        return [NSString stringWithFormat:
            @"%@ access has not been granted; discovery did not request permission.", kind];
    }
    if (status == EKAuthorizationStatusRestricted || status == EKAuthorizationStatusDenied) {
        return [NSString stringWithFormat:
            @"%@ read access is unavailable (authorization status %ld).",
            kind, (long) status];
    }
    return [NSString stringWithFormat:
        @"%@ does not have full read access (authorization status %ld).",
        kind, (long) status];
}

static BOOL
macos_contacts_has_read_access (void)
{
    return [CNContactStore authorizationStatusForEntityType:CNEntityTypeContacts] ==
        CNAuthorizationStatusAuthorized;
}

static NSString *
macos_contacts_read_access_error (void)
{
    CNAuthorizationStatus status = [CNContactStore authorizationStatusForEntityType:CNEntityTypeContacts];

    if (status == CNAuthorizationStatusNotDetermined) {
        return @"Contacts access has not been granted; discovery did not request permission.";
    }
    if (status == CNAuthorizationStatusRestricted || status == CNAuthorizationStatusDenied) {
        return [NSString stringWithFormat:
            @"Contacts read access is unavailable (authorization status %ld).", (long) status];
    }
    return [NSString stringWithFormat:
        @"Contacts access is limited or unavailable for a full snapshot (authorization status %ld).",
        (long) status];
}

static NSDictionary *
macos_eventkit_source_dictionary (EKCalendar *calendar, NSString *kind)
{
    EKSource *source = calendar.source;
    NSString *hexColor = hex_color_from_cgcolor(calendar.CGColor);

    return @{
        @"uid": calendar.calendarIdentifier ?: @"",
        @"name": calendar.title ?: @"",
        @"kind": kind,
        @"account_uid": source.sourceIdentifier ?: @"",
        @"account_name": source.title ?: @"",
        @"backend": @"eventkit",
        @"writable": @(calendar.allowsContentModifications),
        @"color": hexColor ?: @"",
    };
}

static NSDictionary *
macos_contacts_source_dictionary (CNContainer *container)
{
    /* Contacts exposes no public per-container mutability indicator.  Keep
     * the conservative false value rather than inferring write support. */
    return @{
        @"uid": container.identifier ?: @"",
        @"name": container.name ?: @"",
        @"kind": @"contacts",
        @"account_uid": container.identifier ?: @"",
        @"account_name": container.name ?: @"",
        @"backend": @"contacts",
        @"writable": @NO,
    };
}

static NSString *
macos_event_ics_component (EKEvent *event)
{
    NSString *uid = event.eventIdentifier ?: event.calendarItemIdentifier;
    NSMutableString *ics;

    if (uid.length == 0)
        return nil;
    ics = [NSMutableString stringWithString:@"BEGIN:VEVENT\r\n"];
    [ics appendFormat:@"UID:%@\r\n", escape_ics_text (uid)];
    [ics appendFormat:@"SUMMARY:%@\r\n", escape_ics_text (event.title ?: @"")];
    if (event.notes.length > 0)
        [ics appendFormat:@"DESCRIPTION:%@\r\n", escape_ics_text (event.notes)];
    if (event.location.length > 0)
        [ics appendFormat:@"LOCATION:%@\r\n", escape_ics_text (event.location)];
    if (event.allDay) {
        [ics appendFormat:@"DTSTART;VALUE=DATE:%@\r\n", format_date_ics (event.startDate, YES)];
        [ics appendFormat:@"DTEND;VALUE=DATE:%@\r\n", format_date_ics (event.endDate, YES)];
    } else {
        [ics appendFormat:@"DTSTART:%@\r\n", format_date_ics (event.startDate, NO)];
        [ics appendFormat:@"DTEND:%@\r\n", format_date_ics (event.endDate, NO)];
    }
    if (event.creationDate)
        [ics appendFormat:@"CREATED:%@\r\n", format_date_ics (event.creationDate, NO)];
    if (event.lastModifiedDate)
        [ics appendFormat:@"LAST-MODIFIED:%@\r\n", format_date_ics (event.lastModifiedDate, NO)];
    if (event.calendar && event.calendar.title.length > 0) {
        [ics appendFormat:@"CATEGORIES:%@\r\n", escape_ics_text (event.calendar.title)];
        [ics appendFormat:@"X-EVOLUTION-SOURCE-NAME:%@\r\n", escape_ics_text (event.calendar.title)];
        NSString *accUid = event.calendar.source.sourceIdentifier ?: @"";
        NSString *srcUid = [NSString stringWithFormat:@"macos:calendar:%lu:%@:%@",
            (unsigned long)accUid.length,
            accUid,
            event.calendar.calendarIdentifier ?: @""];
        [ics appendFormat:@"X-EVOLUTION-SOURCE-UID:%@\r\n", escape_ics_text (srcUid)];
        NSString *hexColor = hex_color_from_cgcolor(event.calendar.CGColor);
        if (hexColor && hexColor.length > 0) {
            [ics appendFormat:@"COLOR:%@\r\n", hexColor];
            [ics appendFormat:@"X-APPLE-CALENDAR-COLOR:%@\r\n", hexColor];
        }
    }
    [ics appendString:@"END:VEVENT\r\n"];
    return ics;
}

static NSString *
macos_reminder_ics_component (EKReminder *reminder)
{
    NSString *uid = reminder.calendarItemIdentifier;
    NSMutableString *ics;

    if (uid.length == 0)
        return nil;
    ics = [NSMutableString stringWithString:@"BEGIN:VTODO\r\n"];
    [ics appendFormat:@"UID:%@\r\n", escape_ics_text (uid)];
    [ics appendFormat:@"SUMMARY:%@\r\n", escape_ics_text (reminder.title ?: @"")];
    if (reminder.notes.length > 0)
        [ics appendFormat:@"DESCRIPTION:%@\r\n", escape_ics_text (reminder.notes)];
    if (reminder.dueDateComponents) {
        NSDate *due_date = [[NSCalendar currentCalendar]
            dateFromComponents:reminder.dueDateComponents];
        if (due_date)
            [ics appendFormat:@"DUE:%@\r\n", format_date_ics (due_date, NO)];
    }
    if (reminder.completed) {
        [ics appendString:@"STATUS:COMPLETED\r\n"];
        if (reminder.completionDate)
            [ics appendFormat:@"COMPLETED:%@\r\n", format_date_ics (reminder.completionDate, NO)];
    } else {
        [ics appendString:@"STATUS:NEEDS-ACTION\r\n"];
    }
    if (reminder.calendar && reminder.calendar.title.length > 0) {
        [ics appendFormat:@"CATEGORIES:%@\r\n", escape_ics_text (reminder.calendar.title)];
        [ics appendFormat:@"X-EVOLUTION-SOURCE-NAME:%@\r\n", escape_ics_text (reminder.calendar.title)];
        NSString *accUid = reminder.calendar.source.sourceIdentifier ?: @"";
        NSString *srcUid = [NSString stringWithFormat:@"macos:reminders:%lu:%@:%@",
            (unsigned long)accUid.length,
            accUid,
            reminder.calendar.calendarIdentifier ?: @""];
        [ics appendFormat:@"X-EVOLUTION-SOURCE-UID:%@\r\n", escape_ics_text (srcUid)];
        NSString *hexColor = hex_color_from_cgcolor(reminder.calendar.CGColor);
        if (hexColor && hexColor.length > 0) {
            [ics appendFormat:@"COLOR:%@\r\n", hexColor];
            [ics appendFormat:@"X-APPLE-CALENDAR-COLOR:%@\r\n", hexColor];
        }
    }
    [ics appendString:@"END:VTODO\r\n"];
    return ics;
}

static NSArray<EKReminder *> *
macos_fetch_reminders_bounded (EKEventStore *store,
                               EKCalendar *calendar,
                               NSMutableArray<NSString *> *errors)
{
    MacosReminderFetchState *state;
    NSPredicate *predicate;
    long result;

    if ([NSThread isMainThread]) {
        macos_append_error (errors,
            @"Reminder data is loaded only from a worker thread to avoid blocking the UI.");
        return nil;
    }
    predicate = [store predicateForRemindersInCalendars:@[ calendar ]];
    if (!predicate) {
        macos_append_error (errors, @"EventKit could not create a reminder query.");
        return nil;
    }
    state = [[MacosReminderFetchState alloc] init];
    state->semaphore = dispatch_semaphore_create (0);
    [store fetchRemindersMatchingPredicate:predicate
                                completion:^(NSArray<EKReminder *> *items) {
        state->reminders = items ?: @[];
        dispatch_semaphore_signal (state->semaphore);
    }];
    result = dispatch_semaphore_wait (
        state->semaphore,
        dispatch_time (DISPATCH_TIME_NOW, 5 * NSEC_PER_SEC));
    if (result != 0) {
        macos_append_error (errors,
            [NSString stringWithFormat:@"Reminder query for %@ timed out after 5 seconds.",
                calendar.title ?: @"this list"]);
        return nil;
    }
    return state->reminders ?: @[];
}

static void
macos_append_event_components (EKEventStore *store,
                               NSArray<EKCalendar *> *calendars,
                               NSMutableArray<NSDictionary *> *components,
                               NSMutableArray<NSString *> *errors)
{
    NSDate *now = [NSDate date];
    NSDate *start_date = [now dateByAddingTimeInterval:-15552000.0];
    NSDate *end_date = [now dateByAddingTimeInterval:31536000.0];

    for (EKCalendar *calendar in calendars) {
        NSPredicate *predicate = [store predicateForEventsWithStartDate:start_date
                                                                   endDate:end_date
                                                                 calendars:@[ calendar ]];
        NSArray<EKEvent *> *events = predicate ? [store eventsMatchingPredicate:predicate] : nil;
        if (!predicate) {
            macos_append_error (errors,
                [NSString stringWithFormat:@"EventKit could not query calendar %@.",
                    calendar.title ?: @"with no title"]);
            continue;
        }
        for (EKEvent *event in events) {
            NSString *ics = macos_event_ics_component (event);
            if (!ics) {
                macos_append_error (errors,
                    [NSString stringWithFormat:@"Skipped an event without a stable identifier in %@.",
                        calendar.title ?: @"a calendar"]);
                continue;
            }
            [components addObject:@{
                @"source_uid": calendar.calendarIdentifier ?: @"",
                @"kind": @"calendar",
                @"data": ics,
            }];
        }
    }
}

static void
macos_append_reminder_components (EKEventStore *store,
                                  NSArray<EKCalendar *> *calendars,
                                  NSMutableArray<NSDictionary *> *components,
                                  NSMutableArray<NSString *> *errors)
{
    for (EKCalendar *calendar in calendars) {
        NSArray<EKReminder *> *reminders = macos_fetch_reminders_bounded (
            store, calendar, errors);
        for (EKReminder *reminder in reminders) {
            NSString *ics = macos_reminder_ics_component (reminder);
            if (!ics) {
                macos_append_error (errors,
                    [NSString stringWithFormat:@"Skipped a reminder without a stable identifier in %@.",
                        calendar.title ?: @"a list"]);
                continue;
            }
            [components addObject:@{
                @"source_uid": calendar.calendarIdentifier ?: @"",
                @"kind": @"reminders",
                @"data": ics,
            }];
        }
    }
}

static void
macos_append_contact_components (CNContactStore *store,
                                 NSArray<CNContainer *> *containers,
                                 NSMutableArray<NSDictionary *> *components,
                                 NSMutableArray<NSString *> *errors)
{
    NSArray<id<CNKeyDescriptor>> *keys = @[
        [CNContactVCardSerialization descriptorForRequiredKeys]
    ];

    for (CNContainer *container in containers) {
        NSError *enumeration_error = nil;
        CNContactFetchRequest *request = [[CNContactFetchRequest alloc]
            initWithKeysToFetch:keys];
        request.predicate = [CNContact predicateForContactsInContainerWithIdentifier:
            container.identifier];
        request.unifyResults = NO;
        [store enumerateContactsWithFetchRequest:request
                                           error:&enumeration_error
                                      usingBlock:^(CNContact *contact, BOOL *stop) {
            NSError *vcard_error = nil;
            NSData *data = [CNContactVCardSerialization dataWithContacts:@[ contact ]
                                                                      error:&vcard_error];
            NSString *vcard = data
                ? [[NSString alloc] initWithData:data encoding:NSUTF8StringEncoding] : nil;
            (void) stop;
            if (vcard.length > 0) {
                [components addObject:@{
                    @"source_uid": container.identifier ?: @"",
                    @"kind": @"contacts",
                    @"data": vcard,
                }];
            } else {
                macos_append_error (errors,
                    [NSString stringWithFormat:@"Could not serialize a contact in %@: %@",
                        container.name ?: @"a contacts account",
                        vcard_error.localizedDescription ?: @"unknown error"]);
            }
        }];
        if (enumeration_error) {
            macos_append_error (errors,
                [NSString stringWithFormat:@"Could not load contacts from %@: %@",
                    container.name ?: @"a contacts account",
                    enumeration_error.localizedDescription]);
        }
    }
}

static char *
macos_system_sources_json (BOOL include_components)
{
    NSMutableArray<NSDictionary *> *sources = [NSMutableArray array];
    NSMutableArray<NSDictionary *> *components = [NSMutableArray array];
    NSMutableArray<NSString *> *errors = [NSMutableArray array];
    EKEventStore *event_store = macos_shared_event_store ();
    CNContactStore *contact_store = macos_shared_contact_store ();
    NSArray<EKCalendar *> *event_calendars = [event_store calendarsForEntityType:EKEntityTypeEvent] ?: @[];
    NSArray<EKCalendar *> *reminder_calendars = [event_store calendarsForEntityType:EKEntityTypeReminder] ?: @[];
    NSError *container_error = nil;
    NSArray<CNContainer *> *containers = [contact_store containersMatchingPredicate:nil
                                                                                error:&container_error] ?: @[];
    NSDictionary *payload;
    NSError *json_error = nil;
    NSData *json_data;
    NSString *json;

    for (EKCalendar *calendar in event_calendars)
        [sources addObject:macos_eventkit_source_dictionary (calendar, @"calendar")];
    for (EKCalendar *calendar in reminder_calendars)
        [sources addObject:macos_eventkit_source_dictionary (calendar, @"reminders")];
    for (CNContainer *container in containers)
        [sources addObject:macos_contacts_source_dictionary (container)];

    if (container_error) {
        macos_append_error (errors,
            [NSString stringWithFormat:@"Could not discover Contacts containers: %@",
                container_error.localizedDescription]);
    }

    if (include_components) {
        if (macos_eventkit_has_read_access (EKEntityTypeEvent)) {
            macos_append_event_components (event_store, event_calendars, components, errors);
        } else {
            macos_append_error (errors,
                macos_eventkit_read_access_error (@"Calendar", EKEntityTypeEvent));
        }

        if (macos_eventkit_has_read_access (EKEntityTypeReminder))
            macos_append_reminder_components (event_store, reminder_calendars, components, errors);
        else
            macos_append_error (errors,
                macos_eventkit_read_access_error (@"Reminders", EKEntityTypeReminder));

        if (macos_contacts_has_read_access ())
            macos_append_contact_components (contact_store, containers, components, errors);
        else
            macos_append_error (errors, macos_contacts_read_access_error ());
    }

    payload = @{
        @"version": @1,
        @"sources": sources,
        @"components": components,
        @"errors": errors,
    };
    json_data = [NSJSONSerialization dataWithJSONObject:payload options:0 error:&json_error];
    json = json_data ? [[NSString alloc] initWithData:json_data encoding:NSUTF8StringEncoding] : nil;
    if (!json) {
        return strdup("{\"version\":1,\"sources\":[],\"components\":[],\"errors\":[\"Could not encode macOS system sources JSON.\"]}");
    }
    return strdup (json.UTF8String);
}

char *
macos_list_system_sources_json (void)
{
    @autoreleasepool {
        return macos_system_sources_json (NO);
    }
}

char *
macos_load_system_data_json (void)
{
    @autoreleasepool {
        return macos_system_sources_json (YES);
    }
}

void
macos_open_internet_accounts (void)
{
    @autoreleasepool {
        NSURL *url = [NSURL URLWithString:
            @"x-apple.systempreferences:com.apple.Internet-Accounts-Settings.extension"];
        if (url)
            [[NSWorkspace sharedWorkspace] openURL:url];
    }
}

void macos_open_privacy_settings(const char *pane) {
    @autoreleasepool {
        NSString *urlString = @"x-apple.systempreferences:com.apple.preference.security?Privacy_Calendars";
        if (pane) {
            NSString *paneStr = [NSString stringWithUTF8String:pane];
            if ([paneStr isEqualToString:@"Privacy_Contacts"]) {
                urlString = @"x-apple.systempreferences:com.apple.preference.security?Privacy_Contacts";
            } else if ([paneStr isEqualToString:@"Privacy_Reminders"]) {
                urlString = @"x-apple.systempreferences:com.apple.preference.security?Privacy_Reminders";
            } else if ([paneStr isEqualToString:@"Privacy_Calendars"]) {
                urlString = @"x-apple.systempreferences:com.apple.preference.security?Privacy_Calendars";
            }
        }
        NSURL *url = [NSURL URLWithString:urlString];
        if (url) {
            [[NSWorkspace sharedWorkspace] openURL:url];
        }
    }
}

void macos_free_string(char *ptr) {
    if (ptr) {
        free(ptr);
    }
}

static NSString * const kEvolutionDefaultKeychainService = @"com.martingawron.Meridian";

int
macos_keychain_set_password(const char *service, const char *account, const char *password)
{
    if (!account || !password) {
        return -1;
    }

    @autoreleasepool {
        NSString *serviceStr = (service && strlen(service) > 0) ? [NSString stringWithUTF8String:service] : kEvolutionDefaultKeychainService;
        NSString *accountStr = [NSString stringWithUTF8String:account];
        NSData *passwordData = [[NSString stringWithUTF8String:password] dataUsingEncoding:NSUTF8StringEncoding];

        if (!serviceStr || !accountStr || !passwordData) {
            return -1;
        }

        NSDictionary *query = @{
            (__bridge id)kSecClass: (__bridge id)kSecClassGenericPassword,
            (__bridge id)kSecAttrService: serviceStr,
            (__bridge id)kSecAttrAccount: accountStr,
        };

        NSDictionary *attributesToUpdate = @{
            (__bridge id)kSecValueData: passwordData,
        };

        OSStatus status = SecItemUpdate((__bridge CFDictionaryRef)query, (__bridge CFDictionaryRef)attributesToUpdate);
        if (status == errSecItemNotFound) {
            NSMutableDictionary *attributesToAdd = [query mutableCopy];
            attributesToAdd[(__bridge id)kSecValueData] = passwordData;
            status = SecItemAdd((__bridge CFDictionaryRef)attributesToAdd, NULL);
        }

        return (status == errSecSuccess) ? 0 : (int)status;
    }
}

char *
macos_keychain_get_password(const char *service, const char *account)
{
    if (!account) {
        return NULL;
    }

    @autoreleasepool {
        NSString *serviceStr = (service && strlen(service) > 0) ? [NSString stringWithUTF8String:service] : kEvolutionDefaultKeychainService;
        NSString *accountStr = [NSString stringWithUTF8String:account];

        if (!serviceStr || !accountStr) {
            return NULL;
        }

        NSDictionary *query = @{
            (__bridge id)kSecClass: (__bridge id)kSecClassGenericPassword,
            (__bridge id)kSecAttrService: serviceStr,
            (__bridge id)kSecAttrAccount: accountStr,
            (__bridge id)kSecReturnData: (__bridge id)kCFBooleanTrue,
            (__bridge id)kSecMatchLimit: (__bridge id)kSecMatchLimitOne,
        };

        CFTypeRef result = NULL;
        OSStatus status = SecItemCopyMatching((__bridge CFDictionaryRef)query, &result);
        if (status != errSecSuccess || !result) {
            return NULL;
        }

        NSData *data = (__bridge_transfer NSData *)result;
        NSString *pwStr = [[NSString alloc] initWithData:data encoding:NSUTF8StringEncoding];
        if (!pwStr) {
            return NULL;
        }

        return strdup(pwStr.UTF8String);
    }
}

int
macos_keychain_delete_password(const char *service, const char *account)
{
    if (!account) {
        return -1;
    }

    @autoreleasepool {
        NSString *serviceStr = (service && strlen(service) > 0) ? [NSString stringWithUTF8String:service] : kEvolutionDefaultKeychainService;
        NSString *accountStr = [NSString stringWithUTF8String:account];

        if (!serviceStr || !accountStr) {
            return -1;
        }

        NSDictionary *query = @{
            (__bridge id)kSecClass: (__bridge id)kSecClassGenericPassword,
            (__bridge id)kSecAttrService: serviceStr,
            (__bridge id)kSecAttrAccount: accountStr,
        };

        OSStatus status = SecItemDelete((__bridge CFDictionaryRef)query);
        if (status == errSecSuccess || status == errSecItemNotFound) {
            return 0;
        }
        return (int)status;
    }
}

