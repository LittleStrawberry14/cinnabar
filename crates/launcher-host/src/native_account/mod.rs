//! Selected native account ownership and connection preparation on the caller's async runtime.

mod metadata;

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

const ACQUISITION_TIMEOUT: Duration = Duration::from_secs(30);
const LEASE_RETRY: Duration = Duration::from_millis(25);
const XBOX_RELYING_PARTY: &str = "http://xboxlive.com";

/// Retains proof and service credentials for one selected application account.
/// No runtime, polling task or HTTP request is created until `prepare` is polled.
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
    gamertag: String,
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
        mut claims: Value,
        random_id: protocol::ClientRandomId,
    ) -> io::Result<protocol::NativeAuthentication> {
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
                Err(error) => return Err(error),
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
        let policy = xbox::Config::default();
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
                gamertag: String::new(),
            });
        } else if let Some(cached) = operation.cached.as_mut()
            && cached.binding != lease.binding()
        {
            cached
                .owner
                .replace_credentials(import_credentials(lease, &policy)?)?;
            cached.binding = lease.binding();
        }

        if let Owner::Xbox(account) = &mut operation.cached.as_mut().expect("account owner").owner {
            let identity = account
                .authorize(XBOX_RELYING_PARTY)
                .await
                .map_err(service_error)?;
            if operation
                .lease
                .as_ref()
                .expect("acquired lease")
                .selected_id()
                .is_some_and(|id| id != identity.xuid())
            {
                operation.cached = None;
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "native account identity does not match the selection",
                ));
            }
            let version = u32::try_from(protocol::PROTOCOL_VERSION)
                .map_err(|_| io::Error::other("invalid application protocol version"))?;
            let config = minecraft::Config::desktop(
                protocol::GAME_VERSION,
                version,
                metadata::service_device_id(identity.xuid())?,
            );
            operation.persist()?;
            let Cached {
                generation,
                binding,
                owner,
                ..
            } = operation.cached.take().expect("account owner");
            let Owner::Xbox(account) = owner else {
                unreachable!("checked Xbox owner")
            };
            let services = minecraft::Minecraft::new(*account, config).map_err(service_error)?;
            operation.cached = Some(Cached {
                generation,
                binding,
                owner: Owner::Minecraft(Box::new(services)),
                gamertag: identity.gamertag().to_owned(),
            });
            operation.published = false;
        }
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
        let cached = operation.cached.as_mut().expect("account owner");
        let Owner::Minecraft(services) = &mut cached.owner else {
            unreachable!("prepared Minecraft owner")
        };
        let token = services
            .multiplayer_token(key.verifying_key())
            .await
            .map_err(service_error)?;
        profile.apply(&mut claims, random_id, &cached.gamertag);
        let raw = serde_json::value::to_raw_value(&claims)
            .map_err(|_| io::Error::other("serialize native client data"))?;
        let prepared = protocol::NativeAuthentication::new(key, token.as_str().to_owned(), &raw)?;
        operation.persist()?;
        Ok(prepared)
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
        })
    }
}

impl Owner {
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
        let checkpoint = cached.owner.checkpoint()?;
        let credentials = cached.owner.credentials();
        if credentials.access_token().is_empty() {
            lease.publish_checkpoint(checkpoint.as_bytes())?;
        } else {
            let elapsed = credentials
                .expires_at()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| io::Error::other("invalid native OAuth expiry"))?;
            let nanos = i128::try_from(elapsed.as_nanos())
                .map_err(|_| io::Error::other("invalid native OAuth expiry"))?;
            let expiry = OffsetDateTime::from_unix_timestamp_nanos(nanos)
                .map_err(|_| io::Error::other("invalid native OAuth expiry"))?
                .format(&Rfc3339)
                .map_err(|_| io::Error::other("invalid native OAuth expiry"))?;
            lease.publish(
                credentials.access_token(),
                credentials.refresh_token(),
                &expiry,
                checkpoint.as_bytes(),
            )?;
        }
        cached.binding = lease.binding();
        self.published = true;
        Ok(())
    }
}

impl Drop for Operation {
    fn drop(&mut self) {
        if !self.published && self.persist().is_err() {
            // Neither a replaced generation nor a failed durable write may return a reusable owner.
            self.cached = None;
            tracing::warn!("native account credential persistence failed");
        }
        if let Ok(mut state) = self.account.state.lock() {
            state.cached = self.cached.take();
            state.busy = false;
        }
    }
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
fn service_error(_: bedrock_services::Error) -> io::Error {
    io::Error::other("native account service acquisition failed; retry or sign in again")
}

#[cfg(test)]
mod tests;
