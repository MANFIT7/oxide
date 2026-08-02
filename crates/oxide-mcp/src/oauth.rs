use crate::http::{BearerTokenProvider, HttpAuthChallenge};
use crate::secret::{redact_oauth_secrets, SecretString};
use async_trait::async_trait;
use rmcp::transport::auth::{
    AuthError as RmcpAuthError, AuthorizationManager, AuthorizationMetadata,
    AuthorizationMetadataSource, AuthorizationRequest, AuthorizationSession,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::instrument::WithSubscriber;

pub use rmcp::transport::auth::{CredentialStore, StoredCredentials};

const DEFAULT_CALLBACK_PATH: &str = "/oauth/callback";
const MAX_CALLBACK_REQUEST_BYTES: usize = 16 * 1024;

/// OAuth client identity mechanisms in MCP priority order.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum OAuthClientIdentity {
    PreRegistered {
        client_id: String,
        client_secret: Option<SecretString>,
    },
    ClientMetadataUrl(String),
    #[default]
    DynamicRegistration,
}

/// Inputs for one authorization-code flow.
#[derive(Clone, Debug)]
pub struct OAuthStartRequest {
    redirect_uri: String,
    scopes: Vec<String>,
    client_name: String,
    identity: OAuthClientIdentity,
    challenge: Option<HttpAuthChallenge>,
}

impl OAuthStartRequest {
    pub fn new(redirect_uri: impl Into<String>) -> Self {
        Self {
            redirect_uri: redirect_uri.into(),
            scopes: Vec::new(),
            client_name: "Oxide".to_string(),
            identity: OAuthClientIdentity::DynamicRegistration,
            challenge: None,
        }
    }

