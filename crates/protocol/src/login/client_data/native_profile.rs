//! Pure device metadata shared by protected desktop and ephemeral gateway account owners.

use super::{DISPLAY_NAME, RawClientData};
use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io;

use bridge::contract::{
    NATIVE_ACCOUNT_DEFAULT_MODEL as DEFAULT_MODEL, NATIVE_ACCOUNT_DEVICE_OS as ANDROID_DEVICE_OS,
    NATIVE_ACCOUNT_INPUT_MODE as TOUCH_INPUT_MODE, NATIVE_ACCOUNT_PLATFORM_TYPE as MOBILE_PLATFORM,
};
const SERVICE_NAMESPACE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../core/authcache/service_device_namespace.txt"
));

/// The install's shared Go-compatible Android login profile, without a copied model catalog.
#[derive(Serialize, Deserialize)]
pub struct NativeDeviceProfile {
    os: u32,
    model: String,
    id: String,
}

impl NativeDeviceProfile {
    /// Chooses one supported profile and a stable lower-hex device identifier for a new install.
    pub fn fresh() -> Self {
        Self {
            os: ANDROID_DEVICE_OS,
            model: DEFAULT_MODEL.to_owned(),
            id: uuid::Uuid::new_v4().simple().to_string(),
        }
    }

    /// Accepts the install's existing Android profile while bounding every claimed string.
    ///
    /// # Errors
    /// Rejects malformed JSON, a different platform or invalid model/device identifier.
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
    ///
    /// # Errors
    /// Rejects client metadata that is not a JSON object.
    pub fn apply(
        &self,
        claims: &mut Value,
        random_id: crate::ClientRandomId,
        gamertag: &str,
    ) -> io::Result<()> {
        if !claims.is_object() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "native client data must be a JSON object",
            ));
        }
        let platform_default = claims["PlatformType"].as_u64().unwrap_or_default() == 0;
        for (name, value) in self.overlays(random_id.get(), gamertag, platform_default) {
            claims[name] = value;
        }
        Ok(())
    }

    /// Applies the same owned device values while retaining raw relay metadata and full wire IDs.
    /// The application must validate the original name/skin/title data before credential work.
    pub fn apply_relay(
        &self,
        claims: &mut RawClientData,
        random_id: i64,
        gamertag: &str,
    ) -> io::Result<()> {
        let platform_default = claims.integer("PlatformType")?.unwrap_or_default() == 0;
        for (name, value) in self.overlays(random_id, gamertag, platform_default) {
            claims.insert(name, value)?;
        }
        Ok(())
    }

    /// The sole device/derived identity overlay definition for both real claim representations.
    fn overlays(
        &self,
        random_id: i64,
        gamertag: &str,
        platform_default: bool,
    ) -> impl Iterator<Item = (&'static str, Value)> {
        [
            ("DeviceOS", json!(self.os)),
            ("DeviceModel", json!(self.model)),
            ("DeviceId", json!(self.id)),
            ("DefaultInputMode", json!(TOUCH_INPUT_MODE)),
            (DISPLAY_NAME, json!(gamertag)),
            ("SelfSignedId", json!(self_signed_id(random_id, &self.id))),
        ]
        .into_iter()
        .chain(platform_default.then(|| ("PlatformType", json!(MOBILE_PLATFORM))))
    }
}

impl NativeDeviceProfile {
    /// Derives the shared service UUID from the account XUID and existing Go namespace.
    /// SDK-specific formatting stays with each concrete account owner.
    ///
    /// # Errors
    /// Rejects an invalid compiled service namespace.
    pub fn service_device_id(xuid: &str) -> io::Result<uuid::Uuid> {
        let namespace = uuid::Uuid::parse_str(SERVICE_NAMESPACE.trim())
            .map_err(|_| io::Error::other("invalid service device namespace"))?;
        Ok(uuid::Uuid::new_v5(&namespace, xuid.as_bytes()))
    }
}

/// Preserves the signed login-ID bit pattern and per-word UUID byte order.
/// Adapted from Gophertunnel's MIT-licensed self-signed ID derivation; see THIRD_PARTY_NOTICES.md.
fn self_signed_id(random_id: i64, device: &str) -> uuid::Uuid {
    let mut hash = Md5::new();
    hash.update(random_id.to_le_bytes());
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
            assert_eq!(self_signed_id(id, "audit-device-id").to_string(), expected);
        }
        let profile = NativeDeviceProfile::fresh();
        let random_id = crate::ClientRandomId::new(-7).unwrap();
        let settings = crate::LoginSettings {
            language_code: "fr_FR".into(),
            ..Default::default()
        };
        let mut claims = crate::login_client_data("offline-name", random_id, None, &settings);
        profile
            .apply(&mut claims, random_id, "AccountName")
            .unwrap();
        assert_eq!(claims["ThirdPartyName"], "AccountName");
        assert_eq!(claims["DeviceId"], profile.id);
        assert_eq!(claims["LanguageCode"], settings.language_code);
        assert_eq!(claims["SkinImageWidth"], 64);
        assert_eq!(claims["PlatformType"], MOBILE_PLATFORM);
        let repeated = self_signed_id(random_id.get(), &profile.id).to_string();
        assert_eq!(claims["SelfSignedId"], repeated);
        assert_eq!(
            NativeDeviceProfile::service_device_id("123")
                .unwrap()
                .simple()
                .to_string(),
            "fa540d627071506d92c1e6888b3099e6"
        );
        assert_ne!(
            NativeDeviceProfile::service_device_id("123").unwrap(),
            NativeDeviceProfile::service_device_id("456").unwrap()
        );
    }
}
