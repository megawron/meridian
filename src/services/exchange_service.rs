// SPDX-License-Identifier: GPL-3.0-or-later
//
// exchange_service — Abstraction layer for Exchange EWS and Microsoft Graph API.
//
// Supports:
// - GAL (Global Address List) search
// - Out-of-Office (OOF) status retrieval
// - OAuth2 / MSAL authentication tokens

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct GalContact {
    pub display_name: String,
    pub email_address: String,
    pub job_title: String,
    pub department: String,
    pub office_location: String,
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExchangeAuthStatus {
    Authenticated,
    TokenExpired,
    NeedOAuthConsent,
    Disconnected,
}

#[allow(dead_code)]
pub struct ExchangeService {
    pub server_url: String,
    pub username: String,
    pub auth_status: ExchangeAuthStatus,
}

#[allow(dead_code)]
impl ExchangeService {
    pub fn new(server_url: &str, username: &str) -> Self {
        Self {
            server_url: server_url.to_string(),
            username: username.to_string(),
            auth_status: ExchangeAuthStatus::Disconnected,
        }
    }

    /// Search the Global Address List (GAL).
    pub async fn search_gal(&self, query: &str) -> Result<Vec<GalContact>, String> {
        tracing::debug!(query, "Exchange GAL search requested");
        Err("Exchange backend is not connected".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exchange_service_creation() {
        let service = ExchangeService::new(
            "https://outlook.office365.com/EWS/Exchange.asmx",
            "martin@company.de",
        );
        assert_eq!(service.auth_status, ExchangeAuthStatus::Disconnected);
    }
}
