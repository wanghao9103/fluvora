//! Durable media object storage backed by a local filesystem or an S3-compatible service.

use std::env;
use std::ops::Range;
use std::path::{Path as FilePath, PathBuf};
use std::sync::Arc;

use bytes::{Bytes, BytesMut};
use futures_util::{StreamExt as _, TryStreamExt as _};
use object_store::aws::AmazonS3Builder;
use object_store::local::LocalFileSystem;
use object_store::path::Path;
use object_store::{ObjectMeta, ObjectStore, ObjectStoreExt as _, WriteMultipart};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tokio::io::AsyncReadExt as _;

const MULTIPART_CHUNK_BYTES: usize = 8 * 1_024 * 1_024;
const MAX_OBJECT_KEY_BYTES: usize = 1_024;

/// Cloneable media storage client.
#[derive(Clone)]
pub struct MediaStore {
    inner: Arc<dyn ObjectStore>,
    prefix: Path,
    backend: &'static str,
}

impl std::fmt::Debug for MediaStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MediaStore")
            .field("backend", &self.backend)
            .field("prefix", &self.prefix)
            .finish_non_exhaustive()
    }
}

/// S3-compatible object store configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S3Config {
    /// Bucket name.
    pub bucket: String,
    /// AWS region or provider-compatible region value.
    pub region: String,
    /// Optional custom S3 endpoint.
    pub endpoint: Option<String>,
    /// Optional explicit access key. If absent, the AWS credential chain is used.
    pub access_key: Option<String>,
    /// Optional explicit secret key. Must be paired with `access_key`.
    pub secret_key: Option<String>,
    /// Optional temporary credential session token.
    pub session_token: Option<String>,
    /// Allows plaintext HTTP endpoints. Intended only for isolated development networks.
    pub allow_http: bool,
    /// Uses virtual-hosted-style requests instead of path-style requests.
    pub virtual_hosted_style: bool,
    /// Namespace prefix within the bucket.
    pub prefix: String,
}

/// Limits applied while publishing a local directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublishLimits {
    /// Maximum number of regular files.
    pub max_objects: usize,
    /// Maximum size of one object.
    pub max_object_bytes: u64,
    /// Maximum aggregate size.
    pub max_total_bytes: u64,
}

impl Default for PublishLimits {
    fn default() -> Self {
        Self {
            max_objects: 100_000,
            max_object_bytes: 512 * 1_024 * 1_024,
            max_total_bytes: 1_024 * 1_024 * 1_024 * 1_024,
        }
    }
}

/// Metadata for a stored object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredObject {
    /// Size in bytes.
    pub size: u64,
    /// Provider-specific entity tag.
    pub e_tag: Option<String>,
    /// Provider-specific version.
    pub version: Option<String>,
}

/// A verified object included in a directory publication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishedObject {
    /// Key relative to the publication prefix.
    pub key: String,
    /// Object size.
    pub size: u64,
    /// Lowercase SHA-256 digest.
    pub sha256: String,
}

/// Result of publishing a directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Publication {
    /// Publication schema version.
    pub schema_version: u16,
    /// Objects sorted by key.
    pub objects: Vec<PublishedObject>,
    /// Aggregate object size.
    pub total_bytes: u64,
}

/// Result of incrementally synchronizing a live HLS/CMAF directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncReport {
    /// Objects uploaded or overwritten.
    pub uploaded_objects: u64,
    /// Immutable objects already present with the expected size.
    pub skipped_objects: u64,
    /// Aggregate size of eligible local objects.
    pub total_bytes: u64,
}

