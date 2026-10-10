//! Roblox Open Cloud access (File > Phaedrus Settings > API Manager).
//!
//! The user's API key lives in the OS credential store (Windows Credential
//! Manager), never in `settings.json`, place files or Output. It is only
//! ever sent to [`API_HOST`]; follow-up downloads from the CDN URLs that
//! Open Cloud hands back go out without it.

use std::fmt;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use bevy::prelude::*;
use parking_lot::Mutex;
use serde::Deserialize;

pub const API_HOST: &str = "apis.roblox.com";
const KEYRING_SERVICE: &str = "phaedrus";
const KEYRING_USER: &str = "opencloud-api-key";

/// An Open Cloud API key. `Debug`/`Display` never print it.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(Arc<str>);

impl ApiKey {
    pub fn new(key: &str) -> Option<Self> {
        let key = key.trim();
        (!key.is_empty() && !key.chars().any(char::is_whitespace)).then(|| Self(key.into()))
    }

    /// The secret itself, for the `x-api-key` header only.
    fn expose(&self) -> &str {
        &self.0
    }

    /// Last four characters, to tell keys apart in the UI.
    pub fn hint(&self) -> String {
        let n = self.0.chars().count();
        let tail: String = self.0.chars().skip(n.saturating_sub(4)).collect();
        format!("••••{tail}")
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(<redacted>)")
    }
}

impl fmt::Display for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum KeyStatus {
    Unchecked,
    Checking,
    Valid(String),
    Invalid(String),
}

/// The active API key and the API Manager's state.
#[derive(Resource)]
pub struct ApiAuth {
    key: Option<ApiKey>,
    pub status: KeyStatus,
    /// Set when the credential store could not be read or written; the key
    /// then only lasts for this session.
    pub storage_error: Option<String>,
    /// Universe ID typed into the API Manager's "Test" field.
    pub test_universe: String,
    check: Option<Mutex<Receiver<KeyStatus>>>,
}

impl Default for ApiAuth {
    fn default() -> Self {
        let mut auth = Self::empty();
        // Benchmark and screenshot runs never touch the user's credentials.
        if std::env::var_os("PHAEDRUS_BENCH").is_some() || std::env::var_os("PHAEDRUS_SCREENSHOT").is_some() {
            return auth;
        }
        match keyring_entry().and_then(|e| e.get_password()) {
            Ok(k) => auth.key = ApiKey::new(&k),
            Err(keyring::Error::NoEntry) => {}
            Err(e) => auth.storage_error = Some(format!("Could not read the credential store: {e}")),
        }
        auth
    }
}

fn keyring_entry() -> keyring::Result<keyring::Entry> {
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)
}

impl ApiAuth {
    pub fn empty() -> Self {
        Self { key: None, status: KeyStatus::Unchecked, storage_error: None, test_universe: String::new(), check: None }
    }

    pub fn key(&self) -> Option<&ApiKey> {
        self.key.as_ref()
    }

    /// Saves `key` to the credential store and makes it active.
    pub fn set_key(&mut self, key: &str) -> Result<(), String> {
        let key = ApiKey::new(key).ok_or("The key is empty or contains spaces.")?;
        self.storage_error = keyring_entry()
            .and_then(|e| e.set_password(key.expose()))
            .err()
            .map(|e| format!("Could not save to the credential store ({e}); the key will be forgotten when Phaedrus closes."));
        self.key = Some(key);
        self.status = KeyStatus::Unchecked;
        self.check = None;
        Ok(())
    }

    /// Forgets the key here and in the credential store.
    pub fn remove_key(&mut self) {
        self.key = None;
        self.status = KeyStatus::Unchecked;
        self.check = None;
        self.storage_error = match keyring_entry().and_then(|e| e.delete_credential()) {
            Ok(()) | Err(keyring::Error::NoEntry) => None,
            Err(e) => Some(format!("Could not remove the key from the credential store: {e}")),
        };
    }

    /// Checks the key against `GET /cloud/v2/universes/{id}` in the
    /// background; see [`Self::poll`].
    pub fn start_check(&mut self) {
        let Some(key) = self.key.clone() else { return };
        let Ok(universe) = self.test_universe.trim().parse::<u64>() else {
            self.status = KeyStatus::Invalid("Enter a numeric universe ID to test with.".into());
            return;
        };
        let (tx, rx) = channel();
        let spawned = std::thread::Builder::new().name("opencloud-check".into()).spawn(move || {
            let _ = tx.send(check_universe(&OpenCloud::new(key), universe));
        });
        if spawned.is_ok() {
            self.status = KeyStatus::Checking;
            self.check = Some(Mutex::new(rx));
        }
    }

