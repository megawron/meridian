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

pub struct AvatarCacheService {
    cache_dir: PathBuf,
    memory_cache: Arc<Mutex<HashMap<String, Option<glib::Bytes>>>>,
    in_flight: Arc<Mutex<HashSet<String>>>,
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
        Self {
            cache_dir,
            memory_cache: Arc::new(Mutex::new(HashMap::new())),
            in_flight: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Return the path to the disk cache directory.
    pub fn cache_dir(&self) -> &PathBuf {
        &self.cache_dir
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

    /// Get avatar bytes from memory or disk cache synchronously, without fetching.
    pub fn get_cached(&self, email_or_sender: &str) -> Option<glib::Bytes> {
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
                let glib_bytes = glib::Bytes::from_owned(bytes);
                if let Ok(mut cache) = self.memory_cache.lock() {
                    cache.insert(clean.clone(), Some(glib_bytes.clone()));
                }
                return Some(glib_bytes);
            }
        }

        // 3. Check local disk cache for domain hash (if not freemail)
        if let Some(domain) = Self::extract_domain(&clean) {
            if !Self::is_freemail_domain(&domain) {
                let domain_hash = Self::cache_key_hash(&domain);
                let domain_path = self.cache_dir.join(format!("{domain_hash}.png"));
                if domain_path.exists() {
                    if let Ok(bytes) = std::fs::read(&domain_path) {
                        let glib_bytes = glib::Bytes::from_owned(bytes);
                        if let Ok(mut cache) = self.memory_cache.lock() {
                            cache.insert(clean, Some(glib_bytes.clone()));
                        }
                        return Some(glib_bytes);
                    }
                }
            }
        }

        None
    }

    /// Request an avatar synchronously or asynchronously.
    ///
    /// 1. Checks memory cache.
    /// 2. Checks local EDS contacts.
    /// 3. Checks disk cache.
    /// 4. If allow_remote is true and not cached, starts an asynchronous background fetch
    ///    and notifies `on_loaded` when done.
    pub fn request_avatar(
        &self,
        email_or_sender: &str,
        allow_remote: bool,
        tx: Option<async_channel::Sender<glib::Bytes>>,
    ) -> Option<glib::Bytes> {
        let clean = Self::extract_email(email_or_sender);
        if clean.is_empty() {
            return None;
        }

        // 1. Fast cache check
        if let Some(bytes) = self.get_cached(&clean) {
            return Some(bytes);
        }

        // 2. Check local contact photo via ContactStoreService
        let store = ContactStoreService::new();
        let contacts = store.load_contacts();
        for record in contacts {
            let matches = record.contact.emails.iter().any(|e| {
                Self::extract_email(e) == clean || e.trim().eq_ignore_ascii_case(&clean)
            });
            if matches {
                if let Some(photo) = record.contact.photo.as_ref() {
                    if let Some(bytes) = photo.decoded_bytes() {
                        let glib_bytes = glib::Bytes::from_owned(bytes);
                        if let Ok(mut cache) = self.memory_cache.lock() {
                            cache.insert(clean.clone(), Some(glib_bytes.clone()));
                        }
                        return Some(glib_bytes);
                    }
                }
            }
        }

        // 3. Remote fetch if permitted
        if allow_remote {
            let Some(domain) = Self::extract_domain(&clean) else {
                return None;
            };

            if Self::is_freemail_domain(&domain) {
                // Do not fetch company logos for freemail providers
                return None;
            }

            // Check if already in flight
            {
                let mut in_flight = self.in_flight.lock().unwrap();
                if in_flight.contains(&domain) {
                    return None;
                }
                in_flight.insert(domain.clone());
            }

            let cache_dir = self.cache_dir.clone();
            let memory_cache = Arc::clone(&self.memory_cache);
            let in_flight = Arc::clone(&self.in_flight);
            let clean_email = clean.clone();
            let domain_clone = domain.clone();

            std::thread::spawn(move || {
                let domain_hash = Self::cache_key_hash(&domain_clone);
                let target_path = cache_dir.join(format!("{domain_hash}.png"));

                let bytes_opt = Self::fetch_favicon_bytes(&domain_clone);

                if let Some(bytes) = bytes_opt {
                    // Save to disk
                    let _ = std::fs::write(&target_path, &bytes);
                    let glib_bytes = glib::Bytes::from_owned(bytes);

                    // Update memory cache
                    if let Ok(mut cache) = memory_cache.lock() {
                        cache.insert(clean_email, Some(glib_bytes.clone()));
                        cache.insert(domain_clone.clone(), Some(glib_bytes.clone()));
                    }

                    // Notify caller
                    if let Some(sender) = tx {
                        let _ = sender.send_blocking(glib_bytes);
                    }
                }

                // Remove from in-flight
                if let Ok(mut in_flight) = in_flight.lock() {
                    in_flight.remove(&domain_clone);
                }
            });
        }

        None
    }

    /// Blocking helper to fetch favicon bytes for a domain with a short timeout.
    fn fetch_favicon_bytes(domain: &str) -> Option<Vec<u8>> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(4))
            .build()
            .ok()?;

        // Primary: DuckDuckGo Icons API (fast, privacy-preserving)
        let ddg_url = format!("https://icons.duckduckgo.com/ip3/{domain}.ico");
        if let Ok(resp) = client
            .get(&ddg_url)
            .header("User-Agent", "Meridian-Mail")
            .send()
        {
            if resp.status().is_success() {
                if let Ok(bytes) = resp.bytes() {
                    if !bytes.is_empty() {
                        return Some(bytes.to_vec());
                    }
                }
            }
        }

        // Fallback: Google Favicons API (high-availability PNG fallback)
        let google_url = format!("https://www.google.com/s2/favicons?domain={domain}&sz=64");
        if let Ok(resp) = client
            .get(&google_url)
            .header("User-Agent", "Meridian-Mail")
            .send()
        {
            if resp.status().is_success() {
                if let Ok(bytes) = resp.bytes() {
                    if !bytes.is_empty() {
                        return Some(bytes.to_vec());
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
}
