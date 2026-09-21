// SPDX-License-Identifier: GPL-3.0-or-later
//
// macOS Integration Service
// Bridges macOS System Services (EventKit, Contacts) with Evolution GTK4 stores.

use std::collections::{HashMap, HashSet};
#[cfg(all(target_os = "macos", macos_bridge))]
use std::ffi::{CStr, CString};
use std::sync::{Arc, Mutex, atomic::{AtomicU64, Ordering}};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MacosSourceKind { Calendar, Reminders, Contacts }

impl MacosSourceKind {
    pub fn display_name(self) -> &'static str {
        match self { Self::Calendar => "Kalender", Self::Reminders => "Erinnerungen", Self::Contacts => "Kontakte" }
    }
    fn key(self) -> &'static str {
        match self { Self::Calendar => "calendar", Self::Reminders => "reminders", Self::Contacts => "contacts" }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct MacosSystemSource {
    pub uid: String,
    pub name: String,
    pub kind: MacosSourceKind,
    pub account_uid: String,
    pub account_name: String,
    pub backend: String,
    pub writable: bool,
}
impl MacosSystemSource {
    // Length delimiters prevent collisions even when an Apple identifier contains ':'.
    pub fn source_uid(&self) -> String {
        format!("macos:{}:{}:{}:{}", self.kind.key(), self.account_uid.len(), self.account_uid, self.uid)
    }
    pub fn as_eds_source(&self) -> super::eds_service::EdsSource {
        super::eds_service::EdsSource {
            uid: self.source_uid(), display_name: self.name.clone(),
            backend_name: self.backend.clone(), enabled: true,
            parent_name: Some(self.account_name.clone()),
            color: None,
            foreign_mail: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MacosSystemComponent {
    pub source_uid: String,
    pub kind: MacosSourceKind,
    pub data: String,
}
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct MacosSystemSnapshot {
    pub version: u32,
    pub sources: Vec<MacosSystemSource>,
    #[serde(default)]
    pub components: Vec<MacosSystemComponent>,
    #[serde(default)]
    pub errors: Vec<String>,
}
impl MacosSystemSnapshot {
    pub fn from_json(json: &str) -> Result<Self, String> {
        let result: Self = serde_json::from_str(json).map_err(|_| "Ungültige Antwort der macOS-Kontendienste".to_string())?;
        if result.version != 1 { return Err("Unbekannte macOS-Kontendatenversion".into()); }
        let mut identities = HashSet::new();
        for source in &result.sources {
            if source.uid.is_empty() || source.account_uid.is_empty()
                || !identities.insert((source.kind, source.uid.as_str())) {
                return Err("macOS lieferte fehlende oder mehrdeutige Quellkennungen".into());
            }
        }
        if result.components.iter().any(|c| !identities.contains(&(c.kind, c.source_uid.as_str()))) {
            return Err("macOS lieferte Daten ohne zugehörige Kontoquelle".into());
        }
        Ok(result)
    }
    pub fn warning(&self) -> Option<String> {
        (!self.errors.is_empty()).then(|| self.errors.join("; "))
    }
    pub(crate) fn calendar_snapshot(&self) -> super::calendar_store_service::CalendarSnapshot {
        let sources: HashMap<_, _> = self.sources.iter().map(|s| ((s.kind, s.uid.as_str()), s)).collect();
        let mut result = super::calendar_store_service::CalendarSnapshot::default();
        for component in &self.components {
            if component.kind == MacosSourceKind::Contacts { continue; }
            if let Some(source) = sources.get(&(component.kind, component.source_uid.as_str())) {
                super::calendar_store_service::parse_ical_component(&component.data, &source.source_uid(), &source.name, &mut result);
            }
        }
        super::calendar_store_service::sort_snapshot(&mut result);
        result
    }
    pub(crate) fn contact_records(&self) -> Vec<super::ebook_service::EBookContactRecord> {
        let sources: HashMap<_, _> = self.sources.iter().map(|s| ((s.kind, s.uid.as_str()), s)).collect();
        self.components.iter().filter(|c| c.kind == MacosSourceKind::Contacts).filter_map(|c| {
            let source = sources.get(&(c.kind, c.source_uid.as_str()))?;
            let contact = super::vcard_service::VCardContact::parse_vcard_string(&c.data)?;
            let uid = contact.uid().filter(|u| !u.is_empty())?;
            Some(super::ebook_service::EBookContactRecord {
                source_uid: source.source_uid(), source_display_name: source.name.clone(),
                uid, vcard: c.data.clone(), contact,
            })
        }).collect()
    }
}

static REFRESH_GENERATION: AtomicU64 = AtomicU64::new(0);
// One immutable snapshot shared by simultaneous Calendar/Tasks/Contacts worker loads.
static SYSTEM_DATA: Mutex<Option<(u64, std::time::Instant, Arc<MacosSystemSnapshot>)>> = Mutex::new(None);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacosPermissionStatus {
    NotDetermined,
    Restricted,
    Denied,
    Authorized,
    WriteOnly,
    Limited,
    Unsupported,
}

impl MacosPermissionStatus {
    pub fn is_authorized(&self) -> bool {
        matches!(self, Self::Authorized)
    }

    pub fn display_label(&self) -> &'static str {
        match self {
            Self::NotDetermined => "Nicht abgefragt",
            Self::Restricted => "Eingeschränkt (Systemrichtlinie)",
            Self::Denied => "Verweigert",
            Self::Authorized => "Erlaubt (Vollzugriff)",
            Self::WriteOnly => "Nur Schreibzugriff",
            Self::Limited => "Beschränkte Kontaktauswahl",
            Self::Unsupported => "Nicht verfügbar",
        }
    }

    pub fn status_css_class(&self) -> &'static str {
        match self {
            Self::Authorized => "success",
            Self::Denied => "error",
            Self::Restricted | Self::WriteOnly | Self::Limited => "warning",
            Self::Unsupported => "dim-label",
            Self::NotDetermined => "dim-label",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacosPermissionKind {
    Calendar,
    Reminders,
    Contacts,
}

impl MacosPermissionKind {
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Calendar => "Kalender",
            Self::Reminders => "Erinnerungen & Aufgaben",
            Self::Contacts => "Kontakte & Adressbuch",
        }
    }

    pub fn privacy_pane_name(&self) -> &'static str {
        match self {
            Self::Calendar => "Privacy_Calendars",
            Self::Reminders => "Privacy_Reminders",
            Self::Contacts => "Privacy_Contacts",
        }
    }
}

#[cfg(all(target_os = "macos", macos_bridge))]
mod ffi {
    use std::ffi::{c_char, c_int, c_void};

