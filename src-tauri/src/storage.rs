//! S3-compatible storage adapter. Every provider preset (AWS S3, Cloudflare
//! R2, MinIO, Backblaze B2, DigitalOcean Spaces, custom) goes through this
//! single adapter, never a separate upload engine per provider.

use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use aws_credential_types::Credentials;
use aws_sdk_s3::config::{BehaviorVersion, Region, RequestChecksumCalculation, ResponseChecksumValidation};
use aws_sdk_s3::error::{DisplayErrorContext, ProvideErrorMetadata, SdkError};
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::Client;
use aws_smithy_runtime_api::client::http::SharedHttpClient;
use aws_smithy_runtime_api::client::orchestrator::HttpResponse;
use aws_smithy_http_client::tls::{self, rustls_provider::CryptoMode};
use aws_smithy_types::timeout::TimeoutConfig;
use serde::Serialize;

use crate::credentials::StorageCredentials;
use crate::destinations::DestinationConfig;
use crate::output::{encode_key_path, resolve_public_url};
use crate::t;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionResult {
    pub bucket_reachable: bool,
    pub writable: bool,
    pub public_url_reachable: Option<bool>,
}

/// One level of a bucket as S3 lists it with a "/" delimiter: the
/// "folders" (common prefixes) and objects directly under `prefix`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BucketListing {
    pub prefix: String,
    pub folders: Vec<String>,
    pub objects: Vec<BucketObject>,
    pub next_continuation_token: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BucketObject {
    pub key: String,
    pub size: i64,
    /// Unix milliseconds.
    pub last_modified: Option<i64>,
}

impl BucketObject {
    pub fn name(&self) -> &str {
        self.key.rsplit('/').next().unwrap_or(&self.key)
    }
}

#[derive(Debug, Clone)]
pub struct UploadResult {
    pub object_key: String,
    pub public_url: String,
    pub byte_size: i64,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum StorageError {
    #[error("{}", t!("Could not authenticate. Check your Access Key ID and Secret Access Key."))]
    InvalidCredentials,
    #[error("{}", t!("Bucket “{0}” could not be found.", .0))]
    BucketNotFound(String),
    #[error("{}", t!("Connected successfully, but this key cannot upload files."))]
    AccessDenied,
    #[error("{}", t!("Upload interrupted. {0}", .0))]
    Network(String),
    #[error("{0}")]
    Unknown(String),
}

/// One HTTPS client for every destination, so connections are pooled.
fn http_client() -> SharedHttpClient {
    static CLIENT: OnceLock<SharedHttpClient> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            aws_smithy_http_client::Builder::new()
                .tls_provider(tls::Provider::Rustls(CryptoMode::Ring))
                .build_https()
        })
        .clone()
}

pub struct S3Provider {
    config: DestinationConfig,
    client: Client,
}

impl S3Provider {
    pub fn new(config: DestinationConfig, credentials: StorageCredentials) -> Self {
        let credentials = Credentials::new(
            credentials.access_key_id,
            credentials.secret_access_key,
            credentials.session_token,
            None,
            "aktar",
        );
        let region = match config.region.trim() {
            "" => "us-east-1".to_string(),
            region => region.to_string(),
        };
        let s3_config = aws_sdk_s3::Config::builder()
            .behavior_version(BehaviorVersion::latest())
            .http_client(http_client())
            .region(Region::new(region))
            .endpoint_url(normalized_endpoint(&config.endpoint))
            // Path-style for providers that need it (MinIO), virtual-hosted
            // otherwise, same as the Mac app.
            .force_path_style(config.force_path_style)
            .credentials_provider(credentials)
            // Newer SDKs add CRC checksums (and aws-chunked bodies) to every
            // upload by default, which several S3-compatible providers
            // reject. Only send them when an operation requires one.
            .request_checksum_calculation(RequestChecksumCalculation::WhenRequired)
            .response_checksum_validation(ResponseChecksumValidation::WhenRequired)
            .timeout_config(TimeoutConfig::builder().connect_timeout(Duration::from_secs(15)).build())
            .build();
        Self { config, client: Client::from_conf(s3_config) }
    }