/// Media storage failure.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// Invalid configuration.
    #[error("invalid media store configuration: {0}")]
    Configuration(String),
    /// Invalid object key.
    #[error("invalid media object key: {0}")]
    InvalidKey(String),
    /// Local filesystem operation failed.
    #[error("media store local I/O failed for {path}: {source}")]
    Io {
        /// Filesystem path.
        path: PathBuf,
        /// Underlying error.
        source: std::io::Error,
    },
    /// Object store operation failed.
    #[error("media store {operation} failed for {key}: {source}")]
    Object {
        /// Operation name.
        operation: &'static str,
        /// Public-safe object key.
        key: String,
        /// Underlying provider error.
        source: object_store::Error,
    },
    /// A publication exceeded a configured limit.
    #[error("media publication limit exceeded: {0}")]
    Limit(String),
    /// Failed to serialize the completion marker.
    #[error("media publication marker serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    /// A write completed with unexpected metadata.
    #[error("media object verification failed for {key}: expected {expected} bytes, got {actual}")]
    Verification {
        /// Object key.
        key: String,
        /// Expected byte size.
        expected: u64,
        /// Actual byte size.
        actual: u64,
    },
}

impl StoreError {
    /// Returns whether this error represents a missing object.
    #[must_use]
    pub const fn is_not_found(&self) -> bool {
        matches!(
            self,
            Self::Object {
                source: object_store::Error::NotFound { .. },
                ..
            }
        )
    }
}

impl MediaStore {
    /// Creates a store from `FLUVORA_OBJECT_STORE_*` environment variables.
    ///
    /// A configured bucket selects S3; otherwise `local_root` is used.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid environment values or an unusable backend.
    pub fn from_env(local_root: impl AsRef<FilePath>) -> Result<Self, StoreError> {
        let prefix =
            optional_env("FLUVORA_OBJECT_STORE_PREFIX").unwrap_or_else(|| "fluvora".to_owned());
        let Some(bucket) = optional_env("FLUVORA_OBJECT_STORE_BUCKET") else {
            return Self::local(local_root, prefix);
        };
        let access_key = optional_env("FLUVORA_OBJECT_STORE_ACCESS_KEY");
        let secret_key = optional_env("FLUVORA_OBJECT_STORE_SECRET_KEY");
        if access_key.is_some() != secret_key.is_some() {
            return Err(StoreError::Configuration(
                "FLUVORA_OBJECT_STORE_ACCESS_KEY and FLUVORA_OBJECT_STORE_SECRET_KEY must be set together"
                    .to_owned(),
            ));
        }
        let allow_http = boolean_env("FLUVORA_OBJECT_STORE_ALLOW_HTTP", false)?;
        let virtual_hosted_style = boolean_env("FLUVORA_OBJECT_STORE_VIRTUAL_HOSTED_STYLE", false)?;
        Self::s3(S3Config {
            bucket,
            region: optional_env("FLUVORA_OBJECT_STORE_REGION")
                .unwrap_or_else(|| "us-east-1".to_owned()),
            endpoint: optional_env("FLUVORA_OBJECT_STORE_ENDPOINT"),
            access_key,
            secret_key,
            session_token: optional_env("FLUVORA_OBJECT_STORE_SESSION_TOKEN"),
            allow_http,
            virtual_hosted_style,
            prefix,
        })
    }

    /// Creates a durable local filesystem store.
    ///
    /// # Errors
    ///
    /// Returns an error if the root cannot be created/canonicalized or the prefix is invalid.
    pub fn local(root: impl AsRef<FilePath>, prefix: impl AsRef<str>) -> Result<Self, StoreError> {
        let root = root.as_ref();
        std::fs::create_dir_all(root).map_err(|source| StoreError::Io {
            path: root.to_path_buf(),
            source,
        })?;
        let store = LocalFileSystem::new_with_prefix(root)
            .map_err(|source| object_error("initialize", root.display().to_string(), source))?
            .with_automatic_cleanup(true)
            .with_fsync(true);
        Ok(Self {
            inner: Arc::new(store),
            prefix: parse_prefix(prefix.as_ref())?,
            backend: "local",
        })
    }

