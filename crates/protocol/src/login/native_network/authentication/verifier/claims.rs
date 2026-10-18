//! Selected-account and temporal policy after issuer signature verification.

use super::rejected;
use nethernet::identity::jwt::Claims;
use p384::ecdsa::VerifyingKey;
use std::{
    io,
    time::{Duration, SystemTime},
};

const CLAIMS_LEEWAY: Duration = Duration::from_secs(60);

/// Applies the configured Go verifier's hard expiry plus its subsequent claims/IdentityData checks.
/// Subject must be present for native provider qualification; it is not assumed to equal Xbox XUID.
pub(super) fn validate(
    claims: &Claims,
    selected_xuid: &str,
    key: &VerifyingKey,
    now: SystemTime,
) -> io::Result<()> {
    if claims.subject().is_none_or(str::is_empty)
        || claims.xuid() != Some(selected_xuid)
        || selected_xuid.is_empty()
        || selected_xuid.parse::<i64>().is_err()
        || claims.client_public_key().map_err(|_| rejected())? != *key
        || claims.expiry().is_none_or(|expiry| now >= expiry)
    {
        return Err(rejected());
    }
    let latest = now.checked_add(CLAIMS_LEEWAY).ok_or_else(rejected)?;
    for name in ["nbf", "iat"] {
        if claims
            .numeric_date(name)
            .map_err(|_| rejected())?
            .is_some_and(|time| time > latest)
        {
            return Err(rejected());
        }
    }
    if let Some(value) = claims
        .values()
        .get("leguuid")
        .filter(|value| !value.is_null())
    {
        let value = value.as_str().ok_or_else(rejected)?;
        if !value.is_empty() && uuid::Uuid::parse_str(value).map_or(true, |id| id.is_nil()) {
            return Err(rejected());
        }
    }
    let name = claims.display_name().ok_or_else(rejected)?;
    if name.is_empty()
        || name.len() > 15
        || name.starts_with(' ')
        || name.ends_with(' ')
        || name.as_bytes()[0].is_ascii_digit()
        || name.contains("  ")
        || !name
            .bytes()
            .any(|byte| byte.is_ascii_alphanumeric() || byte == b' ')
    {
        return Err(rejected());
    }
    Ok(())
}