    fn bucket(&self) -> &str {
        &self.config.bucket
    }

    pub async fn test_connection(&self) -> Result<ConnectionResult, StorageError> {
        self.client
            .head_bucket()
            .bucket(self.bucket())
            .send()
            .await
            .map_err(|error| map_error(error, self.bucket()))?;

        let test_key = format!("_aktar_test_{}.txt", &uuid::Uuid::new_v4().to_string().to_uppercase()[..8]);
        let writable = self
            .client
            .put_object()
            .bucket(self.bucket())
            .key(&test_key)
            .content_type("text/plain")
            .body(ByteStream::from_static(b"aktar connection test"))
            .send()
            .await
            .is_ok();

        let mut public_url_reachable = None;
        if writable && !self.config.public_base_url.trim().is_empty() {
            let url = resolve_public_url(&self.config.public_base_url, &test_key);
            public_url_reachable = Some(probe(&url).await);
        }
        if writable {
            let _ = self.client.delete_object().bucket(self.bucket()).key(&test_key).send().await;
        }
        Ok(ConnectionResult { bucket_reachable: true, writable, public_url_reachable })
    }

    pub async fn upload(&self, path: &Path, object_key: &str, content_type: &str) -> Result<UploadResult, StorageError> {
        let byte_size = tokio::fs::metadata(path)
            .await
            .map_err(|error| StorageError::Unknown(error.to_string()))?
            .len() as i64;
        let body = ByteStream::from_path(path)
            .await
            .map_err(|error| StorageError::Unknown(error.to_string()))?;
        self.client
            .put_object()
            .bucket(self.bucket())
            .key(object_key)
            .content_type(content_type)
            .body(body)
            .send()
            .await
            .map_err(|error| map_error(error, self.bucket()))?;
        Ok(UploadResult {
            object_key: object_key.to_string(),
            public_url: resolve_public_url(&self.config.public_base_url, object_key),
            byte_size,
        })
    }

    pub async fn delete(&self, object_key: &str) -> Result<(), StorageError> {
        self.client
            .delete_object()
            .bucket(self.bucket())
            .key(object_key)
            .send()
            .await
            .map_err(|error| map_error(error, self.bucket()))?;
        Ok(())
    }

    /// One level under `prefix`: its folders and the objects directly in it.
    pub async fn list(&self, prefix: &str, continuation_token: Option<String>) -> Result<BucketListing, StorageError> {
        self.list_objects(prefix, continuation_token, Some("/")).await
    }

    /// Every key under `prefix`, however deep, 1000 per page. Used for
    /// search, since S3 has no server-side search. `folders` holds the
    /// "name/" placeholders of otherwise empty folders.
    pub async fn list_recursively(&self, prefix: &str, continuation_token: Option<String>) -> Result<BucketListing, StorageError> {
        self.list_objects(prefix, continuation_token, None).await
    }

    async fn list_objects(
        &self,
        prefix: &str,
        continuation_token: Option<String>,
        delimiter: Option<&str>,
    ) -> Result<BucketListing, StorageError> {
        let output = self
            .client
            .list_objects_v2()
            .bucket(self.bucket())
            .set_continuation_token(continuation_token)
            .set_delimiter(delimiter.map(str::to_string))
            .max_keys(1000)
            .set_prefix((!prefix.is_empty()).then(|| prefix.to_string()))
            .send()
            .await
            .map_err(|error| map_error(error, self.bucket()))?;

        let mut folders: Vec<String> = output
            .common_prefixes()
            .iter()
            .filter_map(|common| common.prefix().map(str::to_string))
            .collect();
        let mut objects = Vec::new();
        for object in output.contents() {
            let Some(key) = object.key() else { continue };
            if key == prefix {
                continue;
            }
            // A zero-byte "folder/" placeholder is a folder, not a file.
            if key.ends_with('/') {
                if delimiter.is_none() {
                    folders.push(key.to_string());
                }
                continue;
            }
            objects.push(BucketObject {
                key: key.to_string(),
                size: object.size().unwrap_or(0),
                last_modified: object
                    .last_modified()
                    .map(|date| date.secs() * 1000 + i64::from(date.subsec_nanos() / 1_000_000)),
            });
        }
        let next_continuation_token = if output.is_truncated() == Some(true) {
            output.next_continuation_token().map(str::to_string)
        } else {
            None
        };
        Ok(BucketListing { prefix: prefix.to_string(), folders, objects, next_continuation_token })
    }