    /// Creates an S3-compatible store.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid credentials, key prefix, endpoint, or builder settings.
    pub fn s3(config: S3Config) -> Result<Self, StoreError> {
        if config.bucket.trim().is_empty() {
            return Err(StoreError::Configuration(
                "object store bucket cannot be empty".to_owned(),
            ));
        }
        if config.access_key.is_some() != config.secret_key.is_some() {
            return Err(StoreError::Configuration(
                "S3 access and secret keys must be configured together".to_owned(),
            ));
        }
        let prefix = parse_prefix(&config.prefix)?;
        let mut builder = AmazonS3Builder::from_env()
            .with_bucket_name(config.bucket)
            .with_region(config.region)
            .with_allow_http(config.allow_http)
            .with_virtual_hosted_style_request(config.virtual_hosted_style);
        if let Some(endpoint) = config.endpoint {
            if endpoint.starts_with("http://") && !config.allow_http {
                return Err(StoreError::Configuration(
                    "plaintext object store endpoint requires FLUVORA_OBJECT_STORE_ALLOW_HTTP=true"
                        .to_owned(),
                ));
            }
            builder = builder.with_endpoint(endpoint);
        }
        if let (Some(access_key), Some(secret_key)) = (config.access_key, config.secret_key) {
            builder = builder
                .with_access_key_id(access_key)
                .with_secret_access_key(secret_key);
        }
        if let Some(token) = config.session_token {
            builder = builder.with_token(token);
        }
        let store = builder
            .build()
            .map_err(|source| object_error("initialize", "<s3-bucket>".to_owned(), source))?;
        Ok(Self {
            inner: Arc::new(store),
            prefix,
            backend: "s3",
        })
    }