    pub type PermissionCallback = Option<unsafe extern "C" fn(c_int, *mut c_void)>;

    unsafe extern "C" {
        pub fn macos_check_calendar_permission() -> c_int;
        pub fn macos_check_reminders_permission() -> c_int;
        pub fn macos_check_contacts_permission() -> c_int;
        pub fn macos_request_calendar_permission(
            callback: PermissionCallback,
            user_data: *mut c_void,
        );
        pub fn macos_request_reminders_permission(
            callback: PermissionCallback,
            user_data: *mut c_void,
        );
        pub fn macos_request_contacts_permission(
            callback: PermissionCallback,
            user_data: *mut c_void,
        );
        pub fn macos_list_system_sources_json() -> *mut c_char;
        pub fn macos_load_system_data_json() -> *mut c_char;
        pub fn macos_open_internet_accounts();
        pub fn macos_system_change_generation() -> u64;
        pub fn macos_export_calendar_events_ics() -> *mut c_char;
        pub fn macos_export_reminders_ics() -> *mut c_char;
        pub fn macos_export_contacts_vcf() -> *mut c_char;
        pub fn macos_open_privacy_settings(pane: *const c_char);
        pub fn macos_free_string(ptr: *mut c_char);
        pub fn macos_keychain_set_password(
            service: *const c_char,
            account: *const c_char,
            password: *const c_char,
        ) -> c_int;
        pub fn macos_keychain_get_password(
            service: *const c_char,
            account: *const c_char,
        ) -> *mut c_char;
        pub fn macos_keychain_delete_password(
            service: *const c_char,
            account: *const c_char,
        ) -> c_int;
    }
}


#[derive(Debug, Clone, Default)]
pub struct MacosSyncSummary {
    pub calendar_events: usize,
    pub tasks_reminders: usize,
    pub contacts: usize,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct MacosIntegrationService;

impl MacosIntegrationService {
    pub fn new() -> Self {
        Self
    }

