// SPDX-License-Identifier: GPL-3.0-or-later
//
// NetworkMonitorService — Monitors connectivity, metered connections, power-saver state, and proxy.
//
// Implements network & power state inspection matching upstream Evolution
// (e-network-monitor.c, org.gnome.evolution.shell.network-config, and GIO GNetworkMonitor).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::settings_service::AppSettings;

/// Network connectivity state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkConnectivity {
    /// Fully connected with unmetered connection (Ethernet, WiFi).
    Online,
    /// Connected with metered data limit (Mobile Broadband / Cellular hotspot).
    Metered,
    /// Disconnected / no active route.
    Offline,
}

/// System power-saver and battery state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerSaverState {
    /// Normal operation (mains power or full battery).
    Normal,
    /// System power saver mode enabled by desktop environment.
    PowerSaver,
    /// Battery is below warning threshold.
    LowBattery,
}

/// Proxy operation mode matching upstream Evolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyMode {
    /// Use system desktop proxy settings.
    System,
    /// Direct connection without proxy.
    None,
    /// Manual HTTP/HTTPS proxy configuration.
    Manual,
}

impl ProxyMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::System => "system",
            Self::None => "none",
            Self::Manual => "manual",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "none" | "direct" => Self::None,
            "manual" | "custom" => Self::Manual,
            _ => Self::System,
        }
    }
}

/// Comprehensive status snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkStatusSnapshot {
    pub connectivity: NetworkConnectivity,
    pub power_state: PowerSaverState,
    pub proxy_mode: ProxyMode,
    pub proxy_host: String,
    pub proxy_port: u16,
    pub background_sync_paused: bool,
}

/// Service that evaluates network connectivity and power policies for background jobs.
#[derive(Debug, Clone)]
pub struct NetworkMonitorService {
    connectivity_override: Arc<Mutex<Option<NetworkConnectivity>>>,
    power_override: Arc<Mutex<Option<PowerSaverState>>>,
    manual_offline: Arc<AtomicBool>,
}

impl Default for NetworkMonitorService {
    fn default() -> Self {
        Self::new()
    }
}