    /// Returns the selected backend name.
    #[must_use]
    pub const fn backend(&self) -> &'static str {
        self.backend
    }

    /// Writes an object atomically and verifies the stored size.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid key, backend failure, or size mismatch.
    pub async fn put(&self, key: &str, bytes: Bytes) -> Result<StoredObject, StoreError> {
        let location = self.location(key)?;
        let expected = u64::try_from(bytes.len())
            .map_err(|_| StoreError::Limit("object size exceeds u64".to_owned()))?;
        self.inner
            .put(&location, bytes.into())
            .await
            .map_err(|source| object_error("put", key.to_owned(), source))?;
        self.verify(key, expected).await
    }

    /// Streams a local file through multipart upload and verifies the stored size.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid paths, oversized files, I/O, upload, or verification failures.
    pub async fn put_file(
        &self,
        key: &str,
        file_path: impl AsRef<FilePath>,
        max_bytes: u64,
    ) -> Result<PublishedObject, StoreError> {
        let file_path = file_path.as_ref();
        let metadata = tokio::fs::symlink_metadata(file_path)
            .await
            .map_err(|source| StoreError::Io {
                path: file_path.to_path_buf(),
                source,
            })?;
        if !metadata.is_file() {
            return Err(StoreError::Limit(format!(
                "{} is not a regular file",
                file_path.display()
            )));
        }
        if metadata.len() > max_bytes {
            return Err(StoreError::Limit(format!(
                "{} is {} bytes, above the {max_bytes}-byte object limit",
                file_path.display(),
                metadata.len()
            )));
        }
        let location = self.location(key)?;
        let mut hasher = Sha256::new();
        if metadata.len() == 0 {
            self.inner
                .put(&location, Bytes::new().into())
                .await
                .map_err(|source| object_error("put", key.to_owned(), source))?;
        } else {
            let upload = self
                .inner
                .put_multipart(&location)
                .await
                .map_err(|source| object_error("start multipart upload", key.to_owned(), source))?;
            let mut writer = WriteMultipart::new_with_chunk_size(upload, MULTIPART_CHUNK_BYTES);
            let mut file =
                tokio::fs::File::open(file_path)
                    .await
                    .map_err(|source| StoreError::Io {
                        path: file_path.to_path_buf(),
                        source,
                    })?;
            let mut buffer = BytesMut::zeroed(MULTIPART_CHUNK_BYTES);
            loop {
                let count = match file.read(&mut buffer).await {
                    Ok(count) => count,
                    Err(source) => {
                        let _ = writer.abort().await;
                        return Err(StoreError::Io {
                            path: file_path.to_path_buf(),
                            source,
                        });
                    }
                };
                if count == 0 {
                    break;
                }
                hasher.update(&buffer[..count]);
                writer.put(buffer.split_to(count).freeze());
                writer
                    .wait_for_capacity(4)
                    .await
                    .map_err(|source| object_error("upload part", key.to_owned(), source))?;
                buffer.resize(MULTIPART_CHUNK_BYTES, 0);
            }
            writer.finish().await.map_err(|source| {
                object_error("complete multipart upload", key.to_owned(), source)
            })?;
        }
        self.verify(key, metadata.len()).await?;
        Ok(PublishedObject {
            key: key.to_owned(),
            size: metadata.len(),
            sha256: hex_digest(hasher.finalize().as_slice()),
        })
    }

    /// Publishes every regular file in a directory and writes a completion marker last.
    ///
    /// Symbolic links and non-regular entries are rejected. The returned keys are relative to
    /// `key_prefix`; the marker is stored as `_fluvora.complete.json`.
    ///
    /// # Errors
    ///
    /// Returns an error when traversal, limits, upload, or verification fails.
    pub async fn publish_directory(
        &self,
        local_directory: impl AsRef<FilePath>,
        key_prefix: &str,
        limits: PublishLimits,
    ) -> Result<Publication, StoreError> {
        let local_directory = local_directory.as_ref();
        let mut pending = collect_files(local_directory)?;
        pending.sort_unstable_by(|left, right| left.0.cmp(&right.0));
        if pending.len() > limits.max_objects {
            return Err(StoreError::Limit(format!(
                "directory contains {} objects, above limit {}",
                pending.len(),
                limits.max_objects
            )));
        }
        let mut publication = Publication {
            schema_version: 1,
            objects: Vec::with_capacity(pending.len()),
            total_bytes: 0,
        };
        for (relative_key, file_path) in pending {
            let key = join_key(key_prefix, &relative_key)?;
            let mut object = self
                .put_file(&key, file_path, limits.max_object_bytes)
                .await?;
            object.key = relative_key;
            publication.total_bytes = publication
                .total_bytes
                .checked_add(object.size)
                .ok_or_else(|| StoreError::Limit("publication size overflow".to_owned()))?;
            if publication.total_bytes > limits.max_total_bytes {
                return Err(StoreError::Limit(format!(
                    "directory exceeds {}-byte publication limit",
                    limits.max_total_bytes
                )));
            }
            publication.objects.push(object);
        }
        let marker = serde_json::to_vec(&publication)?;
        let marker_key = join_key(key_prefix, "_fluvora.complete.json")?;
        self.put(&marker_key, Bytes::from(marker)).await?;
        Ok(publication)
    }

    /// Incrementally synchronizes HLS/CMAF output.
    ///
    /// Immutable media objects with a matching remote size are skipped. Playlists are always
    /// uploaded after media objects, so newly referenced segments are visible first.
    ///
    /// # Errors
    ///
    /// Returns an error when traversal, limits, upload, or verification fails.
    pub async fn sync_hls_directory(
        &self,
        local_directory: impl AsRef<FilePath>,
        key_prefix: &str,
        limits: PublishLimits,
    ) -> Result<SyncReport, StoreError> {
        let mut pending = collect_files(local_directory.as_ref())?
            .into_iter()
            .filter(|(key, _)| is_hls_object(key))
            .collect::<Vec<_>>();
        pending.sort_unstable_by(|left, right| {
            is_mutable_hls_object(&left.0)
                .cmp(&is_mutable_hls_object(&right.0))
                .then_with(|| left.0.cmp(&right.0))
        });
        if pending.len() > limits.max_objects {
            return Err(StoreError::Limit(format!(
                "HLS directory contains {} objects, above limit {}",
                pending.len(),
                limits.max_objects
            )));
        }
        let mut report = SyncReport {
            uploaded_objects: 0,
            skipped_objects: 0,
            total_bytes: 0,
        };
        for (relative_key, file_path) in pending {
            let metadata = tokio::fs::symlink_metadata(&file_path)
                .await
                .map_err(|source| StoreError::Io {
                    path: file_path.clone(),
                    source,
                })?;
            if metadata.len() > limits.max_object_bytes {
                return Err(StoreError::Limit(format!(
                    "{} is {} bytes, above the {}-byte object limit",
                    file_path.display(),
                    metadata.len(),
                    limits.max_object_bytes
                )));
            }
            report.total_bytes = report
                .total_bytes
                .checked_add(metadata.len())
                .ok_or_else(|| StoreError::Limit("HLS directory size overflow".to_owned()))?;
            if report.total_bytes > limits.max_total_bytes {
                return Err(StoreError::Limit(format!(
                    "HLS directory exceeds {}-byte publication limit",
                    limits.max_total_bytes
                )));
            }
            let key = join_key(key_prefix, &relative_key)?;
            let needs_upload = if is_mutable_hls_object(&relative_key) {
                true
            } else {
                match self.head(&key).await {
                    Ok(remote) => remote.size != metadata.len(),
                    Err(error) if error.is_not_found() => true,
                    Err(error) => return Err(error),
                }
            };
            if needs_upload {
                self.put_file(&key, file_path, limits.max_object_bytes)
                    .await?;
                report.uploaded_objects = report.uploaded_objects.saturating_add(1);
            } else {
                report.skipped_objects = report.skipped_objects.saturating_add(1);
            }
        }
        Ok(report)
    }

    /// Retrieves an entire object.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid keys or backend failures.
    pub async fn get(&self, key: &str) -> Result<Bytes, StoreError> {
        let location = self.location(key)?;
        self.inner
            .get(&location)
            .await
            .map_err(|source| object_error("get", key.to_owned(), source))?
            .bytes()
            .await
            .map_err(|source| object_error("read", key.to_owned(), source))
    }

    /// Retrieves a half-open byte range.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid keys/ranges or backend failures.
    pub async fn get_range(&self, key: &str, range: Range<u64>) -> Result<Bytes, StoreError> {
        if range.start >= range.end {
            return Err(StoreError::InvalidKey(
                "byte range must be non-empty".to_owned(),
            ));
        }
        let location = self.location(key)?;
        self.inner
            .get_range(&location, range)
            .await
            .map_err(|source| object_error("get range", key.to_owned(), source))
    }

    /// Returns object metadata.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid keys or backend failures.
    pub async fn head(&self, key: &str) -> Result<StoredObject, StoreError> {
        let location = self.location(key)?;
        let meta = self
            .inner
            .head(&location)
            .await
            .map_err(|source| object_error("head", key.to_owned(), source))?;
        Ok(stored_object(&meta))
    }

    /// Deletes one object. Missing objects are treated as success.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid keys or non-not-found backend failures.
    pub async fn delete(&self, key: &str) -> Result<(), StoreError> {
        let location = self.location(key)?;
        match self.inner.delete(&location).await {
            Ok(()) | Err(object_store::Error::NotFound { .. }) => Ok(()),
            Err(source) => Err(object_error("delete", key.to_owned(), source)),
        }
    }

    /// Deletes all objects below a path-segment prefix.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid prefixes, listing, or deletion failures.
    pub async fn delete_prefix(&self, prefix: &str) -> Result<u64, StoreError> {
        let location = self.location(prefix)?;
        let locations = self
            .inner
            .list(Some(&location))
            .map_ok(|meta| meta.location)
            .try_collect::<Vec<_>>()
            .await
            .map_err(|source| object_error("list", prefix.to_owned(), source))?;
        let count = u64::try_from(locations.len())
            .map_err(|_| StoreError::Limit("object count exceeds u64".to_owned()))?;
        let results = self
            .inner
            .delete_stream(futures_util::stream::iter(locations.into_iter().map(Ok)).boxed())
            .try_collect::<Vec<_>>()
            .await;
        match results {
            Ok(_) | Err(object_store::Error::NotFound { .. }) => Ok(count),
            Err(source) => Err(object_error("delete prefix", prefix.to_owned(), source)),
        }
    }

    /// Verifies write/read access using a small durable sentinel.
    ///
    /// # Errors
    ///
    /// Returns an error when the backend cannot write or read metadata.
    pub async fn healthcheck(&self) -> Result<(), StoreError> {
        const SENTINEL_KEY: &str = "_system/healthcheck-v1";
        const SENTINEL: &[u8] = b"fluvora-media-store-v1";
        self.put(SENTINEL_KEY, Bytes::from_static(SENTINEL)).await?;
        let meta = self.head(SENTINEL_KEY).await?;
        if meta.size != u64::try_from(SENTINEL.len()).unwrap_or(u64::MAX) {
            return Err(StoreError::Verification {
                key: SENTINEL_KEY.to_owned(),
                expected: u64::try_from(SENTINEL.len()).unwrap_or(u64::MAX),
                actual: meta.size,
            });
        }
        Ok(())
    }

    async fn verify(&self, key: &str, expected: u64) -> Result<StoredObject, StoreError> {
        let actual = self.head(key).await?;
        if actual.size != expected {
            return Err(StoreError::Verification {
                key: key.to_owned(),
                expected,
                actual: actual.size,
            });
        }
        Ok(actual)
    }

    fn location(&self, key: &str) -> Result<Path, StoreError> {
        let key = parse_key(key)?;
        if self.prefix.is_root() {
            return Ok(key);
        }
        Path::parse(format!("{}/{}", self.prefix, key))
            .map_err(|error| StoreError::InvalidKey(error.to_string()))
    }
}