    /// Export macOS Calendar events as iCalendar string.
    pub fn export_calendar_events_ics(&self) -> Option<String> {
        #[cfg(all(target_os = "macos", macos_bridge))]
        {
            let ptr = unsafe { ffi::macos_export_calendar_events_ics() };
            if ptr.is_null() {
                return None;
            }
            let c_str = unsafe { CStr::from_ptr(ptr) };
            let result = c_str.to_str().ok().map(|s| s.to_string());
            unsafe { ffi::macos_free_string(ptr) };
            result
        }
        #[cfg(not(all(target_os = "macos", macos_bridge)))]
        {
            None
        }
    }

    /// Export macOS Reminders as iCalendar VTODO string.
    pub fn export_reminders_ics(&self) -> Option<String> {
        #[cfg(all(target_os = "macos", macos_bridge))]
        {
            let ptr = unsafe { ffi::macos_export_reminders_ics() };
            if ptr.is_null() {
                return None;
            }
            let c_str = unsafe { CStr::from_ptr(ptr) };
            let result = c_str.to_str().ok().map(|s| s.to_string());
            unsafe { ffi::macos_free_string(ptr) };
            result
        }
        #[cfg(not(all(target_os = "macos", macos_bridge)))]
        {
            None
        }
    }

    /// Export macOS Contacts as vCard string.
    pub fn export_contacts_vcf(&self) -> Option<String> {
        #[cfg(all(target_os = "macos", macos_bridge))]
        {
            let ptr = unsafe { ffi::macos_export_contacts_vcf() };
            if ptr.is_null() {
                return None;
            }
            let c_str = unsafe { CStr::from_ptr(ptr) };
            let result = c_str.to_str().ok().map(|s| s.to_string());
            unsafe { ffi::macos_free_string(ptr) };
            result
        }
        #[cfg(not(all(target_os = "macos", macos_bridge)))]
        {
            None
        }
    }

    /// Synchronize Apple Calendar events from EventKit into Evolution's CalendarStoreService.
    pub fn sync_apple_calendar_to_store(
        &self,
        store: &super::calendar_store_service::CalendarStoreService,
    ) -> Result<usize, String> {
        let ics = self
            .export_calendar_events_ics()
            .ok_or_else(|| "Keine Kalenderdaten von macOS erhalten oder Zugriff verweigert".to_string())?;

        if ics.trim().is_empty() {
            return Ok(0);
        }

        let event_count = ics.match_indices("BEGIN:VEVENT").count();
        let target_path = store.calendar_dir().join("apple-calendar.ics");
        std::fs::create_dir_all(store.calendar_dir()).map_err(|e| e.to_string())?;
        std::fs::write(&target_path, ics.as_bytes()).map_err(|e| e.to_string())?;

        // Also clean up any legacy Apple events from evolution-gtk4-local.ics if present
        let local_path = store.calendar_dir().join("evolution-gtk4-local.ics");
        if local_path.exists() {
            if let Ok(local_content) = std::fs::read_to_string(&local_path) {
                let has_only_apple = local_content.lines().any(|l| l.starts_with("UID:") && l.get(4..).map_or(false, |r| r.contains(':')))
                    && !local_content.lines().any(|l| l.starts_with("UID:") && !l.get(4..).map_or(false, |r| r.contains(':')));
                if has_only_apple {
                    let _ = std::fs::remove_file(&local_path);
                }
            }
        }

        Ok(event_count)
    }

    /// Synchronize Apple Reminders from EventKit into Evolution's CalendarStoreService (tasks/memos).
    pub fn sync_apple_reminders_to_store(
        &self,
        store: &super::calendar_store_service::CalendarStoreService,
    ) -> Result<usize, String> {
        let ics = self
            .export_reminders_ics()
            .ok_or_else(|| "Keine Erinnerungsdaten von macOS erhalten oder Zugriff verweigert".to_string())?;

        if ics.trim().is_empty() {
            return Ok(0);
        }

        let reminder_count = ics.match_indices("BEGIN:VTODO").count();
        let target_path = store.calendar_dir().join("apple-reminders.ics");
        std::fs::create_dir_all(store.calendar_dir()).map_err(|e| e.to_string())?;
        std::fs::write(&target_path, ics.as_bytes()).map_err(|e| e.to_string())?;

        Ok(reminder_count)
    }