    pub fn with_scopes<I, S>(mut self, scopes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.scopes = scopes.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_client_name(mut self, client_name: impl Into<String>) -> Self {
        self.client_name = client_name.into();
        self
    }

    pub fn with_challenge(mut self, challenge: HttpAuthChallenge) -> Self {
        self.challenge = Some(challenge);
        self
    }

    pub fn with_preregistered_client(
        mut self,
        client_id: impl Into<String>,
        client_secret: Option<SecretString>,
    ) -> Self {
        self.identity = OAuthClientIdentity::PreRegistered {
            client_id: client_id.into(),
            client_secret,
        };
        self
    }

    pub fn with_client_metadata_url(mut self, url: impl Into<String>) -> Self {
        self.identity = OAuthClientIdentity::ClientMetadataUrl(url.into());
        self
    }

    fn challenge_header(&self) -> Option<&str> {
        self.challenge
            .as_ref()
            .and_then(HttpAuthChallenge::www_authenticate)
    }

    fn into_rmcp(self) -> AuthorizationRequest {
        let mut request = AuthorizationRequest::new(self.redirect_uri)
            .with_client_name(self.client_name)
            .with_application_type("native");
        if !self.scopes.is_empty() {
            request = request.with_scopes(self.scopes);
        }
        if let Some(challenge) = self.challenge.and_then(HttpAuthChallenge::into_header) {
            request = request.with_challenge(challenge);
        }
        match self.identity {
            OAuthClientIdentity::PreRegistered {
                client_id,
                client_secret,
            } => {
                request = request.with_preregistered_client(client_id);
                if let Some(client_secret) = client_secret {
                    request = request.with_client_secret(client_secret.expose_secret());
                }
            }
            OAuthClientIdentity::ClientMetadataUrl(url) => {
                request = request.with_client_metadata_url(url);
            }
            OAuthClientIdentity::DynamicRegistration => {}
        }
        request
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OAuthDiscoverySource {
    ProtectedResourceMetadata,
    AuthorizationServerMetadata,
    LegacyEndpointFallback,
    Other,
}

impl From<AuthorizationMetadataSource> for OAuthDiscoverySource {
    fn from(source: AuthorizationMetadataSource) -> Self {
        match source {
            AuthorizationMetadataSource::ProtectedResourceMetadata => {
                Self::ProtectedResourceMetadata
            }
            AuthorizationMetadataSource::AuthorizationServerMetadata => {
                Self::AuthorizationServerMetadata
            }
            AuthorizationMetadataSource::LegacyEndpointFallback => Self::LegacyEndpointFallback,
            _ => Self::Other,
        }
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct OAuthAuthorizationLaunch {
    pub authorization_url: String,
    pub redirect_uri: String,
    pub discovery_source: OAuthDiscoverySource,
}

impl std::fmt::Debug for OAuthAuthorizationLaunch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OAuthAuthorizationLaunch")
            .field(
                "authorization_url",
                &redact_oauth_secrets(&self.authorization_url),
            )
            .field("redirect_uri", &self.redirect_uri)
            .field("discovery_source", &self.discovery_source)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OAuthCoordinatorStatus {
    NeedsAuthorization,
    WaitingForCallback,
    Authorized,
}

#[derive(Debug, thiserror::Error)]
pub enum OAuthCoordinatorError {
    #[error("OAuth authorization is required")]
    AuthorizationRequired,
    #[error("OAuth refresh token was rejected; authorization is required again")]
    RefreshRejected,
    #[error("OAuth requires additional scope: {required_scope}")]
    InsufficientScope {
        required_scope: String,
        upgrade_url: Option<String>,
    },
    #[error("OAuth callback was rejected: {code}")]
    CallbackRejected {
        code: String,
        description: Option<String>,
    },
    #[error("OAuth coordinator state is {actual:?}; expected {expected}")]
    InvalidState {
        actual: OAuthCoordinatorStatus,
        expected: &'static str,
    },
    #[error("invalid MCP OAuth endpoint: {0}")]
    InvalidEndpoint(String),
    #[error("OAuth operation failed: {0}")]
    Operation(String),
}

impl From<RmcpAuthError> for OAuthCoordinatorError {
    fn from(error: RmcpAuthError) -> Self {
        match error {
            RmcpAuthError::AuthorizationRequired => Self::AuthorizationRequired,
            RmcpAuthError::TokenRefreshRejected(_) => Self::RefreshRejected,
            RmcpAuthError::InsufficientScope {
                required_scope,
                upgrade_url,
            } => Self::InsufficientScope {
                required_scope,
                upgrade_url,
            },
            error => Self::Operation(redact_oauth_secrets(&error.to_string())),
        }
    }
}

impl OAuthCoordinatorError {
    pub fn requires_authorization(&self) -> bool {
        matches!(self, Self::AuthorizationRequired | Self::RefreshRejected)
    }
}

enum CoordinatorState {
    NeedsAuthorization(AuthorizationManager),
    WaitingForCallback {
        session: AuthorizationSession,
        discovery_source: OAuthDiscoverySource,
    },
    Authorized(AuthorizationManager),
}

/// Native MCP OAuth coordinator backed by rmcp 3.1 discovery, PKCE, and refresh logic.
pub struct OAuthCoordinator<S>
where
    S: CredentialStore + Clone,
{
    endpoint: String,
    credential_store: S,
    state: Option<CoordinatorState>,
    reauthorization_required: AtomicBool,
}

impl<S> std::fmt::Debug for OAuthCoordinator<S>
where
    S: CredentialStore + Clone + 'static,
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OAuthCoordinator")
            .field("endpoint", &redact_oauth_secrets(&self.endpoint))
            .field("status", &self.status())
            .finish_non_exhaustive()
    }
}

impl<S> OAuthCoordinator<S>
where
    S: CredentialStore + Clone + 'static,
{
    pub async fn new(
        endpoint: impl Into<String>,
        credential_store: S,
    ) -> Result<Self, OAuthCoordinatorError> {
        let endpoint = endpoint.into();
        validate_oauth_endpoint(&endpoint)?;
        let mut manager = AuthorizationManager::new(endpoint.as_str()).await?;
        manager.set_credential_store(credential_store.clone());
        let has_stored_tokens = credential_store
            .load()
            .await?
            .is_some_and(|credentials| credentials.token_response.is_some());
        if has_stored_tokens {
            let resolution = manager.resolve_metadata().await?;
            validate_authorization_metadata(&endpoint, &resolution.metadata)?;
            manager.set_metadata(resolution.metadata);
        }
        let restored = manager.initialize_from_store().await?;
        let state = if restored {
            CoordinatorState::Authorized(manager)
        } else {
            CoordinatorState::NeedsAuthorization(manager)
        };
        Ok(Self {
            endpoint,
            credential_store,
            state: Some(state),
            reauthorization_required: AtomicBool::new(false),
        })
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn status(&self) -> OAuthCoordinatorStatus {
        if self.reauthorization_required.load(Ordering::Acquire) {
            return OAuthCoordinatorStatus::NeedsAuthorization;
        }
        match self.state.as_ref() {
            Some(CoordinatorState::NeedsAuthorization(_)) | None => {
                OAuthCoordinatorStatus::NeedsAuthorization
            }
            Some(CoordinatorState::WaitingForCallback { .. }) => {
                OAuthCoordinatorStatus::WaitingForCallback
            }
            Some(CoordinatorState::Authorized(_)) => OAuthCoordinatorStatus::Authorized,
        }
    }

    pub async fn start_authorization(
        &mut self,
        request: OAuthStartRequest,
    ) -> Result<OAuthAuthorizationLaunch, OAuthCoordinatorError> {
        if self.reauthorization_required.load(Ordering::Acquire) {
            self.reset_authorization_state().await?;
        }
        let challenge = request.challenge_header().map(str::to_string);
        let state = self.take_state();
        let CoordinatorState::NeedsAuthorization(manager) = state else {
            let actual = status_of_state(&state);
            self.state = Some(state);
            return Err(OAuthCoordinatorError::InvalidState {
                actual,
                expected: "NeedsAuthorization",
            });
        };

        let resolution = match manager
            .resolve_metadata_from_challenge(challenge.as_deref())
            .await
        {
            Ok(resolution) => resolution,
            Err(error) => {
                self.state = Some(CoordinatorState::NeedsAuthorization(manager));
                return Err(error.into());
            }
        };
        let discovery_source = resolution.source.into();
        let manager = self.install_authorization_metadata(manager, resolution.metadata)?;
        match AuthorizationSession::new(manager, request.into_rmcp()).await {
            Ok(session) => {
                let launch = OAuthAuthorizationLaunch {
                    authorization_url: session.get_authorization_url().to_string(),
                    redirect_uri: session.redirect_uri.clone(),
                    discovery_source,
                };
                self.state = Some(CoordinatorState::WaitingForCallback {
                    session,
                    discovery_source,
                });
                Ok(launch)
            }
            Err((manager, error)) => {
                self.state = Some(CoordinatorState::NeedsAuthorization(manager));
                Err(error.into())
            }
        }
    }

    fn install_authorization_metadata(
        &mut self,
        mut manager: AuthorizationManager,
        metadata: AuthorizationMetadata,
    ) -> Result<AuthorizationManager, OAuthCoordinatorError> {
        if let Err(error) = validate_authorization_metadata(&self.endpoint, &metadata) {
            self.state = Some(CoordinatorState::NeedsAuthorization(manager));
            return Err(error);
        }
        manager.set_metadata(metadata);
        Ok(manager)
    }

    pub async fn complete_callback(
        &mut self,
        callback: &OAuthCallback,
    ) -> Result<(), OAuthCoordinatorError> {
        let state = self.take_state();
        let CoordinatorState::WaitingForCallback {
            session,
            discovery_source,
        } = state
        else {
            let actual = status_of_state(&state);
            self.state = Some(state);
            return Err(OAuthCoordinatorError::InvalidState {
                actual,
                expected: "WaitingForCallback",
            });
        };
        let exchange = session.handle_callback_with_issuer(
            callback.code.expose_secret(),
            callback.state.expose_secret(),
            callback.issuer.as_deref(),
        );
        // rmcp 3.1 emits the authorization code and token response at DEBUG.
        // Poll the exchange with a no-op subscriber until upstream removes it.
        if let Err(error) = exchange
            .with_subscriber(tracing::subscriber::NoSubscriber::default())
            .await
        {
            self.state = Some(CoordinatorState::WaitingForCallback {
                session,
                discovery_source,
            });
            return Err(error.into());
        }
        self.reauthorization_required
            .store(false, Ordering::Release);
        self.state = Some(CoordinatorState::Authorized(session.auth_manager));
        Ok(())
    }

    /// Return a token, refreshing it first when it is close to expiry.
    pub async fn access_token(&self) -> Result<SecretString, OAuthCoordinatorError> {
        let Some(CoordinatorState::Authorized(manager)) = self.state.as_ref() else {
            return Err(OAuthCoordinatorError::InvalidState {
                actual: self.status(),
                expected: "Authorized",
            });
        };
        let result = manager
            .get_access_token()
            .with_subscriber(tracing::subscriber::NoSubscriber::default())
            .await
            .map(SecretString::new)
            .map_err(OAuthCoordinatorError::from);
        if result
            .as_ref()
            .is_err_and(OAuthCoordinatorError::requires_authorization)
        {
            self.reauthorization_required.store(true, Ordering::Release);
        }
        result
    }

    pub async fn refresh(&self) -> Result<(), OAuthCoordinatorError> {
        let Some(CoordinatorState::Authorized(manager)) = self.state.as_ref() else {
            return Err(OAuthCoordinatorError::InvalidState {
                actual: self.status(),
                expected: "Authorized",
            });
        };
        let result = manager
            .refresh_token()
            .with_subscriber(tracing::subscriber::NoSubscriber::default())
            .await
            .map(|_| ())
            .map_err(OAuthCoordinatorError::from);
        if result
            .as_ref()
            .is_err_and(OAuthCoordinatorError::requires_authorization)
        {
            self.reauthorization_required.store(true, Ordering::Release);
        }
        result
    }

    pub async fn start_scope_upgrade(
        &mut self,
        required_scope: &str,
        redirect_uri: &str,
    ) -> Result<OAuthAuthorizationLaunch, OAuthCoordinatorError> {
        let state = self.take_state();
        let CoordinatorState::Authorized(manager) = state else {
            let actual = status_of_state(&state);
            self.state = Some(state);
            return Err(OAuthCoordinatorError::InvalidState {
                actual,
                expected: "Authorized",
            });
        };
        match manager.request_scope_upgrade(required_scope).await {
            Ok(authorization_url) => {
                let session = AuthorizationSession::for_scope_upgrade(
                    manager,
                    authorization_url.clone(),
                    redirect_uri,
                );
                self.state = Some(CoordinatorState::WaitingForCallback {
                    session,
                    discovery_source: OAuthDiscoverySource::Other,
                });
                Ok(OAuthAuthorizationLaunch {
                    authorization_url,
                    redirect_uri: redirect_uri.to_string(),
                    discovery_source: OAuthDiscoverySource::Other,
                })
            }
            Err(error) => {
                self.state = Some(CoordinatorState::Authorized(manager));
                Err(error.into())
            }
        }
    }

    pub async fn clear_credentials(&mut self) -> Result<(), OAuthCoordinatorError> {
        self.reset_authorization_state().await
    }

    async fn reset_authorization_state(&mut self) -> Result<(), OAuthCoordinatorError> {
        self.credential_store.clear().await?;
        let mut manager = AuthorizationManager::new(self.endpoint.as_str()).await?;
        manager.set_credential_store(self.credential_store.clone());
        self.state = Some(CoordinatorState::NeedsAuthorization(manager));
        self.reauthorization_required
            .store(false, Ordering::Release);
        Ok(())
    }

    fn take_state(&mut self) -> CoordinatorState {
        self.state
            .take()
            .expect("OAuth coordinator state is always populated")
    }
}

#[async_trait]
impl<S> BearerTokenProvider for OAuthCoordinator<S>
where
    S: CredentialStore + Clone + 'static,
{
    async fn bearer_token(&self) -> anyhow::Result<SecretString> {
        self.access_token().await.map_err(Into::into)
    }
}

fn status_of_state(state: &CoordinatorState) -> OAuthCoordinatorStatus {
    match state {
        CoordinatorState::NeedsAuthorization(_) => OAuthCoordinatorStatus::NeedsAuthorization,
        CoordinatorState::WaitingForCallback { .. } => OAuthCoordinatorStatus::WaitingForCallback,
        CoordinatorState::Authorized(_) => OAuthCoordinatorStatus::Authorized,
    }
}

fn validate_oauth_endpoint(endpoint: &str) -> Result<(), OAuthCoordinatorError> {
    let url = reqwest::Url::parse(endpoint)
        .map_err(|error| OAuthCoordinatorError::InvalidEndpoint(error.to_string()))?;
    let is_loopback = url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    });
    if url.scheme() != "https" && !(url.scheme() == "http" && is_loopback) {
        return Err(OAuthCoordinatorError::InvalidEndpoint(
            "HTTPS is required except for loopback development servers".to_string(),
        ));
    }
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err(OAuthCoordinatorError::InvalidEndpoint(
            "userinfo and URL fragments are not allowed".to_string(),
        ));
    }
    Ok(())
}

fn validate_authorization_metadata(
    base_endpoint: &str,
    metadata: &AuthorizationMetadata,
) -> Result<(), OAuthCoordinatorError> {
    let base = reqwest::Url::parse(base_endpoint)
        .map_err(|error| OAuthCoordinatorError::InvalidEndpoint(error.to_string()))?;
    validate_discovered_oauth_endpoint(&base, &metadata.authorization_endpoint)?;
    validate_discovered_oauth_endpoint(&base, &metadata.token_endpoint)?;
    if let Some(registration_endpoint) = &metadata.registration_endpoint {
        validate_discovered_oauth_endpoint(&base, registration_endpoint)?;
    }
    if let Some(jwks_uri) = &metadata.jwks_uri {
        validate_discovered_oauth_endpoint(&base, jwks_uri)?;
    }
    Ok(())
}

fn validate_discovered_oauth_endpoint(
    base: &reqwest::Url,
    endpoint: &str,
) -> Result<(), OAuthCoordinatorError> {
    validate_oauth_endpoint(endpoint)?;
    let candidate = reqwest::Url::parse(endpoint)
        .map_err(|error| OAuthCoordinatorError::InvalidEndpoint(error.to_string()))?;
    let base_is_public = base.host_str().is_some_and(is_public_oauth_host);
    let candidate_is_public = candidate.host_str().is_some_and(is_public_oauth_host);
    if base_is_public && !candidate_is_public {
        return Err(OAuthCoordinatorError::InvalidEndpoint(
            "a public MCP server cannot redirect OAuth traffic to a private, local, reserved, or cloud-metadata endpoint".to_string(),
        ));
    }
    Ok(())
}

fn is_public_oauth_host(host: &str) -> bool {
    let normalized = host.trim_end_matches('.');
    let normalized = normalized
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(normalized)
        .to_ascii_lowercase();
    if is_local_or_metadata_hostname(&normalized) {
        return false;
    }
    match normalized.parse::<std::net::IpAddr>() {
        Ok(address) => is_public_ip_address(address),
        Err(_) => !normalized.contains([':', '%']),
    }
}

fn is_local_or_metadata_hostname(host: &str) -> bool {
    matches!(
        host,
        "localhost"
            | "metadata.google"
            | "metadata.google.internal"
            | "instance-data"
            | "instance-data.ec2.internal"
    ) || [
        ".localhost",
        ".internal",
        ".local",
        ".localdomain",
        ".lan",
        ".home.arpa",
        ".invalid",
        ".test",
        ".example",
    ]
    .iter()
    .any(|suffix| host.ends_with(suffix))
}

fn is_public_ip_address(address: std::net::IpAddr) -> bool {
    match address {
        std::net::IpAddr::V4(address) => is_public_ipv4_address(address),
        std::net::IpAddr::V6(address) => is_public_ipv6_address(address),
    }
}

fn is_public_ipv4_address(address: std::net::Ipv4Addr) -> bool {
    let octets = address.octets();
    !(address.is_unspecified()
        || address.is_private()
        || address.is_loopback()
        || address.is_link_local()
        || address.is_broadcast()
        || address.is_documentation()
        || address.is_multicast()
        || octets[0] == 0
        || (octets[0] == 100 && (64..=127).contains(&octets[1]))
        || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
        || (octets[0] == 192 && octets[1] == 88 && octets[2] == 99)
        || (octets[0] == 198 && (18..=19).contains(&octets[1]))
        || octets[0] >= 240)
}

fn is_public_ipv6_address(address: std::net::Ipv6Addr) -> bool {
    if let Some(mapped) = address.to_ipv4_mapped() {
        return is_public_ipv4_address(mapped);
    }
    let segments = address.segments();
    let is_global_unicast = segments[0] & 0xe000 == 0x2000;
    let is_benchmarking = segments[0] == 0x2001 && segments[1] == 0x0002;
    let is_documentation = segments[0] == 0x2001 && segments[1] == 0x0db8;
    let is_orchid = segments[0] == 0x2001 && matches!(segments[1] & 0xfff0, 0x0010 | 0x0020);
    let is_six_to_four = segments[0] == 0x2002;
    let is_documentation_v2 = segments[0] == 0x3fff && segments[1] & 0xf000 == 0;
    is_global_unicast
        && !is_benchmarking
        && !is_documentation
        && !is_orchid
        && !is_six_to_four
        && !is_documentation_v2
}

/// In-memory rmcp credential store for tests and ephemeral sessions.
#[derive(Clone, Default)]
pub struct MemoryCredentialStore {
    credentials: Arc<tokio::sync::RwLock<Option<StoredCredentials>>>,
}

impl std::fmt::Debug for MemoryCredentialStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MemoryCredentialStore")
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl CredentialStore for MemoryCredentialStore {
    async fn load(&self) -> Result<Option<StoredCredentials>, RmcpAuthError> {
        Ok(self.credentials.read().await.clone())
    }

    async fn save(&self, credentials: StoredCredentials) -> Result<(), RmcpAuthError> {
        *self.credentials.write().await = Some(credentials);
        Ok(())
    }

    async fn clear(&self) -> Result<(), RmcpAuthError> {
        *self.credentials.write().await = None;
        Ok(())
    }
}

/// Cross-platform facade for the platform credential backend.
///
/// macOS persists to Keychain. Other targets use an ephemeral memory store so
/// callers remain portable without silently writing plaintext credentials.
#[derive(Clone)]
pub struct NativeCredentialStore {
    profile_id: String,
    #[cfg(target_os = "macos")]
    inner: MacOsKeychainCredentialStore,
    #[cfg(not(target_os = "macos"))]
    inner: MemoryCredentialStore,
}

impl NativeCredentialStore {
    pub fn profile_id(&self) -> &str {
        &self.profile_id
    }