fn optional_env(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn boolean_env(name: &str, default: bool) -> Result<bool, StoreError> {
    let Some(value) = optional_env(name) else {
        return Ok(default);
    };
    match value.as_str() {
        "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        _ => Err(StoreError::Configuration(format!(
            "{name} must be true, false, 1, or 0"
        ))),
    }
}

fn parse_prefix(value: &str) -> Result<Path, StoreError> {
    if value.len() > MAX_OBJECT_KEY_BYTES {
        return Err(StoreError::InvalidKey(
            "object store prefix exceeds 1024 bytes".to_owned(),
        ));
    }
    Path::parse(value).map_err(|error| StoreError::InvalidKey(error.to_string()))
}

fn parse_key(value: &str) -> Result<Path, StoreError> {
    if value.is_empty() || value.len() > MAX_OBJECT_KEY_BYTES {
        return Err(StoreError::InvalidKey(
            "object key must contain 1..=1024 bytes".to_owned(),
        ));
    }
    Path::parse(value).map_err(|error| StoreError::InvalidKey(error.to_string()))
}

fn join_key(prefix: &str, key: &str) -> Result<String, StoreError> {
    let value = format!("{}/{}", prefix.trim_matches('/'), key.trim_matches('/'));
    parse_key(&value)?;
    Ok(value)
}