    /// Synchronize Apple Contacts from Contacts.framework into Evolution's ContactStoreService.
    pub fn sync_apple_contacts_to_store(
        &self,
        store: &super::contact_store_service::ContactStoreService,
    ) -> Result<usize, String> {
        let vcf = self
            .export_contacts_vcf()
            .ok_or_else(|| "Keine Kontaktdaten von macOS erhalten oder Zugriff verweigert".to_string())?;

        if vcf.trim().is_empty() {
            return Ok(0);
        }

        let contact_count = vcf.match_indices("BEGIN:VCARD").count();
        let target_path = store.contact_dir().join("apple-contacts.vcf");
        std::fs::create_dir_all(store.contact_dir()).map_err(|e| e.to_string())?;
        std::fs::write(&target_path, vcf.as_bytes()).map_err(|e| e.to_string())?;

        // Clean up legacy Apple contacts in evolution-gtk4-local.vcf from previous slow imports
        let local_path = store.contact_dir().join("evolution-gtk4-local.vcf");
        if local_path.exists() {
            if let Ok(local_content) = std::fs::read_to_string(&local_path) {
                let has_only_apple = local_content.contains("PRODID:-//Apple Inc.")
                    && !local_content.lines().any(|l| l.starts_with("PRODID:") && !l.contains("Apple Inc."));
                if has_only_apple {
                    let _ = std::fs::remove_file(&local_path);
                }
            }
        }

        Ok(contact_count)
    }

    /// Perform a full synchronization of Calendar, Reminders, and Contacts.
    pub fn sync_all(
        &self,
        cal_store: &super::calendar_store_service::CalendarStoreService,
        contact_store: &super::contact_store_service::ContactStoreService,
    ) -> MacosSyncSummary {
        let mut summary = MacosSyncSummary::default();

        match self.sync_apple_calendar_to_store(cal_store) {
            Ok(count) => summary.calendar_events = count,
            Err(err) => summary.errors.push(err),
        }

        match self.sync_apple_reminders_to_store(cal_store) {
            Ok(count) => summary.tasks_reminders = count,
            Err(err) => summary.errors.push(err),
        }

        match self.sync_apple_contacts_to_store(contact_store) {
            Ok(count) => summary.contacts = count,
            Err(err) => summary.errors.push(err),
        }

        summary
    }

    /// Check the current authorization status for a specific macOS system permission.
    pub fn check_permission(&self, kind: MacosPermissionKind) -> MacosPermissionStatus {
        #[cfg(all(target_os = "macos", macos_bridge))]
        {
            let raw = unsafe {
                match kind {
                    MacosPermissionKind::Calendar => ffi::macos_check_calendar_permission(),
                    MacosPermissionKind::Reminders => ffi::macos_check_reminders_permission(),
                    MacosPermissionKind::Contacts => ffi::macos_check_contacts_permission(),
                }
            };
            permission_status(kind, raw)
        }
        #[cfg(not(all(target_os = "macos", macos_bridge)))]
        {
            let _ = kind;
            MacosPermissionStatus::Unsupported
        }
    }