    /// Asks for the first key starting with `key` instead of sending a HEAD
    /// request, whose bodyless 404 some providers answer in odd ways. Keys
    /// are listed in order, so an exact match always comes first.
    pub async fn object_exists(&self, key: &str) -> Result<bool, StorageError> {
        let output = self
            .client
            .list_objects_v2()
            .bucket(self.bucket())
            .max_keys(1)
            .prefix(key)
            .send()
            .await
            .map_err(|error| map_error(error, self.bucket()))?;
        Ok(output.contents().first().and_then(|object| object.key()) == Some(key))
    }

    /// S3 has no rename or move: both are a server-side copy followed by
    /// deleting the original.
    pub async fn copy(&self, source_key: &str, destination_key: &str) -> Result<(), StorageError> {
        self.client
            .copy_object()
            .bucket(self.bucket())
            .copy_source(format!("{}/{}", self.bucket(), encode_key_path(source_key)))
            .key(destination_key)
            .send()
            .await
            .map_err(|error| map_error(error, self.bucket()))?;
        Ok(())
    }

    /// S3 folders only exist while something is in them; an empty "name/"
    /// object is the conventional way to keep an empty one around.
    pub async fn create_folder(&self, prefix: &str) -> Result<(), StorageError> {
        self.client
            .put_object()
            .bucket(self.bucket())
            .key(prefix)
            .body(ByteStream::from_static(b""))
            .send()
            .await
            .map_err(|error| map_error(error, self.bucket()))?;
        Ok(())
    }

    /// A presigned GET URL, which works for private buckets and doesn't
    /// depend on the public base URL being set up correctly.
    pub async fn temporary_url(&self, object_key: &str, expires_in_seconds: u64) -> Result<String, StorageError> {
        let presigning = PresigningConfig::expires_in(Duration::from_secs(expires_in_seconds))
            .map_err(|error| StorageError::Unknown(error.to_string()))?;
        let request = self
            .client
            .get_object()
            .bucket(self.bucket())
            .key(object_key)
            .presigned(presigning)
            .await
            .map_err(|error| map_error(error, self.bucket()))?;
        Ok(request.uri().to_string())
    }
}

fn normalized_endpoint(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    }
}

async fn probe(url: &str) -> bool {
    let Ok(client) = reqwest::Client::builder().timeout(Duration::from_secs(8)).build() else {
        return false;
    };
    matches!(client.head(url).send().await, Ok(response) if response.status().is_success())
}

fn map_error<E>(error: SdkError<E, HttpResponse>, bucket: &str) -> StorageError
where
    E: ProvideErrorMetadata + std::error::Error + Send + Sync + 'static,
{
    let status = error.raw_response().map(|response| response.status().as_u16());
    let code = error.code().unwrap_or_default().to_ascii_lowercase();
    let description = format!("{code} {}", DisplayErrorContext(&error)).to_ascii_lowercase();

    // HeadBucket's 404 has no body, so it arrives as a bare "NotFound".
    if code == "nosuchbucket" || code == "notfound" || description.contains("nosuchbucket") || (status == Some(404) && code.is_empty()) {
        return StorageError::BucketNotFound(bucket.to_string());
    }
    if description.contains("invalidaccesskeyid")
        || description.contains("signaturedoesnotmatch")
        || description.contains("unauthorized")
        || status == Some(401)
    {
        return StorageError::InvalidCredentials;
    }
    if description.contains("accessdenied") || description.contains("forbidden") || status == Some(403) {
        return StorageError::AccessDenied;
    }
    match &error {
        SdkError::DispatchFailure(_) | SdkError::TimeoutError(_) => StorageError::Network(readable(&error)),
        _ => StorageError::Unknown(error.message().map(str::to_string).unwrap_or_else(|| readable(&error))),
    }
}

