//! Selected native account ownership and connection preparation on the caller's async runtime.

mod metadata;
mod realms;
mod realms_worker;
pub use realms::{RealmListing, is_realm_target};
pub use realms_worker::RealmsWorker;

use bedrock_services::{
    minecraft,
    oauth::{Credentials, MicrosoftAuth},
    xbox,
};
use launcher::accounts::{AccountStore, native::NativeLease};
use p384::{ecdsa::SigningKey, elliptic_curve::Generate};
use rand::rngs::SysRng;
use serde_json::Value;
use std::{
    io,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, UNIX_EPOCH},
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

pub(super) const ACQUISITION_TIMEOUT: Duration = Duration::from_secs(30);
pub(super) const LEASE_RETRY: Duration = Duration::from_millis(25);
pub(super) const XBOX_RELYING_PARTY: &str = "http://xboxlive.com";
pub(super) const REFRESH_ONLY_EXPIRY: &str = "1970-01-01T00:00:00Z";

/// Retains proof and service credentials for one selected application account.
/// No runtime, polling task or HTTP request is created until an operation is polled.
/// Credential values and filesystem locations are deliberately absent from Debug.
pub struct NativeAccount {
    store: AccountStore,
    device_path: PathBuf,
    selected_id: Option<String>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    busy: bool,
    cached: Option<Cached>,
}

struct Cached {
    generation: String,
    binding: [u8; 32],
    owner: Owner,
    identity: Option<AccountIdentity>,
}

/// Retains the generation's account anchor and current OAuth identity validation.
struct AccountIdentity {
    xuid: String,
    gamertag: String,
    current_credential: bool,
}

impl Cached {
    /// Retains the generation's verified XUID while invalidating identity for a changed OAuth revision.
    fn adopt_credentials(&mut self, credentials: Credentials, binding: [u8; 32]) -> io::Result<()> {
        self.owner.replace_credentials(credentials)?;
        self.binding = binding;
        if let Some(identity) = &mut self.identity {
            identity.current_credential = false;
        }
        Ok(())
    }
}

enum Owner {
    Xbox(Box<xbox::Account>),
    Minecraft(Box<minecraft::Minecraft>),
}

/// Takes the SDK owner out of its data mutex for the entire operation.
/// Drop persists an already acquired OAuth rotation, including cancellation during later HTTP.
struct Operation {
    account: Arc<NativeAccount>,
    cached: Option<Cached>,
    lease: Option<NativeLease>,
    published: bool,
    rejected: bool,
}

impl NativeAccount {
    /// Binds a selected identity to its existing account store without accessing credentials.
    pub fn new(store: AccountStore, device_path: PathBuf, selected_id: Option<String>) -> Self {
        Self {
            store,
            device_path,
            selected_id,
            state: Mutex::new(State::default()),
        }
    }