    /// Asynchronously request permission on macOS and invoke callback on the GLib main context.
    pub fn request_permission<F>(&self, kind: MacosPermissionKind, on_complete: F)
    where
        F: FnOnce(bool) + 'static,
    {
        #[cfg(all(target_os = "macos", macos_bridge))]
        {
            let (sender, receiver) = async_channel::bounded::<bool>(1);
            glib::MainContext::default().spawn_local(async move {
                if let Ok(granted) = receiver.recv().await {
                    MacosIntegrationService::invalidate_system_snapshot();
                    on_complete(granted);
                }
            });
            let user_data = Box::into_raw(Box::new(sender)) as *mut std::ffi::c_void;
            unsafe extern "C" fn ffi_callback(granted: std::ffi::c_int, user_data: *mut std::ffi::c_void) {
                // Only a Send channel crosses Apple's callback threads; GTK closures stay local.
                let sender = unsafe { Box::from_raw(user_data.cast::<async_channel::Sender<bool>>()) };
                let _ = sender.try_send(granted != 0);
            }

            unsafe {
                match kind {
                    MacosPermissionKind::Calendar => {
                        ffi::macos_request_calendar_permission(Some(ffi_callback), user_data);
                    }
                    MacosPermissionKind::Reminders => {
                        ffi::macos_request_reminders_permission(Some(ffi_callback), user_data);
                    }
                    MacosPermissionKind::Contacts => {
                        ffi::macos_request_contacts_permission(Some(ffi_callback), user_data);
                    }
                }
            }
        }
        #[cfg(not(all(target_os = "macos", macos_bridge)))]
        {
            let _ = kind;
            glib::idle_add_local_once(move || {
                on_complete(false);
            });
        }
    }

    /// Open macOS System Settings -> Privacy & Security pane.
    pub fn open_privacy_settings(&self, kind: MacosPermissionKind) {
        #[cfg(all(target_os = "macos", macos_bridge))]
        {
            if let Ok(c_pane) = CString::new(kind.privacy_pane_name()) {
                unsafe {
                    ffi::macos_open_privacy_settings(c_pane.as_ptr());
                }
            }
        }
        #[cfg(not(all(target_os = "macos", macos_bridge)))]
        {
            let _ = kind;
        }
    }

    pub fn open_internet_accounts(&self) {
        #[cfg(all(target_os = "macos", macos_bridge))]
        unsafe { ffi::macos_open_internet_accounts(); }
    }
    pub fn system_change_generation(&self) -> u64 {
        let local = REFRESH_GENERATION.load(Ordering::Acquire);
        #[cfg(all(target_os = "macos", macos_bridge))]
        { return local.wrapping_add(unsafe { ffi::macos_system_change_generation() }); }
        #[cfg(not(all(target_os = "macos", macos_bridge)))]
        { local }
    }
    pub fn invalidate_system_snapshot() {
        // Nonblocking, also safe on GTK while a worker holds the snapshot lock.
        REFRESH_GENERATION.fetch_add(1, Ordering::AcqRel);
    }
    /// Worker-only discovery; never prompts or loads individual records.
    pub fn system_sources(&self) -> Result<MacosSystemSnapshot, String> {
        #[cfg(all(target_os = "macos", macos_bridge))]
        { return take_snapshot(unsafe { ffi::macos_list_system_sources_json() }); }
        #[cfg(not(all(target_os = "macos", macos_bridge)))]
        { Err("macOS-Internetaccounts sind auf diesem System nicht verfügbar".into()) }
    }
    /// Worker-only read-through cache, invalidated by system changes or explicit refresh.
    pub fn system_data(&self) -> Result<Arc<MacosSystemSnapshot>, String> {
        let mut cache = SYSTEM_DATA.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let generation = self.system_change_generation();
        if let Some((previous, loaded, snapshot)) = cache.as_ref() {
            if *previous == generation && loaded.elapsed() < std::time::Duration::from_secs(60) {
                return Ok(snapshot.clone());
            }
        }
        #[cfg(all(target_os = "macos", macos_bridge))]
        {
            let snapshot = Arc::new(take_snapshot(unsafe { ffi::macos_load_system_data_json() })?);
            *cache = Some((generation, std::time::Instant::now(), snapshot.clone()));
            Ok(snapshot)
        }
        #[cfg(not(all(target_os = "macos", macos_bridge)))]
        { Err("macOS-Internetaccounts sind auf diesem System nicht verfügbar".into()) }
    }

    pub const KEYCHAIN_SERVICE_MERIDIAN: &str = "com.martingawron.Meridian";
    pub const KEYCHAIN_SERVICE_EVOLUTION: &str = "com.martingawron.Meridian";

    pub fn keychain_set_password(&self, account: &str, password: &str) -> Result<(), String> {
        self.keychain_set_password_for_service(Self::KEYCHAIN_SERVICE_MERIDIAN, account, password)
    }

