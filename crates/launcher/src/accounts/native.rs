//! Generation-fenced native credentials using the account store's existing private files and leases.

use super::*;
use serde::{Deserializer, Serializer};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

const PREFIX: &[u8] = b"CNAC\x01";
const MAX_GENERATION_BYTES: usize = 128;

/// One selected sign-in, held against Go refresh and account-store replacement.
/// This owner has no Debug/Clone; callers must acquire nonblocking and bound their wait.
pub struct NativeLease {
    store: AccountStore,
    _store_lock: File,
    _cache_locks: (File, File),
    oauth: OAuth,
    selected_id: Option<String>,
    binding: [u8; 32],
}

#[derive(Deserialize, Serialize)]
struct OAuth {
    #[serde(default, deserialize_with = "secret", serialize_with = "expose")]
    access_token: Zeroizing<String>,
    #[serde(deserialize_with = "secret", serialize_with = "expose")]
    refresh_token: Zeroizing<String>,
    #[serde(default)]
    expiry: Option<String>,
    cinnabar_sign_in_generation: String,
    #[serde(flatten)]
    other: BTreeMap<String, serde_json::Value>,
}

impl AccountStore {
    /// Reads a consistent saved-account snapshot without waiting for an acquisition's store lease.
    /// Frame callers retain their previous view and retry on WouldBlock.
    pub fn try_snapshot(&self) -> io::Result<(Vec<AccountProfile>, Option<String>)> {
        private_directory(&self.directory)?;
        let _lease = try_lease(&self.directory.join("store.lock"))?;
        let index = self.index()?;
        Ok((sorted_profiles(index.profiles.into_values()), index.active))
    }

    /// Removes unfinished sign-in files only when their leases are immediately available.
    pub fn try_discard_pending(&self) -> io::Result<()> {
        private_directory(&self.directory)?;
        let _lease = try_lease(&self.directory.join("store.lock"))?;
        let pending = self.pending_cache();
        let _pending = cache_try_leases(&pending)?;
        remove_optional(&pending)?;
        remove_optional(&derived_path(&pending))
    }

    /// Tries all credential leases once, releasing acquired files if any later lease is busy.
    /// The caller schedules retries off the frame and cancels by dropping its attempt.
    pub fn try_native_lease(&self) -> io::Result<NativeLease> {
        private_directory(&self.directory)?;
        let store_lock = try_lease(&self.directory.join("store.lock"))?;
        let cache_locks = cache_try_leases(&self.active_cache)?;
        let bytes = Zeroizing::new(read_private(&self.active_cache)?);
        let oauth = parse_oauth(&bytes)?;
        let selected_id = self.index()?.active;
        Ok(NativeLease {
            store: self.clone(),
            _store_lock: store_lock,
            _cache_locks: cache_locks,
            oauth,
            selected_id,
            binding: Sha256::digest(&bytes).into(),
        })
    }
}

impl NativeLease {
    /// Returns the immutable sign-in generation, which every later publication must still own.
    pub fn generation(&self) -> &str {
        &self.oauth.cinnabar_sign_in_generation
    }

    /// Returns the selected saved identity when the account picker has verified one.
    pub fn selected_id(&self) -> Option<&str> {
        self.selected_id.as_deref()
    }

    /// Returns the configured OAuth client ID if an imported cache explicitly records one.
    pub fn client_id(&self) -> Option<&str> {
        self.oauth
            .other
            .get("client_id")
            .and_then(serde_json::Value::as_str)
    }

    /// Explicitly borrows the refresh credential while this owner excludes another refresh.
    pub fn refresh_token(&self) -> &str {
        &self.oauth.refresh_token
    }

    /// Identifies the exact OAuth record read under the cross-process lease.
    pub fn binding(&self) -> [u8; 32] {
        self.binding
    }

    /// Reads a checkpoint only for this sign-in generation; SDK policy validation remains separate.
    /// A different OAuth revision is safe only after the SDK adopts this lease's current credentials.
    pub fn checkpoint(&self) -> io::Result<Option<Zeroizing<Vec<u8>>>> {
        let Some(bytes) = read_optional(&checkpoint_path(&self.store, self.selected_id()))? else {
            return Ok(None);
        };
        let bytes = Zeroizing::new(bytes);
        let mut remaining = bytes.as_slice();
        if take(&mut remaining, PREFIX.len())? != PREFIX {
            return Ok(None);
        }
        let length = u16::from_le_bytes(
            take(&mut remaining, 2)?
                .try_into()
                .map_err(|_| invalid("invalid native checkpoint"))?,
        );
        if take(&mut remaining, usize::from(length))? != self.generation().as_bytes() {
            return Ok(None);
        }
        // The caller must replace old credentials before using the restored SDK owner.
        if remaining.is_empty() {
            return Ok(None);
        }
        Ok(Some(Zeroizing::new(remaining.to_vec())))
    }