    /// Acquires account credentials and mints for one fresh connection key.
    /// The caller polls this on its session runtime and cancels by dropping the future.
    /// It never falls back to a signed-out identity when authentication fails.
    pub async fn prepare(
        self: Arc<Self>,
        claims: Value,
        random_id: protocol::ClientRandomId,
    ) -> io::Result<protocol::NativeAuthentication> {
        if !claims.is_object() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "native client data must be a JSON object",
            ));
        }
        tokio::time::timeout(ACQUISITION_TIMEOUT, self.prepare_inner(claims, random_id))
            .await
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    "native account preparation timed out",
                )
            })?
    }

    /// Serializes one acquisition, adopting a different process's current OAuth revision before HTTP.
    async fn prepare_inner(
        self: Arc<Self>,
        claims: Value,
        random_id: protocol::ClientRandomId,
    ) -> io::Result<protocol::NativeAuthentication> {
        let mut operation = Arc::clone(&self).acquire_verified().await?;
        operation.promote()?;
        self.prepare_owned(operation, claims, random_id).await
    }

    /// Mints the final connection key without releasing or replacing the acquired service owner.
    async fn prepare_owned(
        &self,
        mut operation: Operation,
        mut claims: Value,
        random_id: protocol::ClientRandomId,
    ) -> io::Result<protocol::NativeAuthentication> {
        let fresh = serde_json::to_vec(&metadata::DeviceProfile::fresh())
            .map_err(|_| io::Error::other("serialize native device profile"))?;
        let profile = loop {
            match operation
                .lease
                .as_ref()
                .expect("acquired lease")
                .device_profile(&self.device_path, &fresh, |bytes| {
                    metadata::DeviceProfile::parse(bytes).is_ok()
                }) {
                Ok(bytes) => break metadata::DeviceProfile::parse(&bytes)?,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    tokio::time::sleep(LEASE_RETRY).await
                }
                Err(error) => return Err(error),
            }
        };
        let key = SigningKey::try_generate_from_rng(&mut SysRng)
            .map_err(|_| io::Error::other("native connection key generation failed"))?;
        let token = operation
            .minecraft()?
            .multiplayer_token(key.verifying_key())
            .await
            .map_err(service_error)?;
        profile.apply(
            &mut claims,
            random_id,
            &operation
                .cached
                .as_ref()
                .expect("account owner")
                .identity
                .as_ref()
                .expect("verified identity")
                .gamertag,
        );
        let raw = serde_json::value::to_raw_value(&claims)
            .map_err(|_| io::Error::other("serialize native client data"))?;
        let prepared = protocol::NativeAuthentication::new(key, token.as_str().to_owned(), &raw)?;
        operation.persist()?;
        Ok(prepared)
    }

    /// Validates the selected saved account without minting a Minecraft service credential.
    /// Cancellation retains any SDK-visible OAuth rotation through the operation's existing Drop owner.
    pub async fn validate(self: Arc<Self>) -> io::Result<launcher::accounts::AccountProfile> {
        tokio::time::timeout(ACQUISITION_TIMEOUT, async {
            let mut operation = self.acquire_verified().await?;
            let identity = operation
                .cached
                .as_ref()
                .expect("account owner")
                .identity
                .as_ref()
                .expect("verified identity");
            let profile = launcher::accounts::AccountProfile::from_identity(
                &identity.xuid,
                &identity.gamertag,
            )?;
            operation.persist()?;
            operation
                .lease
                .as_mut()
                .expect("acquired lease")
                .remember_verified_profile(&profile)
        })
        .await
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "native account validation timed out",
            )
        })?
    }

    /// Acquires and verifies one concrete account owner before either validation or service minting.
    async fn acquire_verified(self: Arc<Self>) -> io::Result<Operation> {
        let mut operation = loop {
            match self.begin() {
                Ok(operation) => break operation,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    tokio::time::sleep(LEASE_RETRY).await
                }
                Err(error) => return Err(error),
            }
        };
        operation.lease = Some(loop {
            match self.store.try_native_lease() {
                Ok(lease) => break lease,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    tokio::time::sleep(LEASE_RETRY).await
                }
                Err(error) => {
                    // No new request began; retired or unreadable storage must also release cached secrets.
                    operation.cached = None;
                    return Err(error);
                }
            }
        });
        let lease = operation.lease.as_ref().expect("acquired lease");
        if self
            .selected_id
            .as_deref()
            .is_some_and(|selected| lease.selected_id() != Some(selected))
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "selected native account changed",
            ));
        }
        let policy = authentication_policy();
        if lease.client_id().is_some_and(|id| id != policy.client_id) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "saved account uses another OAuth policy",
            ));
        }
        if operation
            .cached
            .as_ref()
            .is_some_and(|cached| cached.generation != lease.generation())
        {
            // A new explicit join may use the new sign-in, but no credential from the retired owner survives.
            operation.cached = None;
        }
        if operation.cached.is_none() {
            let auth = MicrosoftAuth::new(policy.clone()).map_err(service_error)?;
            let mut account = match lease.checkpoint()? {
                Some(bytes) => auth
                    .restore_account(xbox::Checkpoint::from_bytes(bytes).map_err(service_error)?)
                    .map_err(service_error)?,
                None => auth
                    .into_account(import_credentials(lease, &policy)?)
                    .map_err(service_error)?,
            };
            // A checkpoint can precede an OAuth publication. Never refresh its older credential.
            account
                .replace_credentials(import_credentials(lease, &policy)?)
                .map_err(service_error)?;
            operation.cached = Some(Cached {
                generation: lease.generation().to_owned(),
                binding: lease.binding(),
                owner: Owner::Xbox(Box::new(account)),
                identity: None,
            });
        } else if let Some(cached) = operation.cached.as_mut()
            && cached.binding != lease.binding()
        {
            cached.adopt_credentials(import_credentials(lease, &policy)?, lease.binding())?;
        }

        if let Some(identity) = operation
            .cached
            .as_ref()
            .expect("account owner")
            .identity
            .as_ref()
            && operation
                .lease
                .as_ref()
                .expect("acquired lease")
                .selected_id()
                .is_some_and(|selected| selected != identity.xuid)
        {
            operation.rejected = true;
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "native account identity does not match the selection",
            ));
        }

        if operation
            .cached
            .as_ref()
            .expect("account owner")
            .identity
            .as_ref()
            .is_none_or(|identity| !identity.current_credential)
        {
            let authorization = operation
                .cached
                .as_mut()
                .expect("account owner")
                .owner
                .xbox_authorization()
                .await?;
            operation.verify_identity(authorization.xuid(), authorization.gamertag())?;
        }
        Ok(operation)
    }

    /// Reserves the owned SDK value with a short data lock, never retaining that guard during I/O.
    fn begin(self: &Arc<Self>) -> io::Result<Operation> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("native account owner unavailable"))?;
        if state.busy {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "native account preparation already active",
            ));
        }
        state.busy = true;
        Ok(Operation {
            account: Arc::clone(self),
            cached: state.cached.take(),
            lease: None,
            published: false,
            rejected: false,
        })
    }
}

