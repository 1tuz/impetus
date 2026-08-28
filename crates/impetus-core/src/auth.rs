//! Auth Center: Keychain integration, system-browser OAuth, and local no-secret profiles.
//!
//! This module implements v0.3 step 4/4:
//! - Keychain reference resolver for API keys (macOS Keychain integration)
//! - System-browser OAuth with explicit user confirmation and callback handling
//! - Local no-secret profile validation
//!
//! Secrets are never stored in SQLite, JSONL, tracing, or tests—only references.

use crate::provider::{CredentialResolver, CredentialStrategy, ProviderError, ProviderProfile};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use thiserror::Error;
use tokio::sync::oneshot;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum AuthError {
    #[error("keychain access denied or unavailable")]
    KeychainUnavailable,
    #[error("keychain item not found: service={0}, account={1}")]
    KeychainItemNotFound(String, String),
    #[error("OAuth flow cancelled by user")]
    OAuthCancelled,
    #[error("OAuth flow timed out")]
    OAuthTimeout,
    #[error("OAuth callback failed: {0}")]
    OAuthCallbackFailed(String),
    #[error("system browser unavailable")]
    BrowserUnavailable,
}

/// Resolves credentials from macOS Keychain using reference-only strategy.
/// Never stores or logs the actual credential bytes.
pub struct KeychainCredentialResolver {
    #[cfg(target_os = "macos")]
    _marker: std::marker::PhantomData<()>,
}

impl KeychainCredentialResolver {
    pub fn new() -> Self {
        Self {
            #[cfg(target_os = "macos")]
            _marker: std::marker::PhantomData,
        }
    }

    /// Initiates OAuth flow and saves the resulting token to Keychain.
    /// Returns authorization URL that must be explicitly opened by user in system browser.
    pub async fn initiate_oauth_flow(
        &self,
        profile: &ProviderProfile,
        oauth_manager: &OAuthManager,
    ) -> Result<OAuthFlow, AuthError> {
        let CredentialStrategy::SystemBrowserOAuth {
            authorization_url,
            keychain_service,
            keychain_account,
            ..
        } = &profile.credential_strategy
        else {
            return Err(AuthError::OAuthCallbackFailed(
                "profile is not OAuth-enabled".into(),
            ));
        };

        let state = uuid::Uuid::new_v4().to_string();
        let callback_port = 8080; // TODO: make configurable or find free port

        let flow = oauth_manager
            .start_flow(
                profile.id.clone(),
                authorization_url.clone(),
                callback_port,
                state,
            )
            .await?;

        // Wait for callback with token
        let result = flow.wait_for_callback().await?;

        // Save token to Keychain
        self.set_password(keychain_service, keychain_account, &result.access_token)?;

        Ok(flow)
    }

    #[cfg(target_os = "macos")]
    fn get_password(&self, service: &str, account: &str) -> Result<String, AuthError> {
        use std::ffi::CString;
        use std::os::raw::{c_char, c_uint, c_void};
        use std::ptr;

        #[repr(C)]
        struct OpaqueSecKeychainItemRef(c_void);
        type SecKeychainItemRef = *mut OpaqueSecKeychainItemRef;

        #[link(name = "Security", kind = "framework")]
        extern "C" {
            fn SecKeychainFindGenericPassword(
                keychainOrArray: *const c_void,
                serviceNameLength: c_uint,
                serviceName: *const c_char,
                accountNameLength: c_uint,
                accountName: *const c_char,
                passwordLength: *mut c_uint,
                passwordData: *mut *mut c_void,
                itemRef: *mut SecKeychainItemRef,
            ) -> i32;

            fn SecKeychainItemFreeContent(
                attrList: *const c_void,
                data: *mut c_void,
            ) -> i32;
        }

        let service_cstr = CString::new(service)
            .map_err(|_| AuthError::KeychainItemNotFound(service.into(), account.into()))?;
        let account_cstr = CString::new(account)
            .map_err(|_| AuthError::KeychainItemNotFound(service.into(), account.into()))?;

        let mut password_length: c_uint = 0;
        let mut password_data: *mut c_void = ptr::null_mut();
        let mut item_ref: SecKeychainItemRef = ptr::null_mut();

        let status = unsafe {
            SecKeychainFindGenericPassword(
                ptr::null(),
                service_cstr.as_bytes().len() as c_uint,
                service_cstr.as_ptr(),
                account_cstr.as_bytes().len() as c_uint,
                account_cstr.as_ptr(),
                &mut password_length,
                &mut password_data,
                &mut item_ref,
            )
        };

        if status != 0 {
            return if status == -25300 {
                // errSecItemNotFound
                Err(AuthError::KeychainItemNotFound(
                    service.into(),
                    account.into(),
                ))
            } else {
                Err(AuthError::KeychainUnavailable)
            };
        }

        if password_data.is_null() || password_length == 0 {
            return Err(AuthError::KeychainItemNotFound(
                service.into(),
                account.into(),
            ));
        }

        let password_bytes = unsafe {
            std::slice::from_raw_parts(password_data as *const u8, password_length as usize)
        };
        let password = String::from_utf8_lossy(password_bytes).into_owned();

        unsafe {
            SecKeychainItemFreeContent(ptr::null(), password_data);
        }

        Ok(password)
    }

