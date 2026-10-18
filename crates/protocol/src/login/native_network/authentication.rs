use p384::ecdsa::SigningKey;
use serde_json::{Map, Value, value::RawValue};
use std::io;
use zeroize::Zeroizing;

/// One acquired account credential and the connection proof key it was minted for.
///
/// The application owns account authentication, secure persistence and device
/// policy. This handoff moves the private key into the connection and keeps the
/// credential out of Debug output. It performs no issuer verification or I/O.
pub struct NativeAuthentication {
    pub(super) key: SigningKey,
    pub(super) token: Zeroizing<String>,
    pub(super) claims: Map<String, Value>,
}

impl NativeAuthentication {
    /// Takes an acquired multiplayer token and complete application-owned client data.
    ///
    /// Acquire the token for this key. Supply account/device/version claims for
    /// the same sign-in policy; the connection supplies ServerAddress and Nonce.
    /// The key is retained through client-data signing and server key exchange.
    ///
    /// # Errors
    /// Rejects an empty/oversized credential or oversized/non-object client JSON
    /// before transport establishment. Errors contain no credential bytes.
    pub fn new(
        key: SigningKey,
        multiplayer_token: String,
        client_data: &RawValue,
    ) -> io::Result<Self> {
        let token = Zeroizing::new(multiplayer_token);
        let limit = bedrock_net::BatchLimits::default().max_decoded_bytes;
        if token.is_empty() || token.len() > limit || client_data.get().len() > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "native account credential or client data exceeds its contract",
            ));
        }
        let Value::Object(claims) = serde_json::from_str(client_data.get()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "invalid native client data")
        })?
        else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "native client data must be a JSON object",
            ));
        };
        Ok(Self { key, token, claims })
    }
}