    pub fn is_persistent(&self) -> bool {
        cfg!(target_os = "macos")
    }
}

impl std::fmt::Debug for NativeCredentialStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativeCredentialStore")
            .field("profile_id", &self.profile_id)
            .field("persistent", &self.is_persistent())
            .finish()
    }
}

#[async_trait]
impl CredentialStore for NativeCredentialStore {
    async fn load(&self) -> Result<Option<StoredCredentials>, RmcpAuthError> {
        self.inner.load().await
    }

    async fn save(&self, credentials: StoredCredentials) -> Result<(), RmcpAuthError> {
        self.inner.save(credentials).await
    }

    async fn clear(&self) -> Result<(), RmcpAuthError> {
        self.inner.clear().await
    }
}

pub type NativeOAuthCoordinator = OAuthCoordinator<NativeCredentialStore>;

pub fn native_credential_store(
    profile_id: impl Into<String>,
) -> Result<NativeCredentialStore, OAuthCoordinatorError> {
    let profile_id = profile_id.into();
    validate_profile_id(&profile_id)?;
    #[cfg(target_os = "macos")]
    let inner = MacOsKeychainCredentialStore::new(profile_id.clone())?;
    #[cfg(not(target_os = "macos"))]
    let inner = shared_memory_credential_store(&profile_id)?;
    Ok(NativeCredentialStore { profile_id, inner })
}