    pub fn poll(&mut self) {
        if let Some(status) = self.check.as_ref().and_then(|rx| rx.lock().try_recv().ok()) {
            self.status = status;
            self.check = None;
        }
    }
}

fn check_universe(client: &OpenCloud, universe: u64) -> KeyStatus {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Universe {
        display_name: Option<String>,
    }
    match client.get_json::<Universe>(&format!("/cloud/v2/universes/{universe}")) {
        Ok(u) => KeyStatus::Valid(format!(
            "Key accepted. Universe {universe}: {}",
            u.display_name.unwrap_or_else(|| "(unnamed)".into())
        )),
        Err(e) => KeyStatus::Invalid(e.to_string()),
    }
}

#[derive(Debug)]
pub enum ApiError {
    /// 401: wrong, expired, revoked or IP-restricted key.
    Unauthorized,
    /// 403: the key lacks a scope or access to the resource.
    Forbidden,
    NotFound,
    RateLimited { retry_after: Option<Duration> },
    Status(u16),
    Network(String),
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ApiError::Unauthorized => {
                f.write_str("Key rejected (401): it is wrong, expired or revoked, or this IP is not in its allowed list.")
            }
            ApiError::Forbidden => f.write_str("Access denied (403): the key lacks the required scope or access to this resource."),
            ApiError::NotFound => f.write_str("Not found (404)."),
            ApiError::RateLimited { .. } => f.write_str("Rate limited (429)."),
            ApiError::Status(s) => write!(f, "Open Cloud returned HTTP {s}."),
            ApiError::Network(e) => write!(f, "Network error: {e}"),
        }
    }
}

/// A minimal Open Cloud client.
pub struct OpenCloud {
    key: ApiKey,
    agent: ureq::Agent,
}

impl OpenCloud {
    pub fn new(key: ApiKey) -> Self {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(60)))
            .build()
            .new_agent();
        Self { key, agent }
    }

    fn get_api(&self, path: &str) -> Result<ureq::http::Response<ureq::Body>, ApiError> {
        let resp = self
            .agent
            .get(format!("https://{API_HOST}{path}"))
            .header("x-api-key", self.key.expose())
            .call()
            .map_err(|e| ApiError::Network(e.to_string()))?;
        check_status(resp)
    }

    pub fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, ApiError> {
        self.get_api(path)?.body_mut().read_json().map_err(|e| ApiError::Network(e.to_string()))
    }

    /// Downloads an asset's content via the Asset Delivery API (needs the
    /// `legacy-asset:manage` scope).
    pub fn download_asset(&self, id: u64) -> Result<Vec<u8>, ApiError> {
        #[derive(Deserialize)]
        struct Location {
            location: String,
        }
        let loc: Location = self.get_json(&format!("/asset-delivery-api/v1/assetId/{id}"))?;
        // The CDN is a different host: never send the key there.
        let resp = self
            .agent
            .get(&loc.location)
            .header("Accept-Encoding", "gzip")
            .call()
            .map_err(|e| ApiError::Network(e.to_string()))?;
        check_status(resp)?
            .body_mut()
            .with_config()
            .limit(256 * 1024 * 1024)
            .read_to_vec()
            .map_err(|e| ApiError::Network(e.to_string()))
    }
}

fn check_status(resp: ureq::http::Response<ureq::Body>) -> Result<ureq::http::Response<ureq::Body>, ApiError> {
    match resp.status().as_u16() {
        200..300 => Ok(resp),
        401 => Err(ApiError::Unauthorized),
        403 => Err(ApiError::Forbidden),
        404 => Err(ApiError::NotFound),
        429 => {
            let retry_after = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse().ok())
                .map(Duration::from_secs);
            Err(ApiError::RateLimited { retry_after })
        }
        s => Err(ApiError::Status(s)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_is_never_formatted() {
        let key = ApiKey::new("  SECRET-abcd1234  ").unwrap();
        assert_eq!(key.expose(), "SECRET-abcd1234");
        for s in [format!("{key:?}"), format!("{key}"), format!("{:?}", Some(key.clone()))] {
            assert!(!s.contains("SECRET"), "{s}");
        }
        assert_eq!(key.hint(), "••••1234");
        assert!(ApiKey::new("   ").is_none());
        assert!(ApiKey::new("two words").is_none());
    }
}
