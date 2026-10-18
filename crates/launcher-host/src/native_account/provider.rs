//! One account's bounded, caller-driven OpenID provider cache.

use p384::ecdsa::SigningKey;
use protocol::{NativeAuthentication, NativeCredential, NativeTokenVerifier};
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use serde_json::value::RawValue;
use std::{
    io,
    time::{Duration, Instant},
};
use url::Url;

const KEY_REFRESH_INTERVAL: Duration = Duration::from_secs(30 * 60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Owns reusable public verification metadata alongside the concrete account operation.
/// No mutex, task, timer or runtime is created here.
pub(super) struct Provider {
    issuer: Url,
    expected_issuer: String,
    client: Client,
    configuration: Option<Configuration>,
    verifier: Option<NativeTokenVerifier>,
    fetched: Option<Instant>,
}

#[derive(Deserialize)]
struct Configuration {
    jwks_uri: String,
    #[serde(default)]
    id_token_signing_alg_values_supported: Option<Vec<String>>,
}

impl Provider {
    /// Builds an unauthenticated, redirect-free HTTPS client only when qualification is first needed.
    pub(super) fn new(issuer: &str) -> io::Result<Self> {
        let expected_issuer = intended_issuer(issuer)?;
        Ok(Self {
            issuer: secure_url(&expected_issuer)?,
            expected_issuer,
            client: Client::builder()
                .https_only(true)
                .redirect(reqwest::redirect::Policy::none())
                .no_gzip()
                .no_brotli()
                .no_deflate()
                .no_zstd()
                .timeout(REQUEST_TIMEOUT)
                .connect_timeout(REQUEST_TIMEOUT)
                .user_agent(bedrock_services::minecraft::USER_AGENT)
                .build()
                .map_err(|_| failed())?,
            configuration: None,
            verifier: None,
            fetched: None,
        })
    }

    /// Prevents an account from reusing keys acquired for a different discovered environment.
    pub(super) fn matches(&self, issuer: &str) -> io::Result<bool> {
        Ok(self.expected_issuer == intended_issuer(issuer)?)
    }

    /// Qualifies one key-bound credential without replacing its original service-time observation.
    /// Known keys are reused; an unknown kid can refresh only after the baseline's 30-minute interval.
    pub(super) async fn authentication(
        &mut self,
        key: SigningKey,
        credential: NativeCredential,
        client_data: &RawValue,
        xuid: &str,
    ) -> io::Result<NativeAuthentication> {
        credential.ensure_current(Instant::now())?;
        let unknown = match &self.verifier {
            Some(verifier) => !verifier.recognizes(credential.token())?,
            None => true,
        };
        let now = Instant::now();
        if self.verifier.is_none()
            || (unknown
                && self
                    .fetched
                    .is_some_and(|last| now.duration_since(last) >= KEY_REFRESH_INTERVAL))
        {
            self.refresh().await?;
        }
        NativeAuthentication::new(
            key,
            credential,
            client_data,
            self.verifier.as_ref().ok_or_else(failed)?,
            xuid,
            Instant::now(),
        )
    }

    /// Acquires configuration once and atomically replaces the last successful bounded key set.
    async fn refresh(&mut self) -> io::Result<()> {
        if self.configuration.is_none() {
            let mut url = self.issuer.clone();
            url.path_segments_mut()
                .map_err(|_| failed())?
                .pop_if_empty()
                .extend([".well-known", "openid-configuration"]);
            let bytes = self.document(url).await?;
            let configuration: Configuration =
                serde_json::from_slice(&bytes).map_err(|_| failed())?;
            secure_url(&configuration.jwks_uri)?;
            self.configuration = Some(configuration);
        }
        let configuration = self
            .configuration
            .as_ref()
            .expect("acquired OpenID configuration");
        let bytes = self.document(secure_url(&configuration.jwks_uri)?).await?;
        let verifier = NativeTokenVerifier::new(
            self.expected_issuer.clone(),
            configuration
                .id_token_signing_alg_values_supported
                .as_deref()
                .unwrap_or(&[]),
            &bytes,
        )?;
        self.verifier = Some(verifier);
        self.fetched = Some(Instant::now());
        Ok(())
    }

    /// Bounds declared and streamed public metadata before deserialization; errors reveal no body or URL.
    async fn document(&self, url: Url) -> io::Result<Vec<u8>> {
        let mut response = self
            .client
            .get(url)
            .header("accept", "application/json")
            .header("content-type", "application/json")
            .send()
            .await
            .map_err(|_| failed())?;
        let limit = NativeTokenVerifier::MAX_DOCUMENT_BYTES;
        if response.status() != StatusCode::OK
            || response
                .content_length()
                .is_some_and(|length| length > limit as u64)
        {
            return Err(failed());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| failed())? {
            if chunk.len() > limit.saturating_sub(bytes.len()) {
                return Err(failed());
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }
}

/// Matches the configured Go URL.JoinPath() path cleaning, preserving whether a trailing slash existed.
/// The expected issuer is independent of Rust's HTTP URL serialization (which adds a root slash).
fn intended_issuer(value: &str) -> io::Result<String> {
    let url = secure_url(value)?;
    let authority = value.find("://").ok_or_else(failed)? + 3;
    let path_start = value[authority..]
        .find('/')
        .map_or(value.len(), |index| authority + index);
    let path = &value[path_start..];
    let authority = format!("{}://{}", url.scheme(), &value[authority..path_start]);
    if path.is_empty() {
        return Ok(authority);
    }
    let mut segments = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            _ => segments.push(segment),
        }
    }
    let mut result = format!("{}/{}", authority, segments.join("/"));
    if path.ends_with('/') && !result.ends_with('/') {
        result.push('/');
    }
    Ok(result)
}

/// Public verification metadata never follows plaintext, credentials, fragments or query policy overrides.
fn secure_url(value: &str) -> io::Result<Url> {
    if value.len() > NativeTokenVerifier::MAX_PROVIDER_URL_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(failed());
    }
    let url = Url::parse(value).map_err(|_| failed())?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.query().is_some()
    {
        return Err(failed());
    }
    Ok(url)
}

/// Keeps service and environment diagnostics out of the public authentication error surface.
fn failed() -> io::Error {
    io::Error::other("native multiplayer provider acquisition failed; retry or sign in again")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Prevents HTTP URL canonicalization from silently changing the configured JWT issuer policy.
    #[test]
    fn native_provider_preserves_configured_issuer_path_and_trailing_slash() {
        for (configured, expected) in [
            ("https://issuer.example", "https://issuer.example"),
            ("https://issuer.example/", "https://issuer.example/"),
            (
                "https://issuer.example/tenant",
                "https://issuer.example/tenant",
            ),
            (
                "https://issuer.example/tenant/",
                "https://issuer.example/tenant/",
            ),
            (
                "https://issuer.example/a//b/../tenant/",
                "https://issuer.example/a/tenant/",
            ),
        ] {
            assert_eq!(intended_issuer(configured).unwrap(), expected);
        }
    }
}