#[cfg(not(target_os = "macos"))]
fn shared_memory_credential_store(
    profile_id: &str,
) -> Result<MemoryCredentialStore, OAuthCoordinatorError> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    static STORES: OnceLock<Mutex<HashMap<String, MemoryCredentialStore>>> = OnceLock::new();
    let mut stores = STORES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(|_| {
            OAuthCoordinatorError::Operation(
                "native in-memory credential registry is poisoned".to_string(),
            )
        })?;
    Ok(stores.entry(profile_id.to_string()).or_default().clone())
}

pub async fn native_oauth_coordinator(
    endpoint: impl Into<String>,
    profile_id: impl Into<String>,
) -> Result<NativeOAuthCoordinator, OAuthCoordinatorError> {
    OAuthCoordinator::new(endpoint, native_credential_store(profile_id)?).await
}

/// Remove the persisted credentials for a native OAuth profile.
pub async fn clear_native_credentials(
    profile_id: impl Into<String>,
) -> Result<(), OAuthCoordinatorError> {
    native_credential_store(profile_id)?.clear().await?;
    Ok(())
}

fn validate_profile_id(profile_id: &str) -> Result<(), OAuthCoordinatorError> {
    if profile_id.trim().is_empty() || profile_id.chars().any(char::is_control) {
        return Err(OAuthCoordinatorError::Operation(
            "credential profile must be non-empty and contain no control characters".to_string(),
        ));
    }
    Ok(())
}