    /// Rejects removal, replacement or account switching before returning any prepared credential.
    pub fn recheck(&self) -> io::Result<()> {
        let current = Zeroizing::new(read_private(&self.store.active_cache)?);
        let binding: [u8; 32] = Sha256::digest(&current).into();
        if parse_oauth(&current)?.cinnabar_sign_in_generation != self.generation()
            || binding != self.binding
            || self.store.index()?.active != self.selected_id
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "saved sign-in changed; reopen the account",
            ));
        }
        Ok(())
    }

    /// Atomically publishes OAuth rotation before the proof checkpoint and saved-account copy.
    /// Unknown OAuth fields and the Go derived cache are preserved. Failure never reports success.
    pub fn publish(
        &mut self,
        access: &str,
        refresh: &str,
        expiry: &str,
        checkpoint: &[u8],
    ) -> io::Result<()> {
        self.recheck()?;
        if refresh.is_empty()
            || access
                .len()
                .saturating_add(refresh.len())
                .saturating_add(checkpoint.len())
                > MAX_FILE_BYTES as usize
        {
            return Err(invalid("invalid native credential size"));
        }
        self.oauth.access_token = Zeroizing::new(access.to_owned());
        self.oauth.refresh_token = Zeroizing::new(refresh.to_owned());
        self.oauth.expiry = Some(expiry.to_owned());
        self.oauth
            .other
            .entry("token_type".to_owned())
            .or_insert_with(|| serde_json::Value::String("Bearer".to_owned()));
        let oauth = Zeroizing::new(
            serde_json::to_vec(&self.oauth).map_err(|_| invalid("serialize native credentials"))?,
        );
        write_private(&self.store.active_cache, &oauth)?;
        self.binding = Sha256::digest(&oauth).into();
        self.publish_checkpoint(checkpoint)?;
        if let Some(id) = &self.selected_id {
            self.store
                .write_credentials(id, &read_credentials(&self.store.active_cache)?)?;
        }
        Ok(())
    }

    /// Publishes proof ownership without expiring or replacing an unchanged OAuth record.
    pub fn publish_checkpoint(&self, checkpoint: &[u8]) -> io::Result<()> {
        self.recheck()?;
        let generation = self.generation();
        if checkpoint
            .len()
            .saturating_add(PREFIX.len() + 2 + generation.len())
            > MAX_FILE_BYTES as usize
        {
            return Err(invalid("invalid native checkpoint size"));
        }
        let length =
            u16::try_from(generation.len()).map_err(|_| invalid("invalid sign-in generation"))?;
        let mut bytes = Zeroizing::new(Vec::with_capacity(
            PREFIX.len() + 2 + generation.len() + checkpoint.len(),
        ));
        bytes.extend_from_slice(PREFIX);
        bytes.extend_from_slice(&length.to_le_bytes());
        bytes.extend_from_slice(generation.as_bytes());
        bytes.extend_from_slice(checkpoint);
        write_private(&checkpoint_path(&self.store, self.selected_id()), &bytes)?;
        if self.selected_id.is_some() {
            remove_optional(&checkpoint_path(&self.store, None))?;
        }
        Ok(())
    }

    /// Reuses the Go-compatible install device file under its own nonblocking lease.
    /// `fresh` replaces a missing or unusable profile; validation belongs to its metadata owner.
    pub fn device_profile(
        &self,
        path: &Path,
        fresh: &[u8],
        valid: impl FnOnce(&[u8]) -> bool,
    ) -> io::Result<Zeroizing<Vec<u8>>> {
        let _lease = try_lease(&cache_lock_path(path))?;
        match read_optional(path)? {
            Some(bytes) if valid(&bytes) => Ok(Zeroizing::new(bytes)),
            _ => {
                write_private(path, fresh)?;
                Ok(Zeroizing::new(fresh.to_vec()))
            }
        }
    }
}

/// Validates the authoritative OAuth generation without echoing credential JSON on failure.
fn parse_oauth(bytes: &[u8]) -> io::Result<OAuth> {
    let oauth: OAuth =
        serde_json::from_slice(bytes).map_err(|_| invalid("invalid native OAuth cache"))?;
    let generation = &oauth.cinnabar_sign_in_generation;
    if oauth.refresh_token.is_empty()
        || generation.is_empty()
        || generation.len() > MAX_GENERATION_BYTES
        || !generation
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(invalid(
            "native account requires an existing valid sign-in generation",
        ));
    }
    Ok(oauth)
}

/// Keeps one slot per saved identity; its envelope rejects an older generation after reauthentication.
fn checkpoint_path(store: &AccountStore, selected_id: Option<&str>) -> PathBuf {
    store.directory.join(format!(
        "native-{}.checkpoint",
        selected_id.unwrap_or("active")
    ))
}

/// Removes the current generation's native secret alongside the existing saved account.
pub(super) fn remove_current_checkpoint(store: &AccountStore) -> io::Result<()> {
    let selected = store.index()?.active;
    remove_optional(&checkpoint_path(store, selected.as_deref()))?;
    remove_optional(&checkpoint_path(store, None))
}

/// Takes a checked prefix without allocating or trusting a persisted length.
fn take<'a>(remaining: &mut &'a [u8], length: usize) -> io::Result<&'a [u8]> {
    let (value, tail) = remaining
        .split_at_checked(length)
        .ok_or_else(|| invalid("invalid native checkpoint"))?;
    *remaining = tail;
    Ok(value)
}

/// Deserializes one secret owner without adding serde implementations to public credentials.
fn secret<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Zeroizing<String>, D::Error> {
    String::deserialize(deserializer).map(Zeroizing::new)
}

/// Explicitly serializes a private storage field through its borrowed secret bytes.
fn expose<S: Serializer>(value: &Zeroizing<String>, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(value)
}

#[cfg(test)]
mod tests;