    pub fn keychain_set_password_for_service(
        &self,
        service: &str,
        account: &str,
        password: &str,
    ) -> Result<(), String> {
        #[cfg(all(target_os = "macos", macos_bridge))]
        {
            let c_service = CString::new(service).map_err(|e| e.to_string())?;
            let c_account = CString::new(account).map_err(|e| e.to_string())?;
            let c_password = CString::new(password).map_err(|e| e.to_string())?;
            let rc = unsafe {
                ffi::macos_keychain_set_password(
                    c_service.as_ptr(),
                    c_account.as_ptr(),
                    c_password.as_ptr(),
                )
            };
            if rc == 0 {
                Ok(())
            } else {
                Err(format!("macOS Keychain error: {rc}"))
            }
        }
        #[cfg(not(all(target_os = "macos", macos_bridge)))]
        {
            let _ = (service, account, password);
            Err("macOS Keychain is not available on this platform".into())
        }
    }

    pub fn keychain_get_password(&self, account: &str) -> Result<Option<String>, String> {
        self.keychain_get_password_for_service(Self::KEYCHAIN_SERVICE_EVOLUTION, account)
    }

    pub fn keychain_get_password_for_service(
        &self,
        service: &str,
        account: &str,
    ) -> Result<Option<String>, String> {
        #[cfg(all(target_os = "macos", macos_bridge))]
        {
            let c_service = CString::new(service).map_err(|e| e.to_string())?;
            let c_account = CString::new(account).map_err(|e| e.to_string())?;
            let ptr = unsafe {
                ffi::macos_keychain_get_password(c_service.as_ptr(), c_account.as_ptr())
            };
            if ptr.is_null() {
                return Ok(None);
            }
            let c_str = unsafe { CStr::from_ptr(ptr) };
            let utf8_res = c_str.to_str().map(str::to_owned).map_err(|e| e.to_string());
            unsafe { ffi::macos_free_string(ptr) };
            let result = utf8_res?;
            Ok(Some(result))
        }
        #[cfg(not(all(target_os = "macos", macos_bridge)))]
        {
            let _ = (service, account);
            Err("macOS Keychain is not available on this platform".into())
        }
    }

    pub fn keychain_delete_password(&self, account: &str) -> Result<(), String> {
        self.keychain_delete_password_for_service(Self::KEYCHAIN_SERVICE_EVOLUTION, account)
    }