/// Service name used for OAuth credentials in macOS Keychain.
#[cfg(target_os = "macos")]
pub const OXIDE_MCP_KEYCHAIN_SERVICE: &str = "com.manfit7.oxide.mcp.oauth";

/// Persistent OAuth credential store backed by macOS Keychain.
#[cfg(target_os = "macos")]
#[derive(Clone)]
pub struct MacOsKeychainCredentialStore {
    profile_id: String,
    operation_gate: Arc<tokio::sync::Mutex<()>>,
}

#[cfg(target_os = "macos")]
impl MacOsKeychainCredentialStore {
    pub fn new(profile_id: impl Into<String>) -> Result<Self, OAuthCoordinatorError> {
        let profile_id = profile_id.into();
        validate_profile_id(&profile_id)?;
        let operation_gate = shared_keychain_operation_gate(&profile_id)?;
        Ok(Self {
            profile_id,
            operation_gate,
        })
    }

    pub fn profile_id(&self) -> &str {
        &self.profile_id
    }
}

#[cfg(target_os = "macos")]
fn shared_keychain_operation_gate(
    profile_id: &str,
) -> Result<Arc<tokio::sync::Mutex<()>>, OAuthCoordinatorError> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    type Gate = Arc<tokio::sync::Mutex<()>>;
    static GATES: OnceLock<Mutex<HashMap<String, Gate>>> = OnceLock::new();
    let mut gates = GATES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(|_| {
            OAuthCoordinatorError::Operation(
                "macOS Keychain operation registry is poisoned".to_string(),
            )
        })?;
    Ok(gates
        .entry(profile_id.to_string())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone())
}

