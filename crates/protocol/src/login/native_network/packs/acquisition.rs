//! Runtime-only HTTP and persistent archive I/O for native direct joins.

use super::transfer::Download;
use resource_pack::cache::{ArchiveCache, ArchiveCacheKey, DEFAULT_CACHE_QUOTA};
use std::{
    io,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{sync::Semaphore, task::JoinSet};

/// Cancels any remaining bounded filesystem work when the join future is dropped.
pub(super) struct Cancellation(pub(super) Arc<AtomicBool>);
impl Cancellation {
    /// Creates cancellation state independent of any Tokio runtime or task handle.
    pub(super) fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }
}
impl Drop for Cancellation {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Opens the configured directory off the async executor; cache failures remain misses.
pub(super) async fn open_cache(root: Option<&Path>) -> Option<Arc<ArchiveCache>> {
    let root = root?.to_owned();
    match tokio::task::spawn_blocking(move || ArchiveCache::open(&root, DEFAULT_CACHE_QUOTA)).await
    {
        Ok(Ok(cache)) => Some(Arc::new(cache)),
        _ => {
            tracing::warn!("native resource-pack cache is unavailable");
            None
        }
    }
}

/// Builds one async HTTP client at the I/O edge, with bounded redirect and request timeouts.
pub(super) fn http_client() -> io::Result<reqwest::Client> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(45))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|_| io::Error::other("pack HTTP client could not initialize"))
}

/// Tries an exact verified cache hit, then CDN; a failed CDN leaves in-band fallback available.
pub(super) async fn preload(
    mut download: Download,
    cache: Option<Arc<ArchiveCache>>,
    client: Option<reqwest::Client>,
    cancelled: Arc<AtomicBool>,
) -> Download {
    let id = download.offer.pack_id_version.pack_uuid;
    let version = download
        .offer
        .pack_id_version
        .pack_version
        .version
        .to_string_lossy()
        .into_owned();
    let key = ArchiveCacheKey::new(id, &version, download.size).ok();
    if let (Some(cache), Some(key)) = (&cache, &key) {
        let (cache, key, cancelled) = (Arc::clone(cache), key.clone(), Arc::clone(&cancelled));
        match tokio::task::spawn_blocking(move || {
            cache.load(&key, &cancelled, std::time::SystemTime::now())
        })
        .await
        {
            Ok(Ok(Some(bytes))) => {
                download.complete = Some(Arc::new(bytes));
                download.cache_hit = true;
                return download;
            }
            Ok(Ok(None)) => {}
            _ => tracing::warn!(%id, "native pack cache read failed; downloading again"),
        }
    }
    if !download.offer.cdn_url.is_empty()
        && let Some(client) = client
    {
        let result = async {
            let url = download
                .offer
                .cdn_url
                .to_str()
                .map_err(|_| invalid("invalid pack CDN URL"))?;
            let url = reqwest::Url::parse(url).map_err(|_| invalid("invalid pack CDN URL"))?;
            if !matches!(url.scheme(), "http" | "https")
                || !url.username().is_empty()
                || url.password().is_some()
                || url.fragment().is_some()
            {
                return Err(invalid("unsupported pack CDN URL"));
            }
            let mut response = client
                .get(url)
                .send()
                .await
                .map_err(|_| invalid("pack CDN request failed"))?;
            if response.status() != reqwest::StatusCode::OK {
                return Err(invalid("pack CDN returned an unsuccessful response"));
            }
            if response
                .content_length()
                .is_some_and(|length| length > download.size)
            {
                return Err(invalid("pack CDN length exceeds advertised size"));
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| invalid("pack CDN body failed"))?
            {
                if bytes.len() as u64 + chunk.len() as u64 > download.size {
                    return Err(invalid("pack CDN body exceeds advertised size"));
                }
                bytes
                    .try_reserve(chunk.len())
                    .map_err(|_| invalid("pack CDN allocation failed"))?;
                bytes.extend_from_slice(&chunk);
            }
            let bytes = Arc::new(bytes);
            verify(Arc::clone(&bytes), id, version).await?;
            Ok(bytes)
        }
        .await;
        match result {
            Ok(bytes) => download.complete = Some(bytes),
            Err(_) => {
                tracing::warn!(%id, "native pack CDN acquisition failed; requesting game chunks")
            }
        }
    }
    download
}

/// Checks archive structure and manifest identity on a blocking worker.
pub(super) async fn verify(bytes: Arc<Vec<u8>>, id: uuid::Uuid, version: String) -> io::Result<()> {
    tokio::task::spawn_blocking(move || {
        resource_pack::verify_archive_identity(&bytes, id, &version)
    })
    .await
    .map_err(|_| invalid("pack validation worker failed"))?
    .map_err(|_| invalid("pack archive or manifest identity is invalid"))
}

/// Joins bounded cache writes before releasing the application's original archive bytes.
pub(super) struct Stores {
    jobs: JoinSet<()>,
    slot: Arc<Semaphore>,
}

impl Stores {
    /// Serializes the cache's blocking filesystem worker while other negotiation continues.
    pub(super) fn new() -> Self {
        Self {
            jobs: JoinSet::new(),
            slot: Arc::new(Semaphore::new(1)),
        }
    }

    /// Queues a best-effort write without copying or delaying the completion response.
    pub(super) fn queue(
        &mut self,
        cache: &Option<Arc<ArchiveCache>>,
        download: &Download,
        cancelled: &Arc<AtomicBool>,
    ) {
        if download.cache_hit {
            return;
        }
        let (Some(cache), Some(bytes)) = (cache, &download.complete) else {
            return;
        };
        let Ok(key) = ArchiveCacheKey::new(
            download.offer.pack_id_version.pack_uuid,
            &download
                .offer
                .pack_id_version
                .pack_version
                .version
                .to_string_lossy(),
            download.offer.pack_size.0,
        ) else {
            return;
        };
        if bytes.len() as u64 != download.offer.pack_size.0 {
            return;
        }
        let (cache, bytes, cancelled, slot) = (
            Arc::clone(cache),
            Arc::clone(bytes),
            Arc::clone(cancelled),
            Arc::clone(&self.slot),
        );
        self.jobs.spawn(async move {
            let Ok(_permit) = slot.acquire_owned().await else {
                return;
            };
            if !matches!(
                tokio::task::spawn_blocking(move || cache.store(&key, &bytes, &cancelled)).await,
                Ok(Ok(()))
            ) {
                tracing::warn!("native pack cache write failed");
            }
        });
    }

    /// Waits until all workers have dropped their shared archive references.
    pub(super) async fn finish(&mut self) {
        while self.jobs.join_next().await.is_some() {}
    }
}

/// Produces a bounded failure without exposing signed URLs or response content.
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