impl Owner {
    /// Revalidates the current credential's Xbox identity without consuming the Minecraft session.
    async fn xbox_authorization(&mut self) -> io::Result<Arc<xbox::Authorization>> {
        match self {
            Self::Xbox(account) => account.authorize(XBOX_RELYING_PARTY).await,
            Self::Minecraft(services) => services.xbox_authorization().await,
        }
        .map_err(service_error)
    }

    /// Borrows the current rotated OAuth state without cloning the SDK owner.
    fn credentials(&self) -> &Credentials {
        match self {
            Self::Xbox(account) => account.credentials(),
            Self::Minecraft(services) => services.credentials(),
        }
    }

    /// Encodes proof/refresh ownership for protected generation-bound storage.
    fn checkpoint(&self) -> io::Result<xbox::Checkpoint> {
        match self {
            Self::Xbox(account) => account.checkpoint(),
            Self::Minecraft(services) => services.checkpoint(),
        }
        .map_err(service_error)
    }

    /// Replaces only a current same-generation credential, preserving proof and game-session identity.
    fn replace_credentials(&mut self, credentials: Credentials) -> io::Result<()> {
        match self {
            Self::Xbox(account) => account.replace_credentials(credentials),
            Self::Minecraft(services) => services.replace_credentials(credentials),
        }
        .map_err(service_error)
    }
}

impl Operation {
    /// Binds the saved slot only after checking the acquired authorization's actual identity.
    fn verify_identity(&mut self, xuid: &str, gamertag: &str) -> io::Result<()> {
        let lease = self.lease.as_ref().expect("acquired lease");
        let cached = self.cached.as_ref().expect("account owner");
        if lease.selected_id().is_some_and(|selected| selected != xuid)
            || cached
                .identity
                .as_ref()
                .is_some_and(|previous| previous.xuid != xuid)
        {
            self.rejected = true;
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "native account identity does not match the selection",
            ));
        }
        self.cached.as_mut().expect("account owner").identity = Some(AccountIdentity {
            xuid: xuid.to_owned(),
            gamertag: gamertag.to_owned(),
            current_credential: true,
        });
        Ok(())
    }

    /// Creates Minecraft services once, retaining proof, credentials and Session-Id for borrowed service calls.
    fn promote(&mut self) -> io::Result<()> {
        if matches!(
            self.cached.as_ref().expect("account owner").owner,
            Owner::Xbox(_)
        ) {
            let identity = self
                .cached
                .as_ref()
                .expect("account owner")
                .identity
                .as_ref()
                .expect("verified identity");
            let version = u32::try_from(protocol::PROTOCOL_VERSION)
                .map_err(|_| io::Error::other("invalid application protocol version"))?;
            let config = minecraft::Config::desktop(
                protocol::GAME_VERSION,
                version,
                metadata::service_device_id(&identity.xuid)?,
            );
            self.persist()?;
            let Cached {
                generation,
                binding,
                owner,
                identity,
            } = self.cached.take().expect("account owner");
            let Owner::Xbox(account) = owner else {
                unreachable!("checked Xbox owner")
            };
            let services = minecraft::Minecraft::new(*account, config).map_err(service_error)?;
            self.cached = Some(Cached {
                generation,
                binding,
                owner: Owner::Minecraft(Box::new(services)),
                identity,
            });
            self.published = false;
        }
        Ok(())
    }

    /// Allows service minting only after current credentials pass every application identity fence.
    fn minecraft(&mut self) -> io::Result<&mut minecraft::Minecraft> {
        if self.rejected {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "native account identity was rejected",
            ));
        }
        let cached = self
            .cached
            .as_mut()
            .ok_or_else(|| io::Error::other("native account owner unavailable"))?;
        if cached
            .identity
            .as_ref()
            .is_none_or(|identity| !identity.current_credential)
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "native account identity is not verified",
            ));
        }
        match &mut cached.owner {
            Owner::Minecraft(services) => Ok(services),
            Owner::Xbox(_) => Err(io::Error::other("native Minecraft owner is not prepared")),
        }
    }

    /// Writes the newest refresh before success and updates the cached file revision.
    fn persist(&mut self) -> io::Result<()> {
        let (Some(cached), Some(lease)) = (self.cached.as_mut(), self.lease.as_mut()) else {
            return Ok(());
        };
        if cached.generation != lease.generation() || cached.binding != lease.binding() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "native account has not adopted the current leased credentials",
            ));
        }
        let credentials = cached.owner.credentials();
        let mut publication_error = None;
        if !credentials.access_token().is_empty() {
            let publication = oauth_expiry(credentials.expires_at()).and_then(|expiry| {
                lease.publish_oauth(
                    credentials.access_token(),
                    credentials.refresh_token(),
                    &expiry,
                )
            });
            if let Err(error) = publication {
                if !matches!(
                    error.kind(),
                    io::ErrorKind::FileTooLarge | io::ErrorKind::InvalidInput
                ) {
                    return Err(error);
                }
                // Fail this join, but preserve a rotated refresh when the access record cannot be shared with Go.
                lease.publish_oauth("", credentials.refresh_token(), REFRESH_ONLY_EXPIRY)?;
                self.rejected = true;
                publication_error = Some(error);
            }
        }
        // OAuth is already durable even if checkpoint encoding or saved-slot publication fails next.
        cached.binding = lease.binding();
        let checkpoint = cached.owner.checkpoint()?;
        if !self.rejected
            && let Some(identity) = &cached.identity
            && identity.current_credential
        {
            if let Err(error) =
                lease.publish_verified_identity(&identity.xuid, checkpoint.as_bytes())
            {
                lease.publish_active_checkpoint(checkpoint.as_bytes())?;
                return Err(error);
            }
        } else {
            lease.publish_active_checkpoint(checkpoint.as_bytes())?;
        }
        self.published = true;
        publication_error.map_or(Ok(()), Err)
    }
}