/// The innermost cause is usually the useful part ("connection refused",
/// "dns error: ..."), not the SDK's "dispatch failure" wrapper.
fn readable<E: std::error::Error + 'static, R: std::fmt::Debug>(error: &SdkError<E, R>) -> String {
    let mut source: &dyn std::error::Error = error;
    while let Some(next) = source.source() {
        source = next;
    }
    source.to_string()
}

/// Runs against a real S3-compatible server, e.g. `moto_server -p 9100`
/// with a bucket named "aktar-test":
///
/// ```text
/// AKTAR_TEST_S3_ENDPOINT=http://127.0.0.1:9100 cargo test storage -- --ignored
/// ```
#[cfg(test)]
mod live_tests {
    use super::*;
    use crate::destinations::ProviderPreset;

    fn provider(bucket: &str, secret: &str) -> Option<S3Provider> {
        let endpoint = std::env::var("AKTAR_TEST_S3_ENDPOINT").ok()?;
        let config = DestinationConfig {
            id: "TEST".into(),
            name: "Test".into(),
            preset: ProviderPreset::MinIO,
            account_id: None,
            endpoint: endpoint.clone(),
            region: "us-east-1".into(),
            bucket: bucket.into(),
            public_base_url: format!("{endpoint}/{bucket}"),
            object_path_template: "{filename}.{ext}".into(),
            force_path_style: true,
            is_default: true,
        };
        let credentials = StorageCredentials {
            access_key_id: "AKIATEST".into(),
            secret_access_key: secret.into(),
            session_token: None,
        };
        Some(S3Provider::new(config, credentials))
    }

    #[tokio::test]
    #[ignore]
    async fn round_trip() {
        let Some(storage) = provider("aktar-test", "secret") else { return };

        let result = storage.test_connection().await.unwrap();
        assert!(result.writable);
        // Whether it's reachable depends on the bucket's policy; it's probed.
        assert!(result.public_url_reachable.is_some());

        let file = std::env::temp_dir().join("aktar test ü.txt");
        std::fs::write(&file, b"hello from aktar").unwrap();
        let uploaded = storage.upload(&file, "live/aktar test ü.txt", "text/plain").await.unwrap();
        assert_eq!(uploaded.byte_size, 16);
        assert!(uploaded.public_url.ends_with("/live/aktar%20test%20%C3%BC.txt"));

        assert!(storage.object_exists("live/aktar test ü.txt").await.unwrap());
        assert!(!storage.object_exists("live/aktar test").await.unwrap());

        storage.create_folder("live/empty/").await.unwrap();
        let level = storage.list("live/", None).await.unwrap();
        assert!(level.folders.contains(&"live/empty/".to_string()));
        assert!(level.objects.iter().any(|object| object.key == "live/aktar test ü.txt"));
        let deep = storage.list_recursively("", None).await.unwrap();
        assert!(deep.folders.contains(&"live/empty/".to_string()));

        let key = crate::bucket::available_key(&storage, "aktar test ü.txt", "live/", &[]).await.unwrap();
        assert_eq!(key, "live/aktar test ü 2.txt");

        assert!(crate::bucket::move_object(&storage, "live/aktar test ü.txt", "moved/renamed.txt").await.is_ok());
        assert!(!storage.object_exists("live/aktar test ü.txt").await.unwrap());
        let signed = storage.temporary_url("moved/renamed.txt", 600).await.unwrap();
        assert!(signed.contains("X-Amz-Signature="));
        let body = reqwest::get(&signed).await.unwrap().text().await.unwrap();
        assert_eq!(body, "hello from aktar");

        storage.delete("moved/renamed.txt").await.unwrap();
        storage.delete("live/empty/").await.unwrap();
        assert!(!storage.object_exists("moved/renamed.txt").await.unwrap());
    }

    #[tokio::test]
    #[ignore]
    async fn reports_a_missing_bucket() {
        let Some(storage) = provider("no-such-bucket-aktar", "secret") else { return };
        match storage.test_connection().await {
            Err(StorageError::BucketNotFound(bucket)) => assert_eq!(bucket, "no-such-bucket-aktar"),
            other => panic!("unexpected {other:?}"),
        }
    }
}
