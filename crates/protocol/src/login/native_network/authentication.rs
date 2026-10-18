//! Qualified provider credentials and their original finite validity.

mod verifier;
pub use verifier::NativeTokenVerifier;

use p384::ecdsa::SigningKey;
use serde_json::{Map, Value, value::RawValue};
use std::{
    io,
    time::{Instant, SystemTime},
};
use zeroize::Zeroizing;

/// One acquired response with its service-clock observation and original hard validity.
/// No clock is read, and no token lifetime is restarted by this owner.
pub struct NativeCredential {
    token: Zeroizing<String>,
    service_time: SystemTime,
    observed: Instant,
    hard_expiry: SystemTime,
}

impl NativeCredential {
    /// Captures a mint response. Observe monotonic time before sampling service time.
    pub fn new(
        token: String,
        service_time: SystemTime,
        hard_expiry: SystemTime,
        observed: Instant,
    ) -> io::Result<Self> {
        let token = Zeroizing::new(token);
        if token.is_empty()
            || token.len() > verifier::MAX_TOKEN_BYTES
            || hard_expiry <= service_time
        {
            return Err(expired());
        }
        verifier::parsed(&token)?;
        Ok(Self {
            token,
            service_time,
            observed,
            hard_expiry,
        })
    }

    /// Borrows the acquired credential only for the provider's bounded key-cache lookup.
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Rejects a mint that expired while waiting for provider metadata or connection work.
    pub fn ensure_current(&self, now: Instant) -> io::Result<()> {
        if self.now(now)? < self.hard_expiry {
            Ok(())
        } else {
            Err(expired())
        }
    }

    /// Advances the original service UTC observation with caller-supplied monotonic time.
    fn now(&self, now: Instant) -> io::Result<SystemTime> {
        self.service_time
            .checked_add(
                now.checked_duration_since(self.observed)
                    .ok_or_else(expired)?,
            )
            .ok_or_else(expired)
    }
}

/// One verified provider credential and the connection proof key it was minted for.
///
/// The application owns account authentication, secure persistence and device
/// policy. This handoff moves the private key into the connection and keeps the
/// credential out of Debug output. Verification is pure; HTTP remains provider-owned.
pub struct NativeAuthentication {
    pub(super) key: SigningKey,
    pub(super) token: Zeroizing<String>,
    pub(super) claims: Map<String, Value>,
    pub(super) expires: Instant,
}

impl NativeAuthentication {
    /// Qualifies an acquired credential against trusted provider keys and the selected Xbox identity.
    ///
    /// Acquire the token for this key. Supply account/device/version claims for
    /// the same sign-in policy; the connection supplies ServerAddress and Nonce.
    /// The key is retained through client-data signing and server key exchange.
    ///
    /// # Errors
    /// Rejects untrusted, wrong-identity/key or expired credentials and malformed client JSON.
    /// The signed display name overrides stale application display-name metadata.
    pub fn new(
        key: SigningKey,
        credential: NativeCredential,
        client_data: &RawValue,
        verifier: &NativeTokenVerifier,
        selected_xuid: &str,
        now: Instant,
    ) -> io::Result<Self> {
        credential.ensure_current(now)?;
        let limit = bedrock_net::BatchLimits::default().max_decoded_bytes;
        if client_data.get().len() > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "native account credential or client data exceeds its contract",
            ));
        }
        let Value::Object(mut claims) = serde_json::from_str(client_data.get()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "invalid native client data")
        })?
        else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "native client data must be a JSON object",
            ));
        };
        let service_now = credential.now(now)?;
        let identity = verifier.verify(
            &credential.token,
            key.verifying_key(),
            selected_xuid,
            service_now,
        )?;
        let expiry = identity
            .expiry()
            .expect("qualified identity has expiry")
            .min(credential.hard_expiry);
        let expires = credential
            .observed
            .checked_add(
                expiry
                    .duration_since(credential.service_time)
                    .map_err(|_| expired())?,
            )
            .ok_or_else(expired)?;
        ensure_current(expires, now)?;
        claims.insert(
            "ThirdPartyName".into(),
            Value::String(
                identity
                    .display_name()
                    .expect("qualified identity has name")
                    .into(),
            ),
        );
        Ok(Self {
            key,
            token: credential.token,
            claims,
            expires,
        })
    }
}

/// Rejects expired admission without re-anchoring a copied remaining lifetime.
pub(super) fn ensure_current(expires: Instant, now: Instant) -> io::Result<()> {
    if now < expires {
        Ok(())
    } else {
        Err(expired())
    }
}

/// Identifies only the finite credential deadline, preserving transport errors separately.
pub(super) fn is_expired(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|cause| cause.is::<ExpiredCredential>())
}

#[derive(Debug, thiserror::Error)]
#[error("native multiplayer credential expired before Login")]
struct ExpiredCredential;

/// Omits token, identity, provider URL and key diagnostics.
pub(super) fn expired() -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, ExpiredCredential)
}