impl Drop for Operation {
    fn drop(&mut self) {
        if !self.published && self.persist().is_err() {
            // Neither a replaced generation nor a failed durable write may return a reusable owner.
            self.cached = None;
            tracing::warn!("native account credential persistence failed");
        }
        if self.rejected {
            self.cached = None;
        }
        if let Ok(mut state) = self.account.state.lock() {
            state.cached = self.cached.take();
            state.busy = false;
        }
    }
}

/// Chooses the current application's Android policy explicitly, independent of SDK defaults.
pub(super) fn authentication_policy() -> xbox::Config {
    xbox::Config {
        client_id: "0000000048183522".into(),
        user_agent: "XAL Android 2025.11.20251111.000".into(),
        device: xbox::Device::Android,
        device_version: "13".into(),
        sandbox: "RETAIL".into(),
    }
}

/// Formats an acquired absolute expiry without panicking on an unrepresentable service value.
pub(super) fn oauth_expiry(expires: std::time::SystemTime) -> io::Result<String> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidInput, "invalid native OAuth expiry");
    let elapsed = expires.duration_since(UNIX_EPOCH).map_err(|_| invalid())?;
    let nanos = i128::try_from(elapsed.as_nanos()).map_err(|_| invalid())?;
    OffsetDateTime::from_unix_timestamp_nanos(nanos)
        .map_err(|_| invalid())?
        .format(&Rfc3339)
        .map_err(|_| invalid())
}

/// Imports the leased refresh credential conservatively; process-local expiry anchors are not portable.
fn import_credentials(lease: &NativeLease, policy: &xbox::Config) -> io::Result<Credentials> {
    Credentials::from_tokens(
        policy.client_id.clone(),
        String::new(),
        lease.refresh_token().to_owned(),
        UNIX_EPOCH,
    )
    .map_err(service_error)
}

/// Keeps HTTP bodies, URLs and credential values out of the application error surface.
fn service_error(error: bedrock_services::Error) -> io::Error {
    if matches!(error, bedrock_services::Error::SignInRequired) {
        io::Error::other(RenewSignIn)
    } else {
        io::Error::other("native account service acquisition failed; retry or sign in again")
    }
}

#[derive(Debug, thiserror::Error)]
#[error("Microsoft sign-in must be renewed")]
struct RenewSignIn;

/// Identifies only an explicit OAuth invalid-grant response, without guessing from service failures.
pub(super) fn requires_sign_in(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|cause| cause.is::<RenewSignIn>())
}

#[cfg(test)]
mod tests;
