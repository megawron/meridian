// SPDX-License-Identifier: GPL-3.0-or-later
//
// AvatarCacheService — Asynchronous contact and domain avatar fetching and caching.

use gtk::glib;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use super::contact_store_service::ContactStoreService;

/// Known freemail domains where a sender is an individual person rather than
/// an organization. Favicons for these domains should not be used as sender avatars.
const FREEMAIL_DOMAINS: &[&str] = &[
    "gmail.com",
    "googlemail.com",
    "yahoo.com",
    "yahoo.de",
    "yahoo.fr",
    "yahoo.co.uk",
    "hotmail.com",
    "hotmail.de",
    "outlook.com",
    "outlook.de",
    "live.com",
    "live.de",
    "msn.com",
    "icloud.com",
    "me.com",
    "mac.com",
    "gmx.de",
    "gmx.net",
    "gmx.at",
    "gmx.ch",
    "web.de",
    "t-online.de",
    "freenet.de",
    "posteo.de",
    "posteo.net",
    "proton.me",
    "protonmail.com",
    "mail.ru",
    "yandex.ru",
    "zoho.com",
    "aol.com",
];

/// Singleton instance for avatar caching.
static AVATAR_CACHE_SERVICE: OnceLock<AvatarCacheService> = OnceLock::new();

type WaitingSenders = Vec<async_channel::Sender<glib::Bytes>>;

pub struct AvatarCacheService {
    cache_dir: PathBuf,
    memory_cache: Arc<Mutex<HashMap<String, Option<glib::Bytes>>>>,
    local_contacts: Arc<Mutex<Option<HashMap<String, Option<glib::Bytes>>>>>,
    in_flight: Arc<Mutex<HashMap<String, WaitingSenders>>>,
}

impl Default for AvatarCacheService {
    fn default() -> Self {
        Self::new()
    }
}