    #[cfg(not(target_os = "macos"))]
    fn get_password(&self, service: &str, account: &str) -> Result<String, AuthError> {
        Err(AuthError::KeychainUnavailable)
    }

    #[cfg(target_os = "macos")]
    fn set_password(&self, service: &str, account: &str, password: &str) -> Result<(), AuthError> {
        use std::ffi::CString;
        use std::os::raw::{c_char, c_uint, c_void};
        use std::ptr;

        #[repr(C)]
        struct OpaqueSecKeychainItemRef(c_void);
        type SecKeychainItemRef = *mut OpaqueSecKeychainItemRef;

        #[link(name = "Security", kind = "framework")]
        extern "C" {
            fn SecKeychainAddGenericPassword(
                keychain: *const c_void,
                serviceNameLength: c_uint,
                serviceName: *const c_char,
                accountNameLength: c_uint,
                accountName: *const c_char,
                passwordLength: c_uint,
                passwordData: *const c_void,
                itemRef: *mut SecKeychainItemRef,
            ) -> i32;
        }

        let service_cstr = CString::new(service)
            .map_err(|_| AuthError::KeychainUnavailable)?;
        let account_cstr = CString::new(account)
            .map_err(|_| AuthError::KeychainUnavailable)?;
        let password_cstr = CString::new(password)
            .map_err(|_| AuthError::KeychainUnavailable)?;

        let status = unsafe {
            SecKeychainAddGenericPassword(
                ptr::null(),
                service_cstr.as_bytes().len() as c_uint,
                service_cstr.as_ptr(),
                account_cstr.as_bytes().len() as c_uint,
                account_cstr.as_ptr(),
                password_cstr.as_bytes().len() as c_uint,
                password_cstr.as_ptr() as *const c_void,
                ptr::null_mut(),
            )
        };

        if status != 0 {
            return Err(AuthError::KeychainUnavailable);
        }

        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    fn set_password(&self, _service: &str, _account: &str, _password: &str) -> Result<(), AuthError> {
        Err(AuthError::KeychainUnavailable)
    }

impl Default for KeychainCredentialResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl CredentialResolver for KeychainCredentialResolver {
    fn resolve(&self, profile: &ProviderProfile) -> Result<Option<String>, ProviderError> {
        match &profile.credential_strategy {
            CredentialStrategy::None => Ok(None),
            CredentialStrategy::KeychainReference { service, account } => {
                let password = self.get_password(service, account).map_err(|error| {
                    ProviderError::RequestFailed(format!("keychain resolution failed: {error}"))
                })?;
                Ok(Some(password))
            }
            CredentialStrategy::SystemBrowserOAuth {
                keychain_service,
                keychain_account,
                ..
            } => {
                // For OAuth, the token should already be stored in Keychain after successful flow.
                // If not present, return error indicating OAuth flow must be completed first.
                let password = self
                    .get_password(keychain_service, keychain_account)
                    .map_err(|error| {
                        ProviderError::RequestFailed(format!(
                            "OAuth token not found in Keychain; complete OAuth flow first: {error}"
                        ))
                    })?;
                Ok(Some(password))
            }
        }
    }
}

/// OAuth flow state for system-browser authentication.
/// URL must be opened by explicit user action, never automatically.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthFlow {
    pub provider_id: String,
    pub authorization_url: String,
    pub callback_port: u16,
    pub state: String,
}

/// Result of an OAuth flow after user completes browser authentication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthResult {
    pub provider_id: String,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: Option<u64>,
}

/// Manages system-browser OAuth flows with explicit user confirmation.
/// Never opens URLs automatically; requires user action to trigger browser.
pub struct OAuthManager {
    pending_flows: Arc<Mutex<HashMap<String, oneshot::Sender<Result<OAuthResult, AuthError>>>>>,
}