fn is_hls_object(key: &str) -> bool {
    matches!(
        FilePath::new(key)
            .extension()
            .and_then(|value| value.to_str()),
        Some("m3u8" | "m4s" | "mp4" | "aac" | "vtt")
    )
}

fn is_mutable_hls_object(key: &str) -> bool {
    FilePath::new(key)
        .extension()
        .and_then(|value| value.to_str())
        == Some("m3u8")
}

fn collect_files(root: &FilePath) -> Result<Vec<(String, PathBuf)>, StoreError> {
    let root_metadata = std::fs::symlink_metadata(root).map_err(|source| StoreError::Io {
        path: root.to_path_buf(),
        source,
    })?;
    if !root_metadata.is_dir() {
        return Err(StoreError::Limit(format!(
            "{} is not a directory",
            root.display()
        )));
    }
    let mut directories = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(directory) = directories.pop() {
        let entries = std::fs::read_dir(&directory).map_err(|source| StoreError::Io {
            path: directory.clone(),
            source,
        })?;
        for entry in entries {
            let entry = entry.map_err(|source| StoreError::Io {
                path: directory.clone(),
                source,
            })?;
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path).map_err(|source| StoreError::Io {
                path: path.clone(),
                source,
            })?;
            if metadata.file_type().is_symlink() {
                return Err(StoreError::Limit(format!(
                    "symbolic links are not allowed in publications: {}",
                    path.display()
                )));
            }
            if metadata.is_dir() {
                directories.push(path);
            } else if metadata.is_file() {
                let relative = path.strip_prefix(root).map_err(|_| {
                    StoreError::InvalidKey(format!(
                        "{} is outside publication root",
                        path.display()
                    ))
                })?;
                let relative = relative
                    .components()
                    .map(|component| {
                        component.as_os_str().to_str().ok_or_else(|| {
                            StoreError::InvalidKey(format!(
                                "{} is not valid UTF-8",
                                relative.display()
                            ))
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?
                    .join("/");
                parse_key(&relative)?;
                files.push((relative, path));
            } else {
                return Err(StoreError::Limit(format!(
                    "unsupported filesystem entry in publication: {}",
                    path.display()
                )));
            }
        }
    }
    Ok(files)
}

fn stored_object(meta: &ObjectMeta) -> StoredObject {
    StoredObject {
        size: meta.size,
        e_tag: meta.e_tag.clone(),
        version: meta.version.clone(),
    }
}

fn object_error(operation: &'static str, key: String, source: object_store::Error) -> StoreError {
    StoreError::Object {
        operation,
        key,
        source,
    }
}

fn hex_digest(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use tempfile::tempdir;

    use super::{MediaStore, PublishLimits, StoreError};

    #[tokio::test]
    async fn local_store_put_range_publish_and_delete() {
        let root = tempdir().expect("storage root");
        let source = tempdir().expect("source root");
        std::fs::create_dir_all(source.path().join("nested")).expect("nested");
        std::fs::write(source.path().join("master.m3u8"), b"#EXTM3U\n").expect("manifest");
        std::fs::write(
            source.path().join("nested").join("segment.m4s"),
            b"abcdefghij",
        )
        .expect("segment");
        let store = MediaStore::local(root.path(), "tenant/media").expect("store");

        store
            .put("manual/init.mp4", Bytes::from_static(b"0123456789"))
            .await
            .expect("put");
        let range = store
            .get_range("manual/init.mp4", 2..6)
            .await
            .expect("range");
        assert_eq!(range, Bytes::from_static(b"2345"));

        let publication = store
            .publish_directory(source.path(), "vod/asset-1", PublishLimits::default())
            .await
            .expect("publish");
        assert_eq!(publication.objects.len(), 2);
        assert_eq!(publication.total_bytes, 18);
        assert_eq!(
            store
                .get("vod/asset-1/nested/segment.m4s")
                .await
                .expect("published"),
            Bytes::from_static(b"abcdefghij")
        );
        assert!(
            store
                .head("vod/asset-1/_fluvora.complete.json")
                .await
                .is_ok()
        );
        let first_sync = store
            .sync_hls_directory(source.path(), "live/stream-1", PublishLimits::default())
            .await
            .expect("first live sync");
        assert_eq!(first_sync.uploaded_objects, 2);
        let second_sync = store
            .sync_hls_directory(source.path(), "live/stream-1", PublishLimits::default())
            .await
            .expect("second live sync");
        assert_eq!(second_sync.uploaded_objects, 1);
        assert_eq!(second_sync.skipped_objects, 1);

        assert_eq!(store.delete_prefix("vod/asset-1").await.expect("delete"), 3);
        let error = store
            .head("vod/asset-1/master.m3u8")
            .await
            .expect_err("deleted");
        assert!(error.is_not_found());
    }

    #[tokio::test]
    async fn rejects_invalid_keys_limits_and_symlinks() {
        let root = tempdir().expect("storage root");
        let source = tempdir().expect("source root");
        std::fs::write(source.path().join("large.bin"), b"oversize").expect("large");
        let store = MediaStore::local(root.path(), "").expect("store");
        assert!(matches!(
            store.put("../escape", Bytes::from_static(b"x")).await,
            Err(StoreError::InvalidKey(_))
        ));
        let limits = PublishLimits {
            max_objects: 1,
            max_object_bytes: 2,
            max_total_bytes: 2,
        };
        assert!(matches!(
            store
                .publish_directory(source.path(), "vod/a", limits)
                .await,
            Err(StoreError::Limit(_))
        ));
    }
}