#[cfg(target_os = "macos")]
impl std::fmt::Debug for MacOsKeychainCredentialStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MacOsKeychainCredentialStore")
            .field("service", &OXIDE_MCP_KEYCHAIN_SERVICE)
            .field("profile_id", &self.profile_id)
            .finish()
    }
}

#[cfg(target_os = "macos")]
#[async_trait]
impl CredentialStore for MacOsKeychainCredentialStore {
    async fn load(&self) -> Result<Option<StoredCredentials>, RmcpAuthError> {
        let profile_id = self.profile_id.clone();
        let operation_guard = self.operation_gate.clone().lock_owned().await;
        tokio::task::spawn_blocking(move || {
            let _operation_guard = operation_guard;
            let entry = keyring::Entry::new(OXIDE_MCP_KEYCHAIN_SERVICE, &profile_id)
                .map_err(keychain_auth_error)?;
            match entry.get_password() {
                Ok(serialized) => serde_json::from_str(&serialized)
                    .map(Some)
                    .map_err(|error| RmcpAuthError::InternalError(error.to_string())),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(error) => Err(keychain_auth_error(error)),
            }
        })
        .await
        .map_err(|error| RmcpAuthError::InternalError(error.to_string()))?
    }

    async fn save(&self, credentials: StoredCredentials) -> Result<(), RmcpAuthError> {
        let profile_id = self.profile_id.clone();
        let serialized = serde_json::to_string(&credentials)
            .map_err(|error| RmcpAuthError::InternalError(error.to_string()))?;
        let operation_guard = self.operation_gate.clone().lock_owned().await;
        tokio::task::spawn_blocking(move || {
            let _operation_guard = operation_guard;
            let entry = keyring::Entry::new(OXIDE_MCP_KEYCHAIN_SERVICE, &profile_id)
                .map_err(keychain_auth_error)?;
            entry.set_password(&serialized).map_err(keychain_auth_error)
        })
        .await
        .map_err(|error| RmcpAuthError::InternalError(error.to_string()))?
    }

    async fn clear(&self) -> Result<(), RmcpAuthError> {
        let profile_id = self.profile_id.clone();
        let operation_guard = self.operation_gate.clone().lock_owned().await;
        tokio::task::spawn_blocking(move || {
            let _operation_guard = operation_guard;
            let entry = keyring::Entry::new(OXIDE_MCP_KEYCHAIN_SERVICE, &profile_id)
                .map_err(keychain_auth_error)?;
            match entry.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(error) => Err(keychain_auth_error(error)),
            }
        })
        .await
        .map_err(|error| RmcpAuthError::InternalError(error.to_string()))?
    }
}

#[cfg(target_os = "macos")]
fn keychain_auth_error(error: keyring::Error) -> RmcpAuthError {
    RmcpAuthError::InternalError(redact_oauth_secrets(&error.to_string()))
}

/// Parsed loopback callback. Authorization code and state are redacted by type.
#[derive(Clone, Eq, PartialEq)]
pub struct OAuthCallback {
    code: SecretString,
    state: SecretString,
    issuer: Option<String>,
}

impl std::fmt::Debug for OAuthCallback {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OAuthCallback")
            .field("code", &self.code)
            .field("state", &self.state)
            .field("issuer", &self.issuer)
            .finish()
    }
}

impl OAuthCallback {
    pub fn parse(url: &str) -> Result<Self, OAuthCoordinatorError> {
        let url = reqwest::Url::parse(url)
            .map_err(|error| OAuthCoordinatorError::Operation(error.to_string()))?;
        let mut code = None;
        let mut state = None;
        let mut issuer = None;
        let mut oauth_error = None;
        let mut error_description = None;
        for (key, value) in url.query_pairs() {
            match key.as_ref() {
                "code" => code = Some(SecretString::new(value.into_owned())),
                "state" => state = Some(SecretString::new(value.into_owned())),
                "iss" => issuer = Some(value.into_owned()),
                "error" => oauth_error = Some(value.into_owned()),
                "error_description" => error_description = Some(value.into_owned()),
                _ => {}
            }
        }
        if let Some(code) = oauth_error {
            return Err(OAuthCoordinatorError::CallbackRejected {
                code,
                description: error_description.map(|value| redact_oauth_secrets(&value)),
            });
        }
        Ok(Self {
            code: code.ok_or_else(|| {
                OAuthCoordinatorError::Operation("OAuth callback is missing code".to_string())
            })?,
            state: state.ok_or_else(|| {
                OAuthCoordinatorError::Operation("OAuth callback is missing state".to_string())
            })?,
            issuer,
        })
    }

    pub fn issuer(&self) -> Option<&str> {
        self.issuer.as_deref()
    }
}

/// A one-shot loopback server for native-app OAuth redirects.
pub struct LoopbackCallbackServer {
    listener: TcpListener,
    redirect_uri: String,
    callback_path: String,
}

impl LoopbackCallbackServer {
    pub async fn bind() -> Result<Self, LoopbackCallbackError> {
        Self::bind_path(DEFAULT_CALLBACK_PATH).await
    }