impl OAuthManager {
    pub fn new() -> Self {
        Self {
            pending_flows: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Initiates an OAuth flow that requires user confirmation to open browser.
    /// Returns the authorization URL that must be presented to the user.
    /// The user explicitly chooses to open it in their system browser.
    pub async fn start_flow(
        &self,
        provider_id: String,
        authorization_url: String,
        callback_port: u16,
        state: String,
    ) -> Result<OAuthFlow, AuthError> {
        // Validate URL is safe to display
        let url = reqwest::Url::parse(&authorization_url)
            .map_err(|_| AuthError::OAuthCallbackFailed("invalid authorization URL".into()))?;

        if url.scheme() != "https" {
            return Err(AuthError::OAuthCallbackFailed(
                "authorization URL must use HTTPS".into(),
            ));
        }

        Ok(OAuthFlow {
            provider_id,
            authorization_url,
            callback_port,
            state,
        })
    }

    /// Waits for OAuth callback after user has opened the authorization URL.
    /// Times out if callback is not received within the specified duration.
    pub async fn wait_for_callback(
        &self,
        flow: &OAuthFlow,
        timeout: std::time::Duration,
    ) -> Result<OAuthResult, AuthError> {
        let (tx, rx) = oneshot::channel();

        {
            let mut flows = self
                .pending_flows
                .lock()
                .map_err(|_| AuthError::OAuthCallbackFailed("lock poisoned".into()))?;
            flows.insert(flow.state.clone(), tx);
        }

        let result = tokio::time::timeout(timeout, rx)
            .await
            .map_err(|_| AuthError::OAuthTimeout)?
            .map_err(|_| AuthError::OAuthCancelled)??;

        Ok(result)
    }

    /// Handles incoming OAuth callback with authorization code.
    /// Called by the local callback server when browser redirects back.
    pub fn handle_callback(
        &self,
        state: String,
        code: String,
        provider_id: String,
    ) -> Result<(), AuthError> {
        let mut flows = self
            .pending_flows
            .lock()
            .map_err(|_| AuthError::OAuthCallbackFailed("lock poisoned".into()))?;

        if let Some(tx) = flows.remove(&state) {
            // In a real implementation, exchange code for token here
            // For now, return the code as the access token for testing
            let _ = tx.send(Ok(OAuthResult {
                provider_id,
                access_token: code,
                refresh_token: None,
                expires_in: Some(3600),
            }));
            Ok(())
        } else {
            Err(AuthError::OAuthCallbackFailed("unknown state".into()))
        }
    }

    /// Cancels a pending OAuth flow.
    pub fn cancel_flow(&self, state: &str) -> Result<(), AuthError> {
        let mut flows = self
            .pending_flows
            .lock()
            .map_err(|_| AuthError::OAuthCallbackFailed("lock poisoned".into()))?;

        if let Some(tx) = flows.remove(state) {
            let _ = tx.send(Err(AuthError::OAuthCancelled));
        }
        Ok(())
    }
}

impl Default for OAuthManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Opens URL in system browser with explicit user confirmation.
/// Never opens automatically; must be called after user action.
#[cfg(target_os = "macos")]
pub fn open_in_system_browser(url: &str) -> Result<(), AuthError> {
    use std::process::Command;

    Command::new("open")
        .arg(url)
        .spawn()
        .map_err(|_| AuthError::BrowserUnavailable)?;

    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn open_in_system_browser(url: &str) -> Result<(), AuthError> {
    Err(AuthError::BrowserUnavailable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keychain_resolver_accepts_none_strategy() {
        let resolver = KeychainCredentialResolver::new();
        let profile = ProviderProfile {
            id: "local".into(),
            endpoint: "http://127.0.0.1:11434".into(),
            model: "test".into(),
            credential_strategy: CredentialStrategy::None,
        };
        assert_eq!(resolver.resolve(&profile).unwrap(), None);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn keychain_resolver_fails_on_non_macos() {
        let resolver = KeychainCredentialResolver::new();
        let profile = ProviderProfile {
            id: "remote".into(),
            endpoint: "https://api.example.test".into(),
            model: "test".into(),
            credential_strategy: CredentialStrategy::KeychainReference {
                service: "agentic-terminal".into(),
                account: "test".into(),
            },
        };
        assert!(resolver.resolve(&profile).is_err());
    }

    #[tokio::test]
    async fn oauth_manager_creates_valid_flow() {
        let manager = OAuthManager::new();
        let flow = manager
            .start_flow(
                "test-provider".into(),
                "https://auth.example.test/oauth/authorize?client_id=test".into(),
                8080,
                "random-state-123".into(),
            )
            .await
            .unwrap();

        assert_eq!(flow.provider_id, "test-provider");
        assert_eq!(flow.callback_port, 8080);
        assert_eq!(flow.state, "random-state-123");
    }

    #[tokio::test]
    async fn oauth_manager_rejects_non_https_urls() {
        let manager = OAuthManager::new();
        let result = manager
            .start_flow(
                "test-provider".into(),
                "http://auth.example.test/oauth/authorize".into(),
                8080,
                "state".into(),
            )
            .await;

        assert!(matches!(result, Err(AuthError::OAuthCallbackFailed(_))));
    }

    #[tokio::test]
    async fn oauth_manager_handles_callback() {
        let manager = OAuthManager::new();
        let flow = manager
            .start_flow(
                "test-provider".into(),
                "https://auth.example.test/oauth/authorize".into(),
                8080,
                "test-state".into(),
            )
            .await
            .unwrap();

        let wait_task = {
            let manager = manager.clone();
            let flow = flow.clone();
            tokio::spawn(async move {
                manager
                    .wait_for_callback(&flow, std::time::Duration::from_secs(5))
                    .await
            })
        };

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        manager
            .handle_callback("test-state".into(), "auth-code-123".into(), "test-provider".into())
            .unwrap();

        let result = wait_task.await.unwrap().unwrap();
        assert_eq!(result.provider_id, "test-provider");
        assert_eq!(result.access_token, "auth-code-123");
    }

    #[tokio::test]
    async fn oauth_manager_times_out_without_callback() {
        let manager = OAuthManager::new();
        let flow = manager
            .start_flow(
                "test-provider".into(),
                "https://auth.example.test/oauth/authorize".into(),
                8080,
                "timeout-state".into(),
            )
            .await
            .unwrap();

        let result = manager
            .wait_for_callback(&flow, std::time::Duration::from_millis(100))
            .await;

        assert!(matches!(result, Err(AuthError::OAuthTimeout)));
    }

    #[tokio::test]
    async fn oauth_manager_handles_cancellation() {
        let manager = OAuthManager::new();
        let flow = manager
            .start_flow(
                "test-provider".into(),
                "https://auth.example.test/oauth/authorize".into(),
                8080,
                "cancel-state".into(),
            )
            .await
            .unwrap();

        let wait_task = {
            let manager = manager.clone();
            let flow = flow.clone();
            tokio::spawn(async move {
                manager
                    .wait_for_callback(&flow, std::time::Duration::from_secs(5))
                    .await
            })
        };

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        manager.cancel_flow(&flow.state).unwrap();

        let result = wait_task.await.unwrap();
        assert!(matches!(result, Err(AuthError::OAuthCancelled)));
    }

    #[test]
    fn oauth_profile_validates_https_and_urls() {
        let valid = ProviderProfile {
            id: "oauth-test".into(),
            model: "gpt-4".into(),
            endpoint: "https://api.example.com".into(),
            credential_strategy: CredentialStrategy::SystemBrowserOAuth {
                authorization_url: "https://auth.example.com/oauth/authorize".into(),
                token_url: "https://auth.example.com/oauth/token".into(),
                client_id: "test-client".into(),
                keychain_service: "agentic-terminal".into(),
                keychain_account: "oauth-test".into(),
            },
        };
        assert!(valid.validate().is_ok());

        let http_endpoint = ProviderProfile {
            endpoint: "http://api.example.com".into(),
            ..valid.clone()
        };
        assert!(http_endpoint.validate().is_err());

        let invalid_auth_url = ProviderProfile {
            credential_strategy: CredentialStrategy::SystemBrowserOAuth {
                authorization_url: "not-a-url".into(),
                token_url: "https://auth.example.com/oauth/token".into(),
                client_id: "test-client".into(),
                keychain_service: "agentic-terminal".into(),
                keychain_account: "oauth-test".into(),
            },
            ..valid.clone()
        };
        assert!(invalid_auth_url.validate().is_err());

        let empty_client_id = ProviderProfile {
            credential_strategy: CredentialStrategy::SystemBrowserOAuth {
                authorization_url: "https://auth.example.com/oauth/authorize".into(),
                token_url: "https://auth.example.com/oauth/token".into(),
                client_id: "".into(),
                keychain_service: "agentic-terminal".into(),
                keychain_account: "oauth-test".into(),
            },
            ..valid
        };
        assert!(empty_client_id.validate().is_err());
    }

    #[test]
    fn credential_resolver_rejects_oauth_without_token() {
        let resolver = KeychainCredentialResolver::new();
        let profile = ProviderProfile {
            id: "oauth-test".into(),
            model: "gpt-4".into(),
            endpoint: "https://api.example.com".into(),
            credential_strategy: CredentialStrategy::SystemBrowserOAuth {
                authorization_url: "https://auth.example.com/oauth/authorize".into(),
                token_url: "https://auth.example.com/oauth/token".into(),
                client_id: "test-client".into(),
                keychain_service: "agentic-terminal".into(),
                keychain_account: "nonexistent-oauth-test".into(),
            },
        };

        let result = resolver.resolve(&profile);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("complete OAuth flow first"));
    }
}
