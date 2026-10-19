//! The original Login proof and credential policy used for local HTTP SDP.

use super::*;
use crate::login::native_network::transport::http::LocalIdentity;
use nethernet::identity::{SelfSignedIdentity, TokenTrust};

impl Credentials {
    /// Retains one proof key for both SDP and Login without acquiring another account credential.
    pub(super) fn local_http_identity(&self) -> io::Result<LocalIdentity> {
        if self.messaging.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "local HTTP cannot consume a Messaging handoff",
            ));
        }
        match (&self.authentication, &self.sdp) {
            (Some((token, _, expires)), Some(policy)) => {
                let (identity, trust) = authentication::identity_policy(
                    Arc::clone(&self.key),
                    token,
                    policy.clock,
                    Arc::clone(&policy.verifier),
                    *expires,
                )?;
                let TokenTrust::Provider(provider) = trust else {
                    unreachable!("qualified identity policy retains its configured provider");
                };
                Ok(LocalIdentity {
                    identity,
                    trust: TokenTrust::ServerAnswer(provider),
                    expires: Some(*expires),
                })
            }
            (None, None) => Ok(LocalIdentity {
                identity: Arc::new(
                    SelfSignedIdentity::from_key(
                        Arc::clone(&self.key),
                        SystemTime::now(),
                        crate::NATIVE_JOIN_TIMEOUT,
                    )
                    .map_err(io::Error::other)?,
                ),
                trust: authentication::offline_answer_trust(),
                expires: None,
            }),
            _ => Err(io::Error::other(
                "local HTTP credential policy is unavailable",
            )),
        }
    }
}

#[cfg(test)]
mod tests;
