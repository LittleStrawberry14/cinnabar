//! Signed transport handoff, retaining account qualification separately from peer trust.

use super::{NativeServiceClock, NativeTokenVerifier, ensure_current};
use bedrock_net::messaging::PreparedMessaging;
use nethernet::identity::{
    SdpIdentity, TokenIdentity, TokenTrust, TokenVerifier,
    error::{IdentityError, Result},
    jwt::Claims,
};
use p384::ecdsa::SigningKey;
use std::{
    io,
    sync::Arc,
    time::{Instant, SystemTime},
};

pub(crate) struct MessagingAdmission {
    pub(crate) prepared: PreparedMessaging,
    pub(crate) target: crate::NativeMessagingTarget,
    pub(crate) identity: Arc<dyn SdpIdentity>,
    pub(crate) trust: TokenTrust,
    pub(crate) expires: Instant,
}

impl MessagingAdmission {
    /// Uses the one originally minted proof owner for the existing SDP identity implementation.
    pub(super) fn new(
        prepared: PreparedMessaging,
        target: crate::NativeMessagingTarget,
        key: Arc<SigningKey>,
        token: &str,
        clock: NativeServiceClock,
        verifier: Arc<NativeTokenVerifier>,
        expires: Instant,
    ) -> io::Result<Self> {
        ensure_current(expires, Instant::now())?;
        let identity = TokenIdentity::new(key, token.to_owned(), clock.now(Instant::now())?)
            .map_err(|_| super::verifier::rejected())?;
        Ok(Self {
            prepared,
            target,
            identity: Arc::new(ServiceIdentity {
                identity,
                clock,
                expires,
            }),
            trust: TokenTrust::Provider(Arc::new(PeerVerifier { verifier, clock })),
            expires,
        })
    }
}

struct ServiceIdentity {
    identity: TokenIdentity,
    clock: NativeServiceClock,
    expires: Instant,
}

impl SdpIdentity for ServiceIdentity {
    /// Uses the mint's service clock rather than the runtime adapter's host UTC argument.
    fn augment(&self, sdp: &str, _now: SystemTime) -> Result<String> {
        let now = Instant::now();
        ensure_current(self.expires, now).map_err(|_| rejected())?;
        self.identity
            .augment(sdp, self.clock.now(now).map_err(|_| rejected())?)
    }
}

struct PeerVerifier {
    verifier: Arc<NativeTokenVerifier>,
    clock: NativeServiceClock,
}

impl TokenVerifier for PeerVerifier {
    /// Checks the remote peer's own signed claims; local selected XUID/proof binding is never applied.
    fn verify(&self, token: &str, _now: SystemTime) -> Result<Claims> {
        let now = self.clock.now(Instant::now()).map_err(|_| rejected())?;
        self.verifier
            .verify_peer(token, now)
            .map_err(|_| rejected())
    }
}

/// Keeps peer claim, key and parser diagnostics out of transport errors.
fn rejected() -> IdentityError {
    IdentityError::Untrusted("configured native multiplayer verification failed".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nethernet::identity::{Identity, jwk::EcJwk, jwt::Jws};
    use std::time::{Duration, UNIX_EPOCH};

    /// Host UTC and generic JWT leeway cannot extend the mint's hard monotonic admission deadline.
    #[test]
    fn signed_sdp_uses_original_service_clock_and_hard_deadline() {
        let key = Arc::new(SigningKey::from_slice(&[3; 48]).unwrap());
        let service_time = UNIX_EPOCH + Duration::from_secs(1_900_000_000);
        let observed = Instant::now();
        let token = Jws::sign(
            &SigningKey::from_slice(&[4; 48]).unwrap(),
            &serde_json::json!({
                "iss":"synthetic.example", "exp":1_900_000_060,
                "cpk":EcJwk::from(key.verifying_key()),
            })
            .to_string(),
        )
        .unwrap();
        let identity = ServiceIdentity {
            identity: TokenIdentity::new(Arc::clone(&key), token.clone(), service_time).unwrap(),
            clock: NativeServiceClock {
                service_time,
                observed,
            },
            expires: observed + Duration::from_secs(30),
        };
        assert!(std::ptr::eq(identity.identity.signing_key(), key.as_ref()));
        let sdp = "v=0\r\na=fingerprint:sha-256 AB:CD\r\nm=application 9\r\n";
        let signed = identity.augment(sdp, UNIX_EPOCH).unwrap();
        assert_eq!(Identity::from_sdp(&signed).unwrap().assertion.token, token);
        let expired = ServiceIdentity {
            expires: observed,
            ..identity
        };
        assert!(expired.augment(sdp, UNIX_EPOCH).is_err());
    }
}
