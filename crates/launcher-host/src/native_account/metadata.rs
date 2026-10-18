use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io;

const ANDROID_DEVICE_OS: u32 = 1;
const TOUCH_INPUT_MODE: u32 = 2;
const MOBILE_PLATFORM: u32 = 1;
const DEFAULT_MODEL: &str = "GOOGLE Pixel 8";
const SERVICE_NAMESPACE: &str =
    include_str!("../../../../core/authcache/service_device_namespace.txt");

/// The install's shared Go-compatible Android login profile, without a copied model catalog.
#[derive(Serialize, Deserialize)]
pub(super) struct DeviceProfile {
    os: u32,
    model: String,
    id: String,
}

impl DeviceProfile {
    /// Chooses one supported profile and a stable lower-hex device identifier for a new install.
    pub fn fresh() -> Self {
        Self {
            os: ANDROID_DEVICE_OS,
            model: DEFAULT_MODEL.to_owned(),
            id: uuid::Uuid::new_v4().simple().to_string(),
        }
    }

    /// Accepts the install's existing Android profile while bounding every claimed string.
    pub fn parse(bytes: &[u8]) -> io::Result<Self> {
        let profile: Self = serde_json::from_slice(bytes)
            .map_err(|_| io::Error::other("invalid native device profile"))?;
        if profile.os != ANDROID_DEVICE_OS
            || profile.model.is_empty()
            || profile.model.len() > 256
            || profile.id.len() != 32
            || !profile
                .id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(io::Error::other(
                "native account requires a valid Android device profile",
            ));
        }
        Ok(profile)
    }

    /// Applies account-owned device metadata to the shared skin/settings claim schema.
    pub fn apply(&self, claims: &mut Value, random_id: protocol::ClientRandomId, gamertag: &str) {
        claims["DeviceOS"] = json!(self.os);
        claims["DeviceModel"] = json!(self.model);
        claims["DeviceId"] = json!(self.id);
        claims["DefaultInputMode"] = json!(TOUCH_INPUT_MODE);
        if claims["PlatformType"].as_u64().unwrap_or_default() == 0 {
            claims["PlatformType"] = json!(MOBILE_PLATFORM);
        }
        claims["ThirdPartyName"] = json!(gamertag);
        claims["SelfSignedId"] = json!(self_signed_id(random_id, &self.id));
    }
}

/// Derives the shared service-device identity from the account XUID and one cross-language namespace.
pub(super) fn service_device_id(xuid: &str) -> io::Result<uuid::Uuid> {
    let namespace = uuid::Uuid::parse_str(SERVICE_NAMESPACE.trim())
        .map_err(|_| io::Error::other("invalid service device namespace"))?;
    Ok(uuid::Uuid::new_v5(&namespace, xuid.as_bytes()))
}

/// Preserves the signed login-ID bit pattern and per-word UUID byte order.
/// Adapted from Gophertunnel's MIT-licensed self-signed ID derivation; see THIRD_PARTY_NOTICES.md.
fn self_signed_id(random_id: protocol::ClientRandomId, device: &str) -> uuid::Uuid {
    let mut hash = Md5::new();
    hash.update(random_id.get().to_le_bytes());
    hash.update(device.as_bytes());
    let mut bytes: [u8; 16] = hash.finalize().into();
    bytes[..8].reverse();
    bytes[8..].reverse();
    bytes[6] = (bytes[6] & 0x0f) | 0x30;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Repeated account joins reuse device/skin/settings metadata and full-width signed IDs.
    #[test]
    fn account_device_claims_preserve_the_shared_schema_and_signed_id_vectors() {
        for (id, expected) in [
            (
                0x0102_0304_0506_0708,
                "62c686b6-9dd2-3c26-ac8d-6da654ca1b3f",
            ),
            (-1, "3e6f158c-edd9-3059-b123-396df53ce814"),
            (i64::MIN, "5edb9ba0-4706-3dd7-a52b-24e7790f4d98"),
        ] {
            assert_eq!(
                self_signed_id(
                    protocol::ClientRandomId::new(id).unwrap(),
                    "audit-device-id"
                )
                .to_string(),
                expected
            );
        }
        let profile = DeviceProfile::fresh();
        let random_id = protocol::ClientRandomId::new(-7).unwrap();
        let settings = protocol::LoginSettings {
            language_code: "fr_FR".into(),
            ..Default::default()
        };
        let mut claims = protocol::login_client_data("offline-name", random_id, None, &settings);
        profile.apply(&mut claims, random_id, "AccountName");
        assert_eq!(claims["ThirdPartyName"], "AccountName");
        assert_eq!(claims["DeviceId"], profile.id);
        assert_eq!(claims["LanguageCode"], settings.language_code);
        assert_eq!(claims["SkinImageWidth"], 64);
        assert_eq!(claims["PlatformType"], MOBILE_PLATFORM);
        let repeated = self_signed_id(random_id, &profile.id).to_string();
        assert_eq!(claims["SelfSignedId"], repeated);
        assert_eq!(
            service_device_id("123").unwrap(),
            service_device_id("123").unwrap()
        );
        assert_ne!(
            service_device_id("123").unwrap(),
            service_device_id("456").unwrap()
        );
    }
}