    pub async fn bind_path(path: &str) -> Result<Self, LoopbackCallbackError> {
        if !path.starts_with('/') || path.contains(['?', '#']) {
            return Err(LoopbackCallbackError::InvalidPath(path.to_string()));
        }
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let port = listener.local_addr()?.port();
        Ok(Self {
            listener,
            redirect_uri: format!("http://127.0.0.1:{port}{path}"),
            callback_path: path.to_string(),
        })
    }

    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    pub async fn wait_for_callback(
        self,
        timeout: Duration,
    ) -> Result<OAuthCallback, LoopbackCallbackError> {
        tokio::time::timeout(timeout, self.wait_for_valid_callback())
            .await
            .map_err(|_| LoopbackCallbackError::Timeout)?
    }

    async fn wait_for_valid_callback(&self) -> Result<OAuthCallback, LoopbackCallbackError> {
        loop {
            let (mut stream, _) = self.listener.accept().await?;
            let target = match read_callback_target(&mut stream).await {
                Ok(target) => target,
                Err(error @ LoopbackCallbackError::InvalidRequest(_)) => {
                    write_browser_response(
                        &mut stream,
                        400,
                        "Invalid authorization callback request.",
                    )
                    .await?;
                    tracing::warn!(error = %error, "ignored invalid OAuth loopback request");
                    continue;
                }
                Err(error) => return Err(error),
            };
            let callback_url = format!("http://127.0.0.1{target}");
            let parsed = reqwest::Url::parse(&callback_url)
                .map_err(|error| LoopbackCallbackError::InvalidRequest(error.to_string()))?;
            if parsed.path() != self.callback_path {
                write_browser_response(&mut stream, 404, "OAuth callback path not found").await?;
                continue;
            }
            let callback = OAuthCallback::parse(parsed.as_str());
            match callback {
                Ok(callback) => {
                    write_browser_response(
                        &mut stream,
                        200,
                        "Authorization response received. Return to Oxide to finish.",
                    )
                    .await?;
                    return Ok(callback);
                }
                Err(error) => {
                    write_browser_response(
                        &mut stream,
                        400,
                        "Authorization failed. Return to Oxide for details.",
                    )
                    .await?;
                    return Err(LoopbackCallbackError::OAuth(error));
                }
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LoopbackCallbackError {
    #[error("invalid OAuth callback path: {0}")]
    InvalidPath(String),
    #[error("invalid OAuth callback request: {0}")]
    InvalidRequest(String),
    #[error("OAuth callback timed out")]
    Timeout,
    #[error("OAuth callback failed: {0}")]
    OAuth(#[from] OAuthCoordinatorError),
    #[error("OAuth callback I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

async fn read_callback_target(stream: &mut TcpStream) -> Result<String, LoopbackCallbackError> {
    let mut request = Vec::new();
    let mut chunk = [0_u8; 1024];
    loop {
        let count = stream.read(&mut chunk).await?;
        if count == 0 {
            break;
        }
        request.extend_from_slice(&chunk[..count]);
        if request.len() > MAX_CALLBACK_REQUEST_BYTES {
            return Err(LoopbackCallbackError::InvalidRequest(
                "request headers exceed 16 KiB".to_string(),
            ));
        }
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let request = std::str::from_utf8(&request)
        .map_err(|_| LoopbackCallbackError::InvalidRequest("request is not UTF-8".to_string()))?;
    let first_line = request.lines().next().ok_or_else(|| {
        LoopbackCallbackError::InvalidRequest("request line is missing".to_string())
    })?;
    let mut parts = first_line.split_whitespace();
    if parts.next() != Some("GET") {
        return Err(LoopbackCallbackError::InvalidRequest(
            "callback must use GET".to_string(),
        ));
    }
    let target = parts.next().ok_or_else(|| {
        LoopbackCallbackError::InvalidRequest("request target is missing".to_string())
    })?;
    if !target.starts_with('/') {
        return Err(LoopbackCallbackError::InvalidRequest(
            "request target must be origin-form".to_string(),
        ));
    }
    Ok(target.to_string())
}

async fn write_browser_response(
    stream: &mut TcpStream,
    status: u16,
    message: &str,
) -> Result<(), std::io::Error> {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        _ => "Bad Request",
    };
    let body = format!(
        "<!doctype html><meta charset=\"utf-8\"><title>Oxide OAuth</title><p>{message}</p>"
    );
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn memory_store_round_trips_and_clears_credentials() {
        let store = MemoryCredentialStore::default();
        let credentials = StoredCredentials::new("client-id".into(), None, vec![], None);

        store.save(credentials).await.unwrap();
        let loaded = store.load().await.unwrap().unwrap();
        assert_eq!(loaded.client_id, "client-id");

        store.clear().await.unwrap();
        assert!(store.load().await.unwrap().is_none());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn keychain_stores_for_one_profile_share_operation_gate() {
        let first = MacOsKeychainCredentialStore::new("test-shared-gate").unwrap();
        let second = MacOsKeychainCredentialStore::new("test-shared-gate").unwrap();

        assert!(Arc::ptr_eq(&first.operation_gate, &second.operation_gate));
    }

    #[test]
    fn callback_debug_never_contains_code_or_state() {
        let callback = OAuthCallback::parse(
            "http://127.0.0.1/oauth/callback?code=secret-code&state=secret-state&iss=https%3A%2F%2Fissuer.example",
        )
        .unwrap();
        let debug = format!("{callback:?}");

        assert!(!debug.contains("secret-code"));
        assert!(!debug.contains("secret-state"));
        assert_eq!(callback.issuer(), Some("https://issuer.example"));
    }

    #[test]
    fn authorization_launch_debug_redacts_state() {
        let launch = OAuthAuthorizationLaunch {
            authorization_url: "https://auth.example/authorize?state=secret-state&client_id=oxide"
                .to_string(),
            redirect_uri: "http://127.0.0.1/oauth/callback".to_string(),
            discovery_source: OAuthDiscoverySource::AuthorizationServerMetadata,
        };

        assert!(!format!("{launch:?}").contains("secret-state"));
    }

    #[tokio::test]
    async fn loopback_server_captures_callback_without_live_oauth() {
        let server = LoopbackCallbackServer::bind().await.unwrap();
        let redirect = reqwest::Url::parse(server.redirect_uri()).unwrap();
        let address = format!("127.0.0.1:{}", redirect.port().unwrap());
        let callback = tokio::spawn(server.wait_for_callback(Duration::from_secs(2)));
        let mut client = TcpStream::connect(address).await.unwrap();
        client
            .write_all(
                b"GET /oauth/callback?code=test-code&state=test-state HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            )
            .await
            .unwrap();

        let result = callback.await.unwrap().unwrap();
        assert_eq!(result.code.expose_secret(), "test-code");
        assert_eq!(result.state.expose_secret(), "test-state");
    }

    #[tokio::test]
    async fn loopback_server_ignores_wrong_path_before_callback() {
        let server = LoopbackCallbackServer::bind().await.unwrap();
        let redirect = reqwest::Url::parse(server.redirect_uri()).unwrap();
        let address = format!("127.0.0.1:{}", redirect.port().unwrap());
        let callback = tokio::spawn(server.wait_for_callback(Duration::from_secs(2)));

        let mut wrong_path = TcpStream::connect(&address).await.unwrap();
        wrong_path
            .write_all(b"GET /favicon.ico HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
            .await
            .unwrap();
        wrong_path.shutdown().await.unwrap();

        let mut client = TcpStream::connect(address).await.unwrap();
        client
            .write_all(
                b"GET /oauth/callback?code=test-code&state=test-state HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            )
            .await
            .unwrap();

        let result = callback.await.unwrap().unwrap();
        assert_eq!(result.code.expose_secret(), "test-code");
    }

    #[tokio::test]
    async fn loopback_timeout_covers_stalled_request_read() {
        let server = LoopbackCallbackServer::bind().await.unwrap();
        let redirect = reqwest::Url::parse(server.redirect_uri()).unwrap();
        let address = format!("127.0.0.1:{}", redirect.port().unwrap());
        let callback = tokio::spawn(server.wait_for_callback(Duration::from_millis(100)));
        let _stalled_client = TcpStream::connect(address).await.unwrap();

        let error = callback.await.unwrap().unwrap_err();
        assert!(matches!(error, LoopbackCallbackError::Timeout));
    }

    #[test]
    fn rejects_non_tls_remote_oauth_endpoint() {
        let error = validate_oauth_endpoint("http://example.com/mcp").unwrap_err();

        assert!(matches!(error, OAuthCoordinatorError::InvalidEndpoint(_)));
        assert!(validate_oauth_endpoint("http://127.0.0.1:54321/mcp").is_ok());
        assert!(validate_oauth_endpoint("https://user@example.com/mcp").is_err());
        assert!(validate_oauth_endpoint("https://example.com/mcp#fragment").is_err());
    }

    #[test]
    fn rejects_public_resource_metadata_that_targets_non_public_endpoints() {
        for endpoint in [
            "https://127.0.0.1/token",
            "https://10.0.0.1/token",
            "https://169.254.169.254/token",
            "https://192.0.2.1/token",
            "https://[::1]/token",
            "https://[::ffff:127.0.0.1]/token",
            "https://[2001:2::1]/token",
            "https://[2001:db8::1]/token",
            "https://[3fff::1]/token",
            "https://metadata.google.internal/token",
        ] {
            let mut metadata = AuthorizationMetadata::default();
            metadata.authorization_endpoint = "https://auth.example.com/authorize".to_string();
            metadata.token_endpoint = endpoint.to_string();

            assert!(
                validate_authorization_metadata("https://mcp.example.com/mcp", &metadata).is_err(),
                "endpoint should be rejected: {endpoint}"
            );
        }
    }

    #[test]
    fn permits_private_metadata_for_private_resources() {
        let mut metadata = AuthorizationMetadata::default();
        metadata.authorization_endpoint = "https://10.0.0.2/authorize".to_string();
        metadata.token_endpoint = "https://10.0.0.3/token".to_string();

        assert!(validate_authorization_metadata("https://10.0.0.1/mcp", &metadata).is_ok());
    }

    #[tokio::test]
    async fn invalid_metadata_restores_state_for_retry() {
        let mut coordinator = OAuthCoordinator::new(
            "http://127.0.0.1:54321/mcp",
            MemoryCredentialStore::default(),
        )
        .await
        .unwrap();

        for _ in 0..2 {
            let CoordinatorState::NeedsAuthorization(manager) = coordinator.take_state() else {
                panic!("coordinator must remain ready for authorization");
            };
            let mut metadata = AuthorizationMetadata::default();
            metadata.authorization_endpoint = "http://example.com/authorize".to_string();
            metadata.token_endpoint = "http://example.com/token".to_string();

            let error = match coordinator.install_authorization_metadata(manager, metadata) {
                Ok(_) => panic!("invalid metadata must be rejected"),
                Err(error) => error,
            };

            assert!(matches!(error, OAuthCoordinatorError::InvalidEndpoint(_)));
            assert_eq!(
                coordinator.status(),
                OAuthCoordinatorStatus::NeedsAuthorization
            );
        }
    }
}