impl AvatarCacheService {
    /// Return the global shared instance.
    pub fn global() -> &'static Self {
        AVATAR_CACHE_SERVICE.get_or_init(Self::new)
    }

    pub fn new() -> Self {
        let cache_dir = crate::services::app_cache_dir().join("avatars");
        let _ = std::fs::create_dir_all(&cache_dir);

        // Purge legacy low-resolution or ICO cache files on startup
        if let Ok(entries) = std::fs::read_dir(&cache_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "png") {
                    if let Ok(bytes) = std::fs::read(&path) {
                        if Self::is_low_res_or_ico(&bytes) {
                            let _ = std::fs::remove_file(path);
                        }
                    }
                }
            }
        }

        Self {
            cache_dir,
            memory_cache: Arc::new(Mutex::new(HashMap::new())),
            local_contacts: Arc::new(Mutex::new(None)),
            in_flight: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Return the path to the disk cache directory.
    pub fn cache_dir(&self) -> &PathBuf {
        &self.cache_dir
    }

    /// Clear in-memory decoded avatar and logo cache.
    pub fn clear_memory_cache(&self) {
        if let Ok(mut cache) = self.memory_cache.lock() {
            cache.clear();
        }
    }

    /// Extract a clean email address from a string like "Name <email@example.com>".
    pub fn extract_email(raw: &str) -> String {
        let trimmed = raw.trim();
        if let Some(start) = trimmed.find('<') {
            if let Some(end) = trimmed.rfind('>') {
                if start < end {
                    return trimmed[start + 1..end].trim().to_ascii_lowercase();
                }
            }
        }
        trimmed.to_ascii_lowercase()
    }

    /// Extract domain from an email address (e.g. "user@github.com" -> "github.com").
    pub fn extract_domain(email_or_sender: &str) -> Option<String> {
        let email = Self::extract_email(email_or_sender);
        let domain = email.split('@').nth(1)?.trim().to_ascii_lowercase();
        if domain.is_empty() || !domain.contains('.') {
            None
        } else {
            Some(domain)
        }
    }

    /// Return whether a domain is a freemail provider.
    pub fn is_freemail_domain(domain: &str) -> bool {
        let domain_norm = domain.trim().to_ascii_lowercase();
        FREEMAIL_DOMAINS
            .iter()
            .any(|&f| domain_norm == f || domain_norm.ends_with(&format!(".{f}")))
    }

    /// Compute a disk cache filename hash for a given key.
    pub fn cache_key_hash(key: &str) -> String {
        glib::compute_checksum_for_string(glib::ChecksumType::Sha256, key)
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("{:x}", key.len()))
    }

    /// Extract parent / root domain if this is a subdomain (e.g. "offers.proton.me" -> "proton.me", "news.goflink.com" -> "goflink.com").
    pub fn parent_domain(domain: &str) -> Option<String> {
        let parts: Vec<&str> = domain.split('.').collect();
        let two_part_tlds = [
            "co.uk", "org.uk", "gov.uk", "ac.uk", "co.jp", "ne.jp", "com.br",
            "com.au", "net.au", "com.pl", "co.nz", "com.sg", "co.za", "com.tr",
            "co.kr", "com.de",
        ];
        let last_two = if parts.len() >= 2 {
            format!("{}.{}", parts[parts.len() - 2], parts[parts.len() - 1])
        } else {
            return None;
        };
        let min_parts = if two_part_tlds.contains(&last_two.as_str()) { 3 } else { 2 };
        if parts.len() > min_parts {
            Some(parts[1..].join("."))
        } else {
            None
        }
    }

    /// Return all candidate domain levels from the full domain down to the root registrable domain.
    /// (e.g. "sc.mail.deepseek.com" -> ["sc.mail.deepseek.com", "mail.deepseek.com", "deepseek.com"])
    pub fn all_candidate_domains(domain: &str) -> Vec<String> {
        let parts: Vec<&str> = domain.split('.').collect();
        if parts.len() < 2 {
            return vec![domain.to_string()];
        }
        let two_part_tlds = [
            "co.uk", "org.uk", "gov.uk", "ac.uk", "co.jp", "ne.jp", "com.br",
            "com.au", "net.au", "com.pl", "co.nz", "com.sg", "co.za", "com.tr",
            "co.kr", "com.de",
        ];
        let last_two = format!("{}.{}", parts[parts.len() - 2], parts[parts.len() - 1]);
        let min_parts = if two_part_tlds.contains(&last_two.as_str()) { 3 } else { 2 };

        let mut res = Vec::new();
        res.push(domain.to_string());
        for i in 1..=parts.len().saturating_sub(min_parts) {
            let parent = parts[i..].join(".");
            if !res.contains(&parent) {
                res.push(parent);
            }
        }
        res
    }

    /// Extract candidate domains prioritizing dedicated service brands (e.g. Google Maps, Google Play)
    /// followed by the domain and parent/root domain hierarchy.
    pub fn candidate_domains_for_sender(email: &str, sender_name: &str) -> Vec<String> {
        let clean = Self::extract_email(email);
        let Some(domain) = Self::extract_domain(&clean) else {
            return Vec::new();
        };

        let mut candidates = Vec::new();

        // Check for specific sub-services of major providers
        let local_part = clean.split('@').next().unwrap_or("").to_ascii_lowercase();
        let name_lower = sender_name.trim().to_ascii_lowercase();

        if domain == "google.com" || domain == "googlemail.com" || domain == "gmail.com" || domain.starts_with("google.") {
            if local_part.contains("maps") || name_lower.contains("maps") {
                candidates.push("maps.google.com".to_string());
            } else if local_part.contains("play") || name_lower.contains("play") {
                candidates.push("play.google.com".to_string());
            } else if local_part.contains("photos") || name_lower.contains("photos") {
                candidates.push("photos.google.com".to_string());
            } else if local_part.contains("calendar") || name_lower.contains("calendar") {
                candidates.push("calendar.google.com".to_string());
            } else if local_part.contains("drive") || name_lower.contains("drive") {
                candidates.push("drive.google.com".to_string());
            } else if local_part.contains("youtube") || name_lower.contains("youtube") {
                candidates.push("youtube.com".to_string());
            }
        }

        // Add domain itself and all parent domains down to root
        for parent in Self::all_candidate_domains(&domain) {
            if !candidates.contains(&parent) {
                candidates.push(parent);
            }
        }

        // Ensure google.com is included if domain is a google subdomain or service
        if (domain.ends_with(".google.com") || domain == "googlemail.com" || domain == "gmail.com")
            && !candidates.contains(&"google.com".to_string())
        {
            candidates.push("google.com".to_string());
        }

        candidates
    }

    /// Check whether an email from a freemail/public provider domain is an official
    /// service/transactional communication from the provider itself.
    pub fn is_official_or_brand_sender(email: &str, domain: &str) -> bool {
        let clean = email.trim().to_ascii_lowercase();
        let local_part = clean.split('@').next().unwrap_or("").trim();
        if local_part.is_empty() {
            return false;
        }

        const OFFICIAL_PREFIXES: &[&str] = &[
            "no-reply", "noreply", "notify", "notification", "notifications",
            "news", "newsletter", "team", "support", "security", "contact",
            "hello", "info", "service", "services", "system", "admin", "administrator",
            "billing", "update", "updates", "marketing", "alert", "alerts",
            "help", "account", "accounts", "verify", "verification", "welcome",
            "mail", "mailer-daemon", "postmaster", "offers", "offer", "member",
            "members", "community", "privacy", "legal", "feedback", "announce",
            "announcements", "status", "sales", "order", "orders", "invoice",
            "invoices", "security-noreply", "mail-noreply",
        ];

        if OFFICIAL_PREFIXES.iter().any(|&p| {
            local_part == p
                || local_part.starts_with(&format!("{p}-"))
                || local_part.starts_with(&format!("{p}."))
                || local_part.starts_with(&format!("{p}_"))
                || local_part.ends_with(&format!("-{p}"))
                || local_part.ends_with(&format!(".{p}"))
                || local_part.ends_with(&format!("_{p}"))
                || local_part.contains(&format!("-{p}-"))
        }) {
            return true;
        }

        if local_part.contains("noreply") || local_part.contains("no-reply") {
            return true;
        }

        if let Some(sub) = domain.split('.').next() {
            if OFFICIAL_PREFIXES.contains(&sub) {
                return true;
            }
        }

        let brand_candidates = ["proton", "google", "yahoo", "gmx", "webde", "t-online", "posteo", "zoho", "icloud"];
        for brand in &brand_candidates {
            if domain.contains(brand) && (local_part.contains(brand) || local_part.contains("noreply") || local_part.contains("team") || local_part.contains("support")) {
                return true;
            }
        }

        false
    }

    /// Extract width and height from PNG IHDR chunk, or (0, 0) if not a valid PNG.
    pub fn png_dimensions(bytes: &[u8]) -> (u32, u32) {
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.len() >= 24 {
            let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
            let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
            (width, height)
        } else {
            (0, 0)
        }
    }

    pub const GOOGLE_DEFAULT_GLOBE_SHA256: &str =
        "59bfe9bc385ad69f50793ce4a53397316d7a875a7148a63c16df9b674c6cda64";
    pub const DDG_DEFAULT_GLOBE_SHA256: &str =
        "e5db88ea2322863ca17817b99d60006c625a31cff0dad49cf05d3c6d16a75c17";

    /// Check if bytes are a generic fallback placeholder (e.g. gray globe when domain has no favicon).
    pub fn is_generic_placeholder(bytes: &[u8]) -> bool {
        if let Some(hash) = glib::compute_checksum_for_data(glib::ChecksumType::Sha256, bytes) {
            let h = hash.as_str();
            h == Self::GOOGLE_DEFAULT_GLOBE_SHA256 || h == Self::DDG_DEFAULT_GLOBE_SHA256
        } else {
            false
        }
    }

    /// Check if cached image bytes are corrupt, too small (< 16px), or a generic placeholder.
    pub fn is_invalid_or_placeholder(bytes: &[u8]) -> bool {
        // Too small to be a valid image header
        if bytes.len() < 24 {
            return true;
        }
        // Reject generic fallback globes from Google and DuckDuckGo
        if Self::is_generic_placeholder(bytes) {
            return true;
        }
        // PNG format: reject < 16px (e.g. 1x1 tracking pixels)
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            let (width, height) = Self::png_dimensions(bytes);
            if width < 16 || height < 16 {
                return true;
            }
        }
        // ICO format: reject if all embedded icons are < 16px
        if bytes.starts_with(b"\x00\x00\x01\x00") && bytes.len() >= 14 {
            let num_images = u16::from_le_bytes([bytes[4], bytes[5]]) as usize;
            let mut has_valid_size = false;
            for i in 0..num_images {
                let entry_offset = 6 + i * 16;
                if entry_offset + 2 <= bytes.len() {
                    let w = if bytes[entry_offset] == 0 { 256 } else { bytes[entry_offset] as u32 };
                    let h = if bytes[entry_offset + 1] == 0 { 256 } else { bytes[entry_offset + 1] as u32 };
                    if w >= 16 && h >= 16 {
                        has_valid_size = true;
                        break;
                    }
                }
            }
            if !has_valid_size {
                return true;
            }
        }
        false
    }

    /// Backwards compatibility helper
    pub fn is_low_res_or_ico(bytes: &[u8]) -> bool {
        Self::is_invalid_or_placeholder(bytes)
    }

    /// Get avatar bytes from memory or disk cache synchronously, without fetching.
    pub fn get_cached(&self, email_or_sender: &str) -> Option<glib::Bytes> {
        self.get_cached_for_sender(email_or_sender, "")
    }

    /// Get avatar bytes from memory or disk cache synchronously for a sender.
    pub fn get_cached_for_sender(&self, email_or_sender: &str, sender_name: &str) -> Option<glib::Bytes> {
        let clean = Self::extract_email(email_or_sender);
        if clean.is_empty() {
            return None;
        }

        // 1. Check in-memory cache
        if let Ok(cache) = self.memory_cache.lock() {
            if let Some(cached) = cache.get(&clean) {
                return cached.clone();
            }
        }

        // 2. Check local disk cache for email hash
        let email_hash = Self::cache_key_hash(&clean);
        let email_path = self.cache_dir.join(format!("{email_hash}.png"));
        if email_path.exists() {
            if let Ok(bytes) = std::fs::read(&email_path) {
                if !Self::is_low_res_or_ico(&bytes) {
                    let glib_bytes = glib::Bytes::from_owned(bytes);
                    if let Ok(mut cache) = self.memory_cache.lock() {
                        cache.insert(clean.clone(), Some(glib_bytes.clone()));
                    }
                    return Some(glib_bytes);
                } else {
                    let _ = std::fs::remove_file(&email_path);
                }
            }
        }

        // 3. Check candidate domains
        if let Some(domain) = Self::extract_domain(&clean) {
            let is_freemail = Self::is_freemail_domain(&domain);
            let is_official = Self::is_official_or_brand_sender(&clean, &domain);
            if !is_freemail || is_official {
                let candidate_domains = Self::candidate_domains_for_sender(&clean, sender_name);
                for d in candidate_domains {
                    let domain_hash = Self::cache_key_hash(&d);
                    let domain_path = self.cache_dir.join(format!("{domain_hash}.png"));
                    if domain_path.exists() {
                        if let Ok(bytes) = std::fs::read(&domain_path) {
                            if !Self::is_low_res_or_ico(&bytes) {
                                let glib_bytes = glib::Bytes::from_owned(bytes);
                                if let Ok(mut cache) = self.memory_cache.lock() {
                                    cache.insert(clean.clone(), Some(glib_bytes.clone()));
                                }
                                return Some(glib_bytes);
                            } else {
                                let _ = std::fs::remove_file(&domain_path);
                            }
                        }
                    }
                }
            }
        }

        None
    }

    /// Request an avatar synchronously or asynchronously.
    pub fn request_avatar(
        &self,
        email_or_sender: &str,
        allow_remote: bool,
        tx: Option<async_channel::Sender<glib::Bytes>>,
    ) -> Option<glib::Bytes> {
        self.request_avatar_for_sender(email_or_sender, "", allow_remote, tx)
    }

    /// Request an avatar synchronously or asynchronously for a specific sender name and email.
    ///
    /// 1. Checks memory cache.
    /// 2. Checks local EDS contacts.
    /// 3. Checks disk cache.
    /// 4. If allow_remote is true and not cached, starts or joins an asynchronous background fetch
    ///    and notifies `tx` when done.
    pub fn request_avatar_for_sender(
        &self,
        email_or_sender: &str,
        sender_name: &str,
        allow_remote: bool,
        tx: Option<async_channel::Sender<glib::Bytes>>,
    ) -> Option<glib::Bytes> {
        let clean = Self::extract_email(email_or_sender);
        if clean.is_empty() {
            return None;
        }

        // If remote logos are disabled, ONLY check local contact photo from address book.
        // Never return remote domain favicons or Gravatars from memory or disk cache.
        if !allow_remote {
            return self.get_local_contact_photo(&clean);
        }

        // 1. Check local contact photo via cached ContactStoreService (takes precedence over remote logos)
        if let Some(glib_bytes) = self.get_local_contact_photo(&clean) {
            if let Ok(mut cache) = self.memory_cache.lock() {
                cache.insert(clean.clone(), Some(glib_bytes.clone()));
            }
            return Some(glib_bytes);
        }

        // 2. Fast cache check (memory and disk cache for remote logos)
        if let Some(bytes) = self.get_cached_for_sender(&clean, sender_name) {
            return Some(bytes);
        }

        // 3. Remote fetch if permitted
        if allow_remote {
            let Some(domain) = Self::extract_domain(&clean) else {
                if let Ok(mut cache) = self.memory_cache.lock() {
                    cache.insert(clean.clone(), None);
                }
                return None;
            };

            let is_freemail = Self::is_freemail_domain(&domain);
            let is_official = Self::is_official_or_brand_sender(&clean, &domain);

            // We will fetch Gravatar for EVERYONE (if available).
            // But we only fetch domain favicons for non-freemail, or official brand freemails.
            let fetch_favicons = !is_freemail || is_official;

            let candidate_domains = if fetch_favicons {
                Self::candidate_domains_for_sender(&clean, sender_name)
            } else {
                Vec::new() // Don't fetch favicons for normal freemail users
            };

            let coalesce_key = if fetch_favicons {
                candidate_domains.first().cloned().unwrap_or_else(|| domain.clone())
            } else {
                clean.clone() // For freemail users, coalesce by email address (Gravatar)
            };

            // Check if already in flight - if so, coalesce request by registering tx
            {
                let mut in_flight = self.in_flight.lock().unwrap();
                if let Some(waiting) = in_flight.get_mut(&coalesce_key) {
                    if let Some(sender) = tx {
                        waiting.push(sender);
                    }
                    return None;
                }
                let mut waiting = Vec::new();
                if let Some(sender) = tx {
                    waiting.push(sender);
                }
                in_flight.insert(coalesce_key.clone(), waiting);
            }

            let cache_dir = self.cache_dir.clone();
            let memory_cache = Arc::clone(&self.memory_cache);
            let in_flight = Arc::clone(&self.in_flight);
            let clean_email = clean.clone();

            std::thread::spawn(move || {
                let bytes_opt = Self::fetch_remote_avatar(&clean_email, &candidate_domains);

                // Drain all waiting channels
                let waiting_senders = {
                    let mut in_flight_guard = in_flight.lock().unwrap();
                    in_flight_guard.remove(&coalesce_key).unwrap_or_default()
                };

                if let Some(bytes) = bytes_opt {
                    // Save to disk under email hash
                    let email_hash = Self::cache_key_hash(&clean_email);
                    let _ = std::fs::write(cache_dir.join(format!("{email_hash}.png")), &bytes);

                    // Also save under candidate domain hashes (if any)
                    for cand in &candidate_domains {
                        let cand_hash = Self::cache_key_hash(cand);
                        let _ = std::fs::write(cache_dir.join(format!("{cand_hash}.png")), &bytes);
                    }

                    let glib_bytes = glib::Bytes::from_owned(bytes);

                    // Update memory cache
                    if let Ok(mut cache) = memory_cache.lock() {
                        cache.insert(clean_email, Some(glib_bytes.clone()));
                        for cand in &candidate_domains {
                            cache.insert(cand.clone(), Some(glib_bytes.clone()));
                        }
                    }

                    // Notify ALL waiting callers!
                    for sender in waiting_senders {
                        let _ = sender.send_blocking(glib_bytes.clone());
                    }
                } else if let Ok(mut cache) = memory_cache.lock() {
                    cache.insert(clean_email, None);
                }
            });
        } else if let Ok(mut cache) = self.memory_cache.lock() {
            cache.insert(clean.clone(), None);
        }

        None
    }

    /// Cached lookup for local contact photos from ContactStoreService
    fn get_local_contact_photo(&self, clean_email: &str) -> Option<glib::Bytes> {
        let mut guard = self.local_contacts.lock().ok()?;
        if guard.is_none() {
            let mut map: HashMap<String, Option<glib::Bytes>> = HashMap::new();
            let store = ContactStoreService::new();
            for record in store.load_contacts() {
                if let Some(photo) = record.contact.photo.as_ref() {
                    if let Some(bytes) = photo.decoded_bytes() {
                        let glib_bytes = glib::Bytes::from_owned(bytes);
                        for e in record.contact.emails {
                            let clean_e = Self::extract_email(&e);
                            if !clean_e.is_empty() {
                                map.insert(clean_e, Some(glib_bytes.clone()));
                            }
                        }
                    }
                }
            }
            *guard = Some(map);
        }
        guard.as_ref()?.get(clean_email).and_then(|opt| opt.clone())
    }

    /// Invalidate the cached local contact photos (e.g. when contacts change).
    pub fn invalidate_local_contacts_cache(&self) {
        if let Ok(mut guard) = self.local_contacts.lock() {
            *guard = None;
        }
    }

    /// Blocking helper to fetch avatar bytes for an email and its candidate domains.
    /// Prefers Gravatar first, then falls back to DuckDuckGo/Google favicons for the domains.
    fn fetch_remote_avatar(email: &str, candidate_domains: &[String]) -> Option<Vec<u8>> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .ok()?;

        // Primary: Gravatar (returns 404 if no custom avatar exists)
        if let Some(md5_hash) = glib::compute_checksum_for_string(glib::ChecksumType::Md5, email) {
            let gravatar_url = format!("https://www.gravatar.com/avatar/{}?d=404&s=128", md5_hash);
            if let Ok(resp) = client.get(&gravatar_url).header("User-Agent", "Mozilla/5.0").send() {
                if resp.status().is_success() {
                    if let Ok(bytes) = resp.bytes() {
                        if !Self::is_invalid_or_placeholder(&bytes) {
                            return Some(bytes.to_vec());
                        }
                    }
                }
            }
        }

        // Secondary: DuckDuckGo Icons API (fast, reliable, returns ICO/PNG)
        for cand in candidate_domains {
            let ddg_url = format!("https://icons.duckduckgo.com/ip3/{cand}.ico");
            if let Ok(resp) = client.get(&ddg_url).header("User-Agent", "Mozilla/5.0 (X11; Linux x86_64)").send() {
                if resp.status().is_success() {
                    if let Ok(bytes) = resp.bytes() {
                        if !Self::is_invalid_or_placeholder(&bytes) {
                            return Some(bytes.to_vec());
                        }
                    }
                }
            }
        }

        // Tertiary: Google FaviconV2 API with size=128
        for cand in candidate_domains {
            let google_url = format!("https://t0.gstatic.com/faviconV2?client=SOCIAL&type=FAVICON&fallback_opts=TYPE,SIZE,URL&url=https://{cand}&size=128");
            if let Ok(resp) = client.get(&google_url).header("User-Agent", "Mozilla/5.0 (X11; Linux x86_64)").send() {
                if resp.status().is_success() {
                    if let Ok(bytes) = resp.bytes() {
                        if !Self::is_invalid_or_placeholder(&bytes) {
                            return Some(bytes.to_vec());
                        }
                    }
                }
            }
        }

        None
    }

    /// Clear in-memory and disk avatar caches.
    pub fn clear_cache(&self) {
        if let Ok(mut cache) = self.memory_cache.lock() {
            cache.clear();
        }
        if let Ok(entries) = std::fs::read_dir(&self.cache_dir) {
            for entry in entries.flatten() {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_email_and_domain() {
        assert_eq!(
            AvatarCacheService::extract_email("GitHub <notifications@github.com>"),
            "notifications@github.com"
        );
        assert_eq!(
            AvatarCacheService::extract_domain("GitHub <notifications@github.com>"),
            Some("github.com".to_string())
        );
        assert_eq!(
            AvatarCacheService::extract_domain("alice@work.sub.company.org"),
            Some("work.sub.company.org".to_string())
        );
        assert_eq!(AvatarCacheService::extract_domain("invalid-no-at"), None);
    }

    #[test]
    fn test_freemail_detection() {
        assert!(AvatarCacheService::is_freemail_domain("gmail.com"));
        assert!(AvatarCacheService::is_freemail_domain("web.de"));
        assert!(AvatarCacheService::is_freemail_domain("gmx.net"));
        assert!(AvatarCacheService::is_freemail_domain("proton.me"));
        assert!(!AvatarCacheService::is_freemail_domain("github.com"));
        assert!(!AvatarCacheService::is_freemail_domain("company.de"));
    }

    #[test]
    fn test_cache_key_hash() {
        let hash1 = AvatarCacheService::cache_key_hash("github.com");
        let hash2 = AvatarCacheService::cache_key_hash("github.com");
        let hash3 = AvatarCacheService::cache_key_hash("other.com");
        assert_eq!(hash1, hash2);
        assert_ne!(hash1, hash3);
        assert_eq!(hash1.len(), 64);
    }

    #[test]
    fn test_parent_domain_extraction() {
        assert_eq!(AvatarCacheService::parent_domain("offers.proton.me"), Some("proton.me".to_string()));
        assert_eq!(AvatarCacheService::parent_domain("notify.proton.me"), Some("proton.me".to_string()));
        assert_eq!(AvatarCacheService::parent_domain("news.goflink.com"), Some("goflink.com".to_string()));
        assert_eq!(AvatarCacheService::parent_domain("mail.sub.example.co.uk"), Some("sub.example.co.uk".to_string()));
        assert_eq!(AvatarCacheService::parent_domain("proton.me"), None);
    }

    #[test]
    fn test_is_official_or_brand_sender() {
        assert!(AvatarCacheService::is_official_or_brand_sender("no-reply@offers.proton.me", "offers.proton.me"));
        assert!(AvatarCacheService::is_official_or_brand_sender("team@proton.me", "proton.me"));
        assert!(AvatarCacheService::is_official_or_brand_sender("support@proton.me", "proton.me"));
        assert!(AvatarCacheService::is_official_or_brand_sender("no-reply@notify.proton.me", "notify.proton.me"));
        assert!(AvatarCacheService::is_official_or_brand_sender("no-reply@accounts.google.com", "accounts.google.com"));
        assert!(AvatarCacheService::is_official_or_brand_sender("protonteam@proton.me", "proton.me"));
        assert!(!AvatarCacheService::is_official_or_brand_sender("max.jensen@example.org", "protonmail.com"));
        assert!(!AvatarCacheService::is_official_or_brand_sender("john.doe@gmail.com", "gmail.com"));
    }

    #[test]
    fn test_is_low_res_or_ico() {
        // Corrupted / too short header (<24 bytes) is rejected
        assert!(AvatarCacheService::is_invalid_or_placeholder(&[0, 0, 1, 0, 1, 2, 3]));

        // PNG header with 8x8 is rejected (too small / tracking pixel)
        let mut png_8 = Vec::from(b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR");
        png_8.extend_from_slice(&8u32.to_be_bytes());
        png_8.extend_from_slice(&8u32.to_be_bytes());
        assert!(AvatarCacheService::is_invalid_or_placeholder(&png_8));

        // PNG header with 16x16 is accepted as valid low-res fallback
        let mut png_16 = Vec::from(b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR");
        png_16.extend_from_slice(&16u32.to_be_bytes());
        png_16.extend_from_slice(&16u32.to_be_bytes());
        assert!(!AvatarCacheService::is_invalid_or_placeholder(&png_16));

        // PNG header with 128x128 is accepted
        let mut png_128 = Vec::from(b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR");
        png_128.extend_from_slice(&128u32.to_be_bytes());
        png_128.extend_from_slice(&128u32.to_be_bytes());
        assert!(!AvatarCacheService::is_invalid_or_placeholder(&png_128));
    }

    #[test]
    fn test_all_candidate_domains() {
        let deepseek = AvatarCacheService::all_candidate_domains("sc.mail.deepseek.com");
        assert_eq!(deepseek, vec!["sc.mail.deepseek.com", "mail.deepseek.com", "deepseek.com"]);

        let flink = AvatarCacheService::all_candidate_domains("news.goflink.com");
        assert_eq!(flink, vec!["news.goflink.com", "goflink.com"]);

        let bbc = AvatarCacheService::all_candidate_domains("news.bbc.co.uk");
        assert_eq!(bbc, vec!["news.bbc.co.uk", "bbc.co.uk"]);
    }

    #[test]
    fn test_candidate_domains_for_sender() {
        let maps = AvatarCacheService::candidate_domains_for_sender("google-maps-noreply@google.com", "Google Maps");
        assert_eq!(maps[0], "maps.google.com");
        assert!(maps.contains(&"google.com".to_string()));

        let play = AvatarCacheService::candidate_domains_for_sender("googleplay-noreply@google.com", "Google Play");
        assert_eq!(play[0], "play.google.com");
        assert!(play.contains(&"google.com".to_string()));

        let accounts = AvatarCacheService::candidate_domains_for_sender("no-reply@accounts.google.com", "Google");
        assert_eq!(accounts, vec!["accounts.google.com", "google.com"]);
    }

    #[test]
    fn test_allow_remote_disabled_returns_none_and_clear_memory_cache() {
        let service = AvatarCacheService::new();
        // Insert a dummy remote avatar into memory cache
        if let Ok(mut cache) = service.memory_cache.lock() {
            cache.insert("user@example.com".to_string(), Some(glib::Bytes::from_static(b"fake-avatar")));
        }

        // With allow_remote = false, request_avatar_for_sender must not return the remote logo
        let res = service.request_avatar_for_sender("user@example.com", "User", false, None);
        assert!(res.is_none());

        // clear_memory_cache should empty the in-memory cache
        service.clear_memory_cache();
        if let Ok(cache) = service.memory_cache.lock() {
            assert!(cache.is_empty());
        }
    }
}