impl NetworkMonitorService {
    pub fn new() -> Self {
        Self {
            connectivity_override: Arc::new(Mutex::new(None)),
            power_override: Arc::new(Mutex::new(None)),
            manual_offline: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Global singleton instance.
    pub fn global() -> &'static Self {
        use std::sync::OnceLock;
        static INSTANCE: OnceLock<NetworkMonitorService> = OnceLock::new();
        INSTANCE.get_or_init(Self::new)
    }

    /// Explicitly sets manual offline mode.
    pub fn set_manual_offline(&self, offline: bool) {
        self.manual_offline.store(offline, Ordering::SeqCst);
    }

    /// Returns true if manual offline toggle is active.
    pub fn is_manual_offline(&self) -> bool {
        self.manual_offline.load(Ordering::SeqCst)
    }

    /// Sets an override for network connectivity (primarily for testing and controlled environments).
    pub fn set_connectivity_override(&self, connectivity: Option<NetworkConnectivity>) {
        let mut guard = self.connectivity_override.lock().unwrap();
        *guard = connectivity;
    }

    /// Sets an override for power state (primarily for testing and controlled environments).
    pub fn set_power_override(&self, power: Option<PowerSaverState>) {
        let mut guard = self.power_override.lock().unwrap();
        *guard = power;
    }

    /// Returns current network connectivity.
    pub fn connectivity(&self) -> NetworkConnectivity {
        if self.is_manual_offline() {
            return NetworkConnectivity::Offline;
        }

        if let Some(forced) = *self.connectivity_override.lock().unwrap() {
            return forced;
        }

        // Detect via gio::NetworkMonitor if available on platform
        #[cfg(unix)]
        {
            use gio::prelude::*;
            let monitor = gio::NetworkMonitor::default();
            if !monitor.is_network_available() {
                return NetworkConnectivity::Offline;
            }
            if monitor.is_network_metered() {
                return NetworkConnectivity::Metered;
            }
            NetworkConnectivity::Online
        }
        #[cfg(not(unix))]
        {
            NetworkConnectivity::Online
        }
    }

    /// Returns true if the network is available and online.
    pub fn is_online(&self) -> bool {
        match self.connectivity() {
            NetworkConnectivity::Online | NetworkConnectivity::Metered => true,
            NetworkConnectivity::Offline => false,
        }
    }

    /// Returns true if the connection is marked as metered / mobile data.
    pub fn is_metered(&self) -> bool {
        self.connectivity() == NetworkConnectivity::Metered
    }

    /// Returns current power state.
    pub fn power_state(&self) -> PowerSaverState {
        if let Some(forced) = *self.power_override.lock().unwrap() {
            return forced;
        }
        PowerSaverState::Normal
    }

    /// Evaluates whether periodic background synchronization should be paused based on active policy.
    pub fn should_pause_background_sync(&self, settings: &AppSettings) -> bool {
        if !self.is_online() || settings.offline_mode() {
            return true;
        }

        if self.is_metered() && settings.network_offline_on_metered() {
            return true;
        }

        if self.power_state() != PowerSaverState::Normal
            && settings.network_power_saver_sync_pause()
        {
            return true;
        }

        false
    }

    /// Returns human-readable status description for UI status bars and tooltips.
    pub fn status_description(&self, settings: &AppSettings) -> String {
        if settings.offline_mode() || self.is_manual_offline() {
            return "Offline mode active".to_string();
        }

        match self.connectivity() {
            NetworkConnectivity::Offline => "No network connection (Offline)".to_string(),
            NetworkConnectivity::Metered => {
                if settings.network_offline_on_metered() {
                    "Metered connection — synchronization paused".to_string()
                } else {
                    "Metered connection (Cellular / Hotspot)".to_string()
                }
            }
            NetworkConnectivity::Online => {
                if self.power_state() != PowerSaverState::Normal
                    && settings.network_power_saver_sync_pause()
                {
                    "Online — Power Saver active (sync paused)".to_string()
                } else {
                    "Online (Connected)".to_string()
                }
            }
        }
    }

    /// Resolves effective proxy URI based on configured proxy mode.
    pub fn effective_proxy_uri(&self, settings: &AppSettings) -> Option<String> {
        let mode = ProxyMode::from_str(&settings.network_proxy_mode());
        match mode {
            ProxyMode::None => None,
            ProxyMode::Manual => {
                let host = settings.network_proxy_http_host();
                let port = settings.network_proxy_http_port();
                if host.trim().is_empty() {
                    None
                } else {
                    Some(format!("http://{}:{port}", host.trim()))
                }
            }
            ProxyMode::System => {
                if let Ok(proxy) = std::env::var("HTTP_PROXY").or_else(|_| std::env::var("http_proxy")) {
                    if !proxy.trim().is_empty() {
                        return Some(proxy.trim().to_string());
                    }
                }
                None
            }
        }
    }

    /// Creates a complete status snapshot.
    pub fn snapshot(&self, settings: &AppSettings) -> NetworkStatusSnapshot {
        NetworkStatusSnapshot {
            connectivity: self.connectivity(),
            power_state: self.power_state(),
            proxy_mode: ProxyMode::from_str(&settings.network_proxy_mode()),
            proxy_host: settings.network_proxy_http_host(),
            proxy_port: settings.network_proxy_http_port(),
            background_sync_paused: self.should_pause_background_sync(settings),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_mode_parsing_and_serialization() {
        assert_eq!(ProxyMode::from_str("system"), ProxyMode::System);
        assert_eq!(ProxyMode::from_str("SYSTEM"), ProxyMode::System);
        assert_eq!(ProxyMode::from_str("none"), ProxyMode::None);
        assert_eq!(ProxyMode::from_str("direct"), ProxyMode::None);
        assert_eq!(ProxyMode::from_str("manual"), ProxyMode::Manual);
        assert_eq!(ProxyMode::from_str("custom"), ProxyMode::Manual);
        assert_eq!(ProxyMode::from_str("other"), ProxyMode::System);

        assert_eq!(ProxyMode::System.as_str(), "system");
        assert_eq!(ProxyMode::None.as_str(), "none");
        assert_eq!(ProxyMode::Manual.as_str(), "manual");
    }

    #[test]
    fn network_monitor_policy_evaluation() {
        let service = NetworkMonitorService::new();
        let backend = gio::memory_settings_backend_new();
        let settings =
            AppSettings::try_new_with_backend(&backend).expect("settings should load");

        // 1. Online, normal power -> sync allowed
        service.set_connectivity_override(Some(NetworkConnectivity::Online));
        service.set_power_override(Some(PowerSaverState::Normal));
        assert!(service.is_online());
        assert!(!service.is_metered());
        assert!(!service.should_pause_background_sync(&settings));

        // 2. Offline -> sync paused
        service.set_connectivity_override(Some(NetworkConnectivity::Offline));
        assert!(!service.is_online());
        assert!(service.should_pause_background_sync(&settings));

        // 3. Metered connection with network_offline_on_metered
        service.set_connectivity_override(Some(NetworkConnectivity::Metered));
        assert!(service.is_online());
        assert!(service.is_metered());
        settings.set_network_offline_on_metered(false);
        assert!(!service.should_pause_background_sync(&settings));
        settings.set_network_offline_on_metered(true);
        assert!(service.should_pause_background_sync(&settings));

        // 4. Power saver mode with network_power_saver_sync_pause
        service.set_connectivity_override(Some(NetworkConnectivity::Online));
        settings.set_network_offline_on_metered(false);
        service.set_power_override(Some(PowerSaverState::PowerSaver));
        settings.set_network_power_saver_sync_pause(false);
        assert!(!service.should_pause_background_sync(&settings));
        settings.set_network_power_saver_sync_pause(true);
        assert!(service.should_pause_background_sync(&settings));

        // 5. Manual offline mode
        service.set_power_override(Some(PowerSaverState::Normal));
        service.set_manual_offline(true);
        assert_eq!(service.connectivity(), NetworkConnectivity::Offline);
        assert!(!service.is_online());
        assert!(service.should_pause_background_sync(&settings));
        service.set_manual_offline(false);
    }

    #[test]
    fn proxy_uri_generation() {
        let service = NetworkMonitorService::new();
        let backend = gio::memory_settings_backend_new();
        let settings =
            AppSettings::try_new_with_backend(&backend).expect("settings should load");

        settings.set_network_proxy_mode("none");
        assert_eq!(service.effective_proxy_uri(&settings), None);

        settings.set_network_proxy_mode("manual");
        settings.set_network_proxy_http_host("proxy.internal.corp");
        settings.set_network_proxy_http_port(3128);
        assert_eq!(
            service.effective_proxy_uri(&settings),
            Some("http://proxy.internal.corp:3128".to_string())
        );
    }
}