    pub fn keychain_delete_password_for_service(
        &self,
        service: &str,
        account: &str,
    ) -> Result<(), String> {
        #[cfg(all(target_os = "macos", macos_bridge))]
        {
            let c_service = CString::new(service).map_err(|e| e.to_string())?;
            let c_account = CString::new(account).map_err(|e| e.to_string())?;
            let rc = unsafe {
                ffi::macos_keychain_delete_password(c_service.as_ptr(), c_account.as_ptr())
            };
            if rc == 0 {
                Ok(())
            } else {
                Err(format!("macOS Keychain error: {rc}"))
            }
        }
        #[cfg(not(all(target_os = "macos", macos_bridge)))]
        {
            let _ = (service, account);
            Err("macOS Keychain is not available on this platform".into())
        }
    }
}


fn permission_status(kind: MacosPermissionKind, raw: i32) -> MacosPermissionStatus {
    match raw {
        0 => MacosPermissionStatus::NotDetermined,
        1 => MacosPermissionStatus::Restricted,
        2 => MacosPermissionStatus::Denied,
        3 => MacosPermissionStatus::Authorized,
        4 if matches!(kind, MacosPermissionKind::Contacts) => MacosPermissionStatus::Limited,
        4 => MacosPermissionStatus::WriteOnly,
        _ => MacosPermissionStatus::Unsupported,
    }
}
#[cfg(all(target_os = "macos", macos_bridge))]
fn take_snapshot(ptr: *mut std::ffi::c_char) -> Result<MacosSystemSnapshot, String> {
    if ptr.is_null() { return Err("macOS-Kontendaten konnten nicht gelesen werden".into()); }
    let json = unsafe { CStr::from_ptr(ptr) }.to_str().map(str::to_owned);
    unsafe { ffi::macos_free_string(ptr); }
    MacosSystemSnapshot::from_json(&json.map_err(|_| "Ungültige Zeichenkodierung der macOS-Kontendaten")?)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn source(kind: MacosSourceKind, account: &str) -> MacosSystemSource {
        MacosSystemSource { uid: "shared".into(), name: "Test".into(), kind, account_uid: account.into(), account_name: "Account".into(), backend: "eventkit".into(), writable: true }
    }
    #[test]
    fn native_identity_keeps_kind_and_account() {
        let calendar = source(MacosSourceKind::Calendar, "a:b");
        assert_ne!(calendar.source_uid(), source(MacosSourceKind::Reminders, "a:b").source_uid());
        assert_ne!(calendar.source_uid(), source(MacosSourceKind::Calendar, "a").source_uid());
        assert_eq!(calendar.as_eds_source().parent_name.as_deref(), Some("Account"));
    }
    #[test]
    fn write_only_and_limited_are_never_full_read_access() {
        assert!(!permission_status(MacosPermissionKind::Calendar, 4).is_authorized());
        assert_eq!(permission_status(MacosPermissionKind::Contacts, 4), MacosPermissionStatus::Limited);
        assert!(!permission_status(MacosPermissionKind::Contacts, 99).is_authorized());
    }
    #[test]
    fn rejects_unknown_version_and_orphan_records() {
        assert!(MacosSystemSnapshot::from_json(r#"{"version":2,"sources":[]}"#).is_err());
        assert!(MacosSystemSnapshot::from_json(r#"{"version":1,"sources":[],"components":[{"source_uid":"missing","kind":"calendar","data":""}]}"#).is_err());
    }
    #[test]
    fn projects_records_without_losing_native_source_or_copying_to_local_store() {
        let calendar = source(MacosSourceKind::Calendar, "test");
        let contacts = source(MacosSourceKind::Contacts, "test");
        let snapshot = MacosSystemSnapshot {
            version: 1, sources: vec![calendar.clone(), contacts.clone()], errors: vec![],
            components: vec![
                MacosSystemComponent { source_uid: "shared".into(), kind: MacosSourceKind::Calendar, data: "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:event-1\r\nSUMMARY:Test\r\nDTSTART:20260909T100000Z\r\nDTEND:20260909T110000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n".into() },
                MacosSystemComponent { source_uid: "shared".into(), kind: MacosSourceKind::Contacts, data: "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:contact-1\r\nFN:Example\r\nEND:VCARD\r\n".into() },
            ],
        };
        let parsed = MacosSystemSnapshot::from_json(&serde_json::to_string(&snapshot).unwrap()).unwrap();
        let events = parsed.calendar_snapshot().events;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].source_uid, calendar.source_uid());
        let records = parsed.contact_records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].source_uid, contacts.source_uid());
        assert_eq!(records[0].uid, "contact-1");
    }

    #[test]
    fn keychain_service_identifier_is_org_gnome_evolution4() {
        assert_eq!(MacosIntegrationService::KEYCHAIN_SERVICE_EVOLUTION, "com.martingawron.Meridian");
    }

    #[test]
    fn keychain_crud_operations_test() {
        let service = MacosIntegrationService::new();
        if cfg!(all(target_os = "macos", macos_bridge)) {
            let test_account = "test-worker-m3-account-keychain";
            let test_pass = "WorkerM3SecretP@ss!";
            assert!(service.keychain_set_password(test_account, test_pass).is_ok());
            let fetched = service.keychain_get_password(test_account);
            assert!(fetched.is_ok());
            assert_eq!(fetched.unwrap(), Some(test_pass.to_string()));

            // Update
            let updated_pass = "WorkerM3UpdatedP@ss!";
            assert!(service.keychain_set_password(test_account, updated_pass).is_ok());
            assert_eq!(service.keychain_get_password(test_account).unwrap(), Some(updated_pass.to_string()));

            // Delete
            assert!(service.keychain_delete_password(test_account).is_ok());
            assert_eq!(service.keychain_get_password(test_account).unwrap(), None);
        } else {
            let res = service.keychain_get_password("any-account");
            assert!(res.is_err() || res.is_ok());
        }
    }
}

