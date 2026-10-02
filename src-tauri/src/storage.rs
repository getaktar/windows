//! S3-compatible storage adapter. Every provider preset (AWS S3, Cloudflare
//! R2, MinIO, Backblaze B2, DigitalOcean Spaces, custom) goes through this
//! single adapter, never a separate upload engine per provider.

use std::path::Path;
use std::pin::Pin;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll};
use std::time::Duration;

use aws_credential_types::Credentials;
use aws_sigv4::http_request::{
    sign, PayloadChecksumKind, PercentEncodingMode, SignableBody, SignableRequest, SigningSettings, UriPathNormalizationMode,
};
use aws_sigv4::sign::v4;
use aws_sdk_s3::config::retry::RetryConfig;
use aws_sdk_s3::config::{BehaviorVersion, Region, RequestChecksumCalculation, ResponseChecksumValidation};
use aws_sdk_s3::error::{DisplayErrorContext, ProvideErrorMetadata, SdkError};
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::primitives::{ByteStream, Length};
use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart};
use aws_sdk_s3::Client;
use aws_smithy_runtime_api::client::http::SharedHttpClient;
use aws_smithy_runtime_api::client::orchestrator::HttpResponse;
use aws_smithy_http_client::tls::{self, rustls_provider::CryptoMode};
use aws_smithy_types::body::SdkBody;
use aws_smithy_types::timeout::TimeoutConfig;
use serde::{Deserialize, Serialize};

use crate::credentials::StorageCredentials;
use crate::destinations::DestinationConfig;
use crate::expiry::{Removal, RulesStatus};
use crate::output::{encode_copy_source, resolve_public_url};
use crate::t;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionResult {
    pub bucket_reachable: bool,
    pub writable: bool,
    /// What opening the test file's public link returned. None when
    /// nothing was uploaded to open.
    pub public_link: Option<PublicLinkCheck>,
}

/// The outcome of opening a link the way someone it's shared with would.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum PublicLinkCheck {
    Reachable,
    /// The server answered with a status outside 2xx, such as 403 from a
    /// bucket that accepts uploads but doesn't allow public reads.
    Status { code: u16 },
    /// No HTTP answer at all: DNS, TLS or a timeout.
    NoResponse,
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
    /// A network failure outside an upload (listing, deleting, testing).
    #[error("{}", t!("Could not connect. {0}", .0))]
    Connection(String),
    /// A lifecycle request the key isn't allowed to make.
    #[error("{}", t!("This key can't change the bucket's lifecycle rules."))]
    LifecycleNotAllowed,
    #[error("{}", t!("This provider doesn't support lifecycle rules."))]
    LifecycleUnsupported,
    /// The server broke or was overloaded (5xx, 429): trying again later
    /// may work.
    #[error("{0}")]
    Server(String),
    #[error("{0}")]
    Unknown(String),
}

impl StorageError {
    /// Whether the same request may work later on its own: the network
    /// dropped, or the server had a problem. Watched folders try those
    /// again by themselves.
    pub fn is_transient(&self) -> bool {
        matches!(self, StorageError::Network(_) | StorageError::Connection(_) | StorageError::Server(_))
    }
}

const MIB: u64 = 1024 * 1024;
/// Files up to this size go up in one request; bigger ones in parts.
pub const SINGLE_UPLOAD_LIMIT: u64 = 64 * MIB;
/// How long to wait before each new try of a part that failed on the way.
const PART_RETRY_DELAYS: [u64; 5] = [1, 2, 4, 8, 16];

/// The size of each part of a multipart upload: at least 16 MiB, and big
/// enough (in whole MiB) that S3's 10,000 parts cover the file, with room
/// to spare: 9,000 parts of 5 TB fit.
pub fn part_size(file_size: u64) -> u64 {
    (file_size.div_ceil(9000).div_ceil(MIB) * MIB).max(16 * MIB)
}

/// A part of a multipart upload the server has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadedPart {
    pub number: i32,
    pub etag: String,
    pub size: u64,
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
    /// For the requests signed by hand (lifecycle rules).
    credentials: Credentials,
    region: String,
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
            .region(Region::new(region.clone()))
            .endpoint_url(normalized_endpoint(&config.endpoint))
            // Path-style for providers that need it (MinIO), virtual-hosted
            // otherwise, same as the Mac app.
            .force_path_style(config.force_path_style)
            .credentials_provider(credentials.clone())
            // Newer SDKs add CRC checksums (and aws-chunked bodies) to every
            // upload by default, which several S3-compatible providers
            // reject. Only send them when an operation requires one.
            .request_checksum_calculation(RequestChecksumCalculation::WhenRequired)
            .response_checksum_validation(ResponseChecksumValidation::WhenRequired)
            .timeout_config(TimeoutConfig::builder().connect_timeout(Duration::from_secs(15)).build())
            .build();
        Self { config, client: Client::from_conf(s3_config), credentials, region }
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

        let mut public_link = None;
        if writable && !self.config.public_base_url.trim().is_empty() {
            let url = resolve_public_url(&self.config.public_base_url, &test_key);
            public_link = Some(probe(&url).await);
        }
        if writable {
            let _ = self.client.delete_object().bucket(self.bucket()).key(&test_key).send().await;
        }
        Ok(ConnectionResult { bucket_reachable: true, writable, public_link })
    }

    /// Uploads a file, counting the bytes sent into `progress` as the HTTP
    /// client reads them.
    pub async fn upload(
        &self,
        path: &Path,
        object_key: &str,
        content_type: &str,
        progress: Arc<Progress>,
    ) -> Result<UploadResult, StorageError> {
        let byte_size = tokio::fs::metadata(path)
            .await
            .map_err(|error| StorageError::Unknown(error.to_string()))?
            .len();
        progress.total.store(byte_size, Ordering::Relaxed);
        let file = ByteStream::from_path(path)
            .await
            .map_err(|error| StorageError::Unknown(error.to_string()))?
            .into_inner();
        // Rebuilt from the file for every attempt, so the SDK can still
        // retry, with the count starting over each time.
        let body = SdkBody::retryable(move || {
            progress.sent.store(0, Ordering::Relaxed);
            match file.try_clone() {
                Some(inner) => SdkBody::from_body_1_x(CountingBody { inner, progress: progress.clone(), attempt: None }),
                None => SdkBody::taken(),
            }
        });
        self.client
            .put_object()
            .bucket(self.bucket())
            .key(object_key)
            .content_type(content_type)
            .body(ByteStream::new(body))
            .send()
            .await
            .map_err(|error| upload_error(error, self.bucket()))?;
        Ok(UploadResult {
            object_key: object_key.to_string(),
            public_url: resolve_public_url(&self.config.public_base_url, object_key),
            byte_size: byte_size as i64,
        })
    }

    /// Starts a multipart upload and returns its ID.
    pub async fn create_multipart(&self, object_key: &str, content_type: &str) -> Result<String, StorageError> {
        let output = self
            .client
            .create_multipart_upload()
            .bucket(self.bucket())
            .key(object_key)
            .content_type(content_type)
            .send()
            .await
            .map_err(|error| upload_error(error, self.bucket()))?;
        output.upload_id().map(str::to_string).ok_or_else(|| StorageError::Unknown("No upload ID".into()))
    }

    /// The parts the server has of a multipart upload, or None when it no
    /// longer knows the upload (aborted, or cleaned up by the bucket).
    pub async fn uploaded_parts(&self, object_key: &str, upload_id: &str) -> Result<Option<Vec<UploadedPart>>, StorageError> {
        let mut parts = Vec::new();
        let mut marker: Option<String> = None;
        loop {
            let result = self
                .client
                .list_parts()
                .bucket(self.bucket())
                .key(object_key)
                .upload_id(upload_id)
                .set_part_number_marker(marker.take())
                .send()
                .await;
            let output = match result {
                Ok(output) => output,
                Err(error) if error.code() == Some("NoSuchUpload") || error.raw_response().is_some_and(|r| r.status().as_u16() == 404) => {
                    return Ok(None);
                }
                Err(error) => return Err(upload_error(error, self.bucket())),
            };
            for part in output.parts() {
                if let (Some(number), Some(etag)) = (part.part_number(), part.e_tag()) {
                    parts.push(UploadedPart { number, etag: etag.to_string(), size: part.size().unwrap_or(0).max(0) as u64 });
                }
            }
            match output.next_part_number_marker() {
                Some(next) if output.is_truncated() == Some(true) => marker = Some(next.to_string()),
                _ => break,
            }
        }
        Ok(Some(parts))
    }

    /// What uploads the parts, which can be sent from tasks of their own.
    pub fn part_sender(&self) -> PartSender {
        PartSender { client: self.client.clone(), bucket: self.bucket().to_string() }
    }

    pub async fn complete_multipart(&self, object_key: &str, upload_id: &str, parts: &[UploadedPart]) -> Result<(), StorageError> {
        let mut parts = parts.to_vec();
        parts.sort_by_key(|part| part.number);
        let completed = CompletedMultipartUpload::builder()
            .set_parts(Some(
                parts.iter().map(|part| CompletedPart::builder().part_number(part.number).e_tag(&part.etag).build()).collect(),
            ))
            .build();
        self.client
            .complete_multipart_upload()
            .bucket(self.bucket())
            .key(object_key)
            .upload_id(upload_id)
            .multipart_upload(completed)
            .send()
            .await
            .map_err(|error| upload_error(error, self.bucket()))?;
        Ok(())
    }

    /// Throws away a multipart upload and the parts sent so far.
    pub async fn abort_multipart(&self, object_key: &str, upload_id: &str) -> Result<(), StorageError> {
        self.client
            .abort_multipart_upload()
            .bucket(self.bucket())
            .key(object_key)
            .upload_id(upload_id)
            .send()
            .await
            .map_err(|error| map_error(error, self.bucket()))?;
        Ok(())
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

    /// The size of the object at exactly `key`, or None when there's none.
    /// Watched folders check it before moving an uploaded original away.
    pub async fn object_size(&self, key: &str) -> Result<Option<i64>, StorageError> {
        let output = self
            .client
            .list_objects_v2()
            .bucket(self.bucket())
            .max_keys(1)
            .prefix(key)
            .send()
            .await
            .map_err(|error| map_error(error, self.bucket()))?;
        Ok(output.contents().first().filter(|object| object.key() == Some(key)).map(|object| object.size().unwrap_or_default()))
    }

    /// S3 has no rename or move: both are a server-side copy followed by
    /// deleting the original.
    pub async fn copy(&self, source_key: &str, destination_key: &str) -> Result<(), StorageError> {
        self.client
            .copy_object()
            .bucket(self.bucket())
            .copy_source(format!("{}/{}", self.bucket(), encode_copy_source(source_key)))
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

    /// Installs the lifecycle rules that delete expiring uploads (see
    /// `expiry`), keeping every other rule the bucket has exactly as it
    /// was. Saves nothing when they're already in place. A key that isn't
    /// allowed to manage rules, or a provider without them, is a status
    /// rather than an error.
    pub async fn ensure_expiry_rules(&self) -> Result<RulesStatus, StorageError> {
        match self.write_expiry_rules().await {
            Ok(()) => Ok(RulesStatus::Active),
            Err(error @ StorageError::LifecycleNotAllowed) => Ok(RulesStatus::Denied { message: error.to_string() }),
            Err(error @ StorageError::LifecycleUnsupported) => Ok(RulesStatus::Unsupported { message: error.to_string() }),
            Err(error) => Err(error),
        }
    }

    async fn write_expiry_rules(&self) -> Result<(), StorageError> {
        let existing = self.lifecycle_rules().await?.unwrap_or_default();
        let Some(xml) = crate::expiry::merged_rules(&existing) else { return Ok(()) };
        self.put_lifecycle(Some(xml)).await?;
        // Read back: a provider can answer 200 to a configuration it
        // ignored, and "active" means the bucket really deletes the files.
        let written = self.lifecycle_rules().await?.unwrap_or_default();
        if crate::expiry::rules_in_place(&written) {
            Ok(())
        } else {
            Err(StorageError::Unknown(t!("The provider didn't keep the lifecycle rules.")))
        }
    }

    /// Which of the `tmp/{N}d/` folders already hold files while their rule
    /// isn't in place yet. Setting the rules up makes the bucket delete
    /// those too, including ones older than N days, so the user is asked
    /// first. Empty when the rules are all in place or can't be read (setting
    /// up then reports why).
    pub async fn expiry_prefixes_in_use(&self) -> Result<Vec<String>, StorageError> {
        let existing = match self.lifecycle_rules().await {
            Ok(rules) => rules.unwrap_or_default(),
            Err(_) => return Ok(Vec::new()),
        };
        let mut in_use = Vec::new();
        for days in crate::expiry::missing_durations(&existing) {
            let prefix = crate::expiry::prefix(days);
            let listing = self
                .client
                .list_objects_v2()
                .bucket(self.bucket())
                .prefix(&prefix)
                .max_keys(1)
                .send()
                .await
                .map_err(|error| map_error(error, self.bucket()))?;
            if !listing.contents().is_empty() {
                in_use.push(prefix);
            }
        }
        Ok(in_use)
    }

    /// When the object at exactly `key` was last written (Unix
    /// milliseconds), or None when there's no such object.
    pub async fn last_modified(&self, key: &str) -> Result<Option<i64>, StorageError> {
        let output = self
            .client
            .list_objects_v2()
            .bucket(self.bucket())
            .max_keys(1)
            .prefix(key)
            .send()
            .await
            .map_err(|error| map_error(error, self.bucket()))?;
        Ok(output
            .contents()
            .first()
            .filter(|object| object.key() == Some(key))
            .map(|object| object.last_modified().map(|date| date.secs() * 1000).unwrap_or_default()))
    }

    async fn put_lifecycle(&self, xml: Option<String>) -> Result<(), StorageError> {
        let Some(xml) = xml else { return Ok(()) };
        let response = self.lifecycle_request(reqwest::Method::PUT, Some(xml.as_bytes())).await?;
        response.ok_or_failure(self.bucket())
    }

    /// Takes Aktar's expiry rules back out of the bucket, leaving its other
    /// rules as they are. Nothing is written when there are none; when no
    /// rules would be left, the configuration is deleted, since S3 doesn't
    /// accept an empty one.
    pub async fn remove_expiry_rules(&self) -> Result<(), StorageError> {
        let Some(existing) = self.lifecycle_rules().await? else { return Ok(()) };
        match crate::expiry::without_aktar_rules(&existing) {
            Removal::Nothing => Ok(()),
            Removal::Put(xml) => self.put_lifecycle(Some(xml)).await,
            Removal::DeleteConfiguration => {
                let response = self.lifecycle_request(reqwest::Method::DELETE, None).await?;
                response.ok_or_failure(self.bucket())
            }
        }
    }

    /// The bucket's lifecycle rules, or None when it has no configuration
    /// (answered with NoSuchLifecycleConfiguration rather than an empty one).
    async fn lifecycle_rules(&self) -> Result<Option<Vec<crate::expiry::XmlRule>>, StorageError> {
        let response = self.lifecycle_request(reqwest::Method::GET, None).await?;
        if response.is_success() {
            // Anything that doesn't read as a complete lifecycle
            // configuration stops here, before a write could replace the
            // bucket's own rules.
            return crate::expiry::parse_configuration(&response.body)
                .map(Some)
                .ok_or_else(|| StorageError::Unknown(t!("The bucket's lifecycle rules couldn't be read, so nothing was changed.")));
        }
        if crate::expiry::error_code(&response.body).0.as_deref() == Some("NoSuchLifecycleConfiguration") {
            return Ok(None);
        }
        Err(lifecycle_failure(response.status, &response.body, self.bucket()))
    }

    /// `?lifecycle` on the bucket, signed here and sent as is: the SDK's
    /// operations would decode and re-encode the rules (see `expiry`).
    async fn lifecycle_request(&self, method: reqwest::Method, body: Option<&[u8]>) -> Result<RawResponse, StorageError> {
        let url = bucket_url(&self.config, "lifecycle")?;
        let content_md5 = body.map(|body| {
            use md5::Digest;
            aws_smithy_types::base64::encode(md5::Md5::digest(body))
        });
        let mut headers: Vec<(&str, &str)> = Vec::new();
        if let Some(content_md5) = &content_md5 {
            headers.push(("content-type", "application/xml"));
            headers.push(("content-md5", content_md5));
        }

        let identity = self.credentials.clone().into();
        let mut settings = SigningSettings::default();
        // S3 wants x-amz-content-sha256, and the path as it is.
        settings.payload_checksum_kind = PayloadChecksumKind::XAmzSha256;
        settings.percent_encoding_mode = PercentEncodingMode::Single;
        settings.uri_path_normalization_mode = UriPathNormalizationMode::Disabled;
        let params = v4::SigningParams::builder()
            .identity(&identity)
            .region(&self.region)
            .name("s3")
            .time(std::time::SystemTime::now())
            .settings(settings)
            .build()
            .map_err(|error| StorageError::Unknown(error.to_string()))?
            .into();
        let signable = SignableRequest::new(
            method.as_str(),
            url.as_str(),
            headers.iter().copied(),
            SignableBody::Bytes(body.unwrap_or_default()),
        )
        .map_err(|error| StorageError::Unknown(error.to_string()))?;
        let (instructions, _) = sign(signable, &params)
            .map_err(|error| StorageError::Unknown(error.to_string()))?
            .into_parts();

        let mut request = lifecycle_client().request(method, url);
        for (name, value) in headers.iter().copied().chain(instructions.headers()) {
            request = request.header(name, value);
        }
        if let Some(body) = body {
            request = request.body(body.to_vec());
        }
        let response = request.send().await.map_err(|error| StorageError::Connection(innermost(&error)))?;
        let status = response.status().as_u16();
        let body = response.text().await.map_err(|error| StorageError::Connection(innermost(&error)))?;
        Ok(RawResponse { status, body })
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

/// Sends the parts of a multipart upload.
#[derive(Clone)]
pub struct PartSender {
    client: Client,
    bucket: String,
}

impl PartSender {
    /// Uploads `length` bytes of `path` from `offset` as part `number`, read
    /// from the file as they're sent. A dropped connection, a timeout or a
    /// server error (5xx, 429) is tried again after 1, 2, 4, 8 and 16
    /// seconds; anything else fails right away. Returns the part's ETag.
    #[allow(clippy::too_many_arguments)]
    pub async fn send(
        &self,
        object_key: &str,
        upload_id: &str,
        number: i32,
        path: &Path,
        offset: u64,
        length: u64,
        progress: Arc<Progress>,
    ) -> Result<String, StorageError> {
        let mut delays = PART_RETRY_DELAYS.iter();
        loop {
            let attempt = Arc::new(AtomicU64::new(0));
            let result = self.send_once(object_key, upload_id, number, path, offset, length, &progress, &attempt).await;
            match result {
                Ok(etag) => return Ok(etag),
                Err((error, transient)) => {
                    // What this try sent doesn't count any more.
                    progress.sent.fetch_sub(attempt.load(Ordering::Relaxed), Ordering::Relaxed);
                    match delays.next() {
                        Some(seconds) if transient => {
                            log::info!("Part {number} failed, trying again in {seconds} s: {error}");
                            tokio::time::sleep(Duration::from_secs(*seconds)).await;
                            // Waiting isn't stalling.
                            progress.last_activity.store(crate::util::now_millis(), Ordering::Relaxed);
                        }
                        _ => return Err(error),
                    }
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn send_once(
        &self,
        object_key: &str,
        upload_id: &str,
        number: i32,
        path: &Path,
        offset: u64,
        length: u64,
        progress: &Arc<Progress>,
        attempt: &Arc<AtomicU64>,
    ) -> Result<String, (StorageError, bool)> {
        let file = ByteStream::read_from()
            .path(path)
            .offset(offset)
            .length(Length::Exact(length))
            .build()
            .await
            .map_err(|error| (StorageError::Unknown(error.to_string()), false))?
            .into_inner();
        let (progress, attempt) = (progress.clone(), attempt.clone());
        let body = SdkBody::retryable(move || match file.try_clone() {
            Some(inner) => SdkBody::from_body_1_x(CountingBody { inner, progress: progress.clone(), attempt: Some(attempt.clone()) }),
            None => SdkBody::taken(),
        });
        let output = self
            .client
            .upload_part()
            .bucket(&self.bucket)
            .key(object_key)
            .upload_id(upload_id)
            .part_number(number)
            .content_length(length as i64)
            .body(ByteStream::new(body))
            // Retried here, with longer waits than the SDK's.
            .customize()
            .config_override(aws_sdk_s3::Config::builder().retry_config(RetryConfig::disabled()))
            .send()
            .await
            .map_err(|error| {
                let transient = is_transient(&error);
                (upload_error(error, &self.bucket), transient)
            })?;
        output.e_tag().map(str::to_string).ok_or((StorageError::Unknown("No ETag".into()), false))
    }
}

/// Whether trying again may work: the connection dropped or timed out, or
/// the server was overloaded or broke.
fn is_transient<E>(error: &SdkError<E, HttpResponse>) -> bool {
    match error {
        SdkError::DispatchFailure(_) | SdkError::TimeoutError(_) | SdkError::ResponseError(_) => true,
        SdkError::ServiceError(service) => {
            let status = service.raw().status().as_u16();
            status >= 500 || status == 429
        }
        _ => false,
    }
}

/// An upload's error: a connection failure is an interrupted upload.
fn upload_error<E>(error: SdkError<E, HttpResponse>, bucket: &str) -> StorageError
where
    E: ProvideErrorMetadata + std::error::Error + Send + Sync + 'static,
{
    match map_error(error, bucket) {
        StorageError::Connection(message) => StorageError::Network(message),
        other => other,
    }
}

/// How far an upload has got, and when it last moved.
pub struct Progress {
    sent: AtomicU64,
    total: AtomicU64,
    /// Unix milliseconds of the last bytes sent.
    last_activity: AtomicI64,
}

impl Default for Progress {
    fn default() -> Self {
        Self { sent: AtomicU64::new(0), total: AtomicU64::new(0), last_activity: AtomicI64::new(crate::util::now_millis()) }
    }
}

impl Progress {
    /// Starts counting `total` bytes, `done` of which are already sent
    /// (the parts of an upload being resumed).
    pub fn start(&self, total: u64, done: u64) {
        self.total.store(total, Ordering::Relaxed);
        self.sent.store(done, Ordering::Relaxed);
        self.last_activity.store(crate::util::now_millis(), Ordering::Relaxed);
    }

    /// 0 to 1, once the size is known.
    pub fn fraction(&self) -> Option<f64> {
        let total = self.total.load(Ordering::Relaxed);
        (total > 0).then(|| (self.sent.load(Ordering::Relaxed) as f64 / total as f64).min(1.0))
    }

    /// How long nothing has been sent. After the last byte, this is how long
    /// the server has taken to answer.
    pub fn idle_millis(&self) -> i64 {
        crate::util::now_millis() - self.last_activity.load(Ordering::Relaxed)
    }
}

/// Passes a request body through unchanged, counting the bytes the HTTP
/// client pulls from it: into the upload's progress, and into `attempt`
/// for a part, so a try that fails can be taken back out.
struct CountingBody {
    inner: SdkBody,
    progress: Arc<Progress>,
    attempt: Option<Arc<AtomicU64>>,
}

impl http_body::Body for CountingBody {
    type Data = <SdkBody as http_body::Body>::Data;
    type Error = <SdkBody as http_body::Body>::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        let poll = http_body::Body::poll_frame(Pin::new(&mut this.inner), context);
        if let Poll::Ready(Some(Ok(frame))) = &poll {
            if let Some(data) = frame.data_ref() {
                this.progress.sent.fetch_add(data.len() as u64, Ordering::Relaxed);
                if let Some(attempt) = &this.attempt {
                    attempt.fetch_add(data.len() as u64, Ordering::Relaxed);
                }
                this.progress.last_activity.store(crate::util::now_millis(), Ordering::Relaxed);
            }
        }
        poll
    }

    fn is_end_stream(&self) -> bool {
        http_body::Body::is_end_stream(&self.inner)
    }

    fn size_hint(&self) -> http_body::SizeHint {
        http_body::Body::size_hint(&self.inner)
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

async fn probe(url: &str) -> PublicLinkCheck {
    let Ok(client) = reqwest::Client::builder().timeout(Duration::from_secs(8)).build() else {
        return PublicLinkCheck::NoResponse;
    };
    let status = |method: reqwest::Method| {
        let request = client.request(method, url).header("cache-control", "no-cache");
        async move { request.send().await.ok().map(|response| response.status().as_u16()) }
    };
    let head = status(reqwest::Method::HEAD).await;
    // Some servers and CDNs don't answer HEAD; ask again the way a browser
    // would before calling the link broken.
    let code = match head {
        Some(405 | 501) => status(reqwest::Method::GET).await,
        other => other,
    };
    match code {
        None => PublicLinkCheck::NoResponse,
        Some(code) if (200..300).contains(&code) => PublicLinkCheck::Reachable,
        Some(code) => PublicLinkCheck::Status { code },
    }
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
        SdkError::DispatchFailure(_) | SdkError::TimeoutError(_) => StorageError::Connection(readable(&error)),
        _ if status.is_some_and(|status| status >= 500 || status == 429) => {
            StorageError::Server(error.message().map(str::to_string).unwrap_or_else(|| readable(&error)))
        }
        _ => StorageError::Unknown(error.message().map(str::to_string).unwrap_or_else(|| readable(&error))),
    }
}

struct RawResponse {
    status: u16,
    body: String,
}

impl RawResponse {
    fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    fn ok_or_failure(self, bucket: &str) -> Result<(), StorageError> {
        if self.is_success() {
            Ok(())
        } else {
            Err(lifecycle_failure(self.status, &self.body, bucket))
        }
    }
}

/// Lifecycle requests get their own client, with an overall timeout: the
/// shared SDK client isn't one reqwest can use.
fn lifecycle_client() -> reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(15))
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap_or_default()
        })
        .clone()
}

/// `https://endpoint/bucket?query` (path style) or
/// `https://bucket.endpoint/?query` (virtual hosted), the same URLs the SDK
/// uses for the destination.
fn bucket_url(config: &DestinationConfig, query: &str) -> Result<url::Url, StorageError> {
    let invalid = || StorageError::Unknown(t!("The endpoint URL is not valid."));
    let bucket = config.bucket.trim();
    let mut url = url::Url::parse(&normalized_endpoint(&config.endpoint)).map_err(|_| invalid())?;
    let host = url.host_str().filter(|host| !host.is_empty()).ok_or_else(invalid)?.to_string();
    if bucket.is_empty() {
        return Err(invalid());
    }
    let base_path = url.path().trim_end_matches('/').to_string();
    if config.force_path_style {
        url.set_path(&format!("{base_path}/{bucket}"));
    } else {
        url.set_host(Some(&format!("{bucket}.{host}"))).map_err(|_| invalid())?;
        url.set_path(&format!("{base_path}/"));
    }
    url.set_query(Some(query));
    Ok(url)
}

/// What a failed lifecycle request says. Keys that can upload but not
/// change bucket settings (an R2 "Object Read & Write" token) get "access
/// denied"; a provider without lifecycle rules answers "not implemented"
/// or refuses the method.
fn lifecycle_failure(status: u16, body: &str, bucket: &str) -> StorageError {
    let (code, message) = crate::expiry::error_code(body);
    match (status, code.as_deref()) {
        (_, Some("NoSuchBucket")) => StorageError::BucketNotFound(bucket.to_string()),
        (_, Some("InvalidAccessKeyId" | "SignatureDoesNotMatch")) => StorageError::InvalidCredentials,
        (403, _) | (_, Some("AccessDenied")) => StorageError::LifecycleNotAllowed,
        (405 | 501, _) | (_, Some("NotImplemented")) => StorageError::LifecycleUnsupported,
        _ => StorageError::Unknown(message.or(code).unwrap_or_else(|| format!("HTTP {status}"))),
    }
}

fn innermost(error: &dyn std::error::Error) -> String {
    let mut source = error;
    while let Some(next) = source.source() {
        source = next;
    }
    source.to_string()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::destinations::ProviderPreset;

    fn config(endpoint: &str, bucket: &str, force_path_style: bool) -> DestinationConfig {
        DestinationConfig {
            id: "TEST".into(),
            name: "Test".into(),
            preset: ProviderPreset::CustomS3,
            account_id: None,
            endpoint: endpoint.into(),
            region: "auto".into(),
            bucket: bucket.into(),
            public_base_url: String::new(),
            object_path_template: "{filename}.{ext}".into(),
            force_path_style,
            is_default: false,
            output_mode: None,
            expiry_days: None,
            temporary_link: None,
            image_metadata: None,
            folder_upload: None,
            image_processing: None,
        }
    }

    #[test]
    fn builds_bucket_urls() {
        let url = |endpoint: &str, path_style: bool| bucket_url(&config(endpoint, "files", path_style), "lifecycle").map(|url| url.to_string());
        assert_eq!(url("https://acc.r2.cloudflarestorage.com", false).unwrap(), "https://files.acc.r2.cloudflarestorage.com/?lifecycle");
        assert_eq!(url("acc.r2.cloudflarestorage.com/", false).unwrap(), "https://files.acc.r2.cloudflarestorage.com/?lifecycle");
        assert_eq!(url("http://127.0.0.1:9000", true).unwrap(), "http://127.0.0.1:9000/files?lifecycle");
        assert_eq!(url("https://minio.example.com/s3/", true).unwrap(), "https://minio.example.com/s3/files?lifecycle");
        assert!(url("", false).is_err());
        assert!(bucket_url(&config("https://example.com", " ", true), "lifecycle").is_err());
    }

    fn failure(status: u16, code: &str) -> StorageError {
        let body = format!("<?xml version=\"1.0\"?><Error><Code>{code}</Code><Message>Details</Message></Error>");
        lifecycle_failure(status, &body, "files")
    }

    #[test]
    fn sorts_lifecycle_failures() {
        assert!(matches!(failure(403, "AccessDenied"), StorageError::LifecycleNotAllowed));
        assert!(matches!(lifecycle_failure(403, "", "files"), StorageError::LifecycleNotAllowed));
        assert!(matches!(failure(400, "AccessDenied"), StorageError::LifecycleNotAllowed));
        assert!(matches!(failure(403, "InvalidAccessKeyId"), StorageError::InvalidCredentials));
        assert!(matches!(failure(403, "SignatureDoesNotMatch"), StorageError::InvalidCredentials));
        assert!(matches!(failure(404, "NoSuchBucket"), StorageError::BucketNotFound(bucket) if bucket == "files"));
        assert!(matches!(failure(501, "NotImplemented"), StorageError::LifecycleUnsupported));
        assert!(matches!(lifecycle_failure(405, "", "files"), StorageError::LifecycleUnsupported));
        assert!(matches!(failure(400, "NotImplemented"), StorageError::LifecycleUnsupported));
        assert!(matches!(failure(400, "MalformedXML"), StorageError::Unknown(message) if message == "Details"));
        assert!(matches!(lifecycle_failure(500, "", "files"), StorageError::Unknown(message) if message == "HTTP 500"));
    }
}

/// Runs against a real S3-compatible server, e.g. `moto_server -p 9100`
/// (moto 5.1: 5.2 answers ListParts in a shape the SDK rejects) with a
/// bucket named "aktar-test":
///
/// ```text
/// AKTAR_TEST_S3_ENDPOINT=http://127.0.0.1:9100 cargo test storage -- --ignored
/// ```
///
/// To check request signatures too, start moto with
/// `INITIAL_NO_AUTH_ACTION_COUNT` set, create an IAM user and key, and pass
/// them as `AKTAR_TEST_S3_ACCESS_KEY` and `AKTAR_TEST_S3_SECRET`.
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
            output_mode: None,
            expiry_days: None,
            temporary_link: None,
            image_metadata: None,
            folder_upload: None,
            image_processing: None,
        };
        // Moto takes any key unless it's started with authentication on.
        let credentials = StorageCredentials {
            access_key_id: std::env::var("AKTAR_TEST_S3_ACCESS_KEY").unwrap_or_else(|_| "AKIATEST".into()),
            secret_access_key: std::env::var("AKTAR_TEST_S3_SECRET").unwrap_or_else(|_| secret.into()),
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
        assert!(result.public_link.is_some());

        let file = std::env::temp_dir().join("aktar test ü.txt");
        std::fs::write(&file, b"hello from aktar").unwrap();
        let progress = Arc::new(Progress::default());
        let uploaded = storage.upload(&file, "live/aktar test ü.txt", "text/plain", progress.clone()).await.unwrap();
        assert_eq!(uploaded.byte_size, 16);
        assert_eq!(progress.fraction(), Some(1.0));
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

        // "+" and parentheses survive both the public URL and CopyObject.
        let plus = storage.upload(&file, "live/a+b (1).txt", "text/plain", Arc::new(Progress::default())).await.unwrap();
        assert!(plus.public_url.ends_with("/live/a%2Bb%20(1).txt"));
        assert!(crate::bucket::move_object(&storage, "live/a+b (1).txt", "live/c+d (2).txt").await.is_ok());
        assert!(storage.object_exists("live/c+d (2).txt").await.unwrap());
        assert!(!storage.object_exists("live/a+b (1).txt").await.unwrap());
        storage.delete("live/c+d (2).txt").await.unwrap();

        storage.delete("moved/renamed.txt").await.unwrap();
        storage.delete("live/empty/").await.unwrap();
        assert!(!storage.object_exists("moved/renamed.txt").await.unwrap());
    }

    /// A file of `size` bytes that differ from part to part, and its SHA-256.
    fn big_file(name: &str, size: usize) -> (std::path::PathBuf, String) {
        let path = std::env::temp_dir().join(format!("aktar-{name}-{}.bin", crate::util::new_id()));
        let mut data = vec![0u8; size];
        let mut seed = 0x2545_F491u32;
        for byte in data.iter_mut() {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            *byte = seed as u8;
        }
        std::fs::write(&path, &data).unwrap();
        let sha256 = crate::util::content_hashes(&path).unwrap().sha256;
        (path, sha256)
    }

    async fn downloaded_sha256(storage: &S3Provider, key: &str) -> String {
        let url = storage.temporary_url(key, 600).await.unwrap();
        let bytes = reqwest::get(&url).await.unwrap().bytes().await.unwrap();
        let path = std::env::temp_dir().join(format!("aktar-download-{}", crate::util::new_id()));
        std::fs::write(&path, &bytes).unwrap();
        let sha256 = crate::util::content_hashes(&path).unwrap().sha256;
        std::fs::remove_file(path).unwrap();
        sha256
    }

    fn session_store() -> (crate::multipart::SessionStore, std::path::PathBuf) {
        let directory = std::env::temp_dir().join(format!("aktar-sessions-{}", crate::util::new_id()));
        std::fs::create_dir_all(&directory).unwrap();
        (crate::multipart::SessionStore::load(&directory), directory)
    }

    #[tokio::test]
    #[ignore]
    async fn uploads_big_files_in_parts() {
        use crate::multipart::{self, SourceFile};
        let Some(storage) = provider("aktar-test", "secret") else { return };
        let (store, directory) = session_store();
        let size = 70 * 1024 * 1024 + 123;
        let (file, sha256) = big_file("multipart", size);
        let key = "live/multipart.bin";
        let session = multipart::start(&storage, &store, "TEST", "aktar-test", key, "application/octet-stream", size as u64, SourceFile::of(&file), None)
            .await
            .unwrap();
        assert_eq!(session.part_size, 16 * 1024 * 1024);
        let progress = Arc::new(Progress::default());
        multipart::upload(&storage, &store, session, &file, "application/octet-stream", false, progress.clone()).await.unwrap();
        assert_eq!(progress.fraction(), Some(1.0));
        assert!(store.all().is_empty());
        assert_eq!(downloaded_sha256(&storage, key).await, sha256);
        storage.delete(key).await.unwrap();
        std::fs::remove_file(file).unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    /// Stopped halfway (the app quit), then picked up again from the saved
    /// session after a "restart": only the missing parts are sent.
    #[tokio::test]
    #[ignore]
    async fn resumes_an_unfinished_upload() {
        use crate::multipart::{self, Candidate, SessionStore, SourceFile};
        let Some(storage) = provider("aktar-test", "secret") else { return };
        let (store, directory) = session_store();
        let size = 100 * 1024 * 1024;
        let (file, sha256) = big_file("resume", size);
        let key = "live/resumed.bin";
        let source = SourceFile::of(&file);
        let session =
            multipart::start(&storage, &store, "TEST", "aktar-test", key, "application/octet-stream", size as u64, source.clone(), None)
                .await
                .unwrap();
        // Two parts get there; the rest are cut off.
        let sender = storage.part_sender();
        for number in 1..=2 {
            let part_size = 16 * 1024 * 1024;
            let etag = sender
                .send(key, &session.upload_id, number, &file, (number as u64 - 1) * part_size, part_size, Arc::new(Progress::default()))
                .await
                .unwrap();
            if number == 1 {
                // Only the first one was saved before the "crash"; the
                // server's list has both.
                let mut saved = store.get(&session.id).unwrap();
                saved.parts.push(UploadedPart { number, etag, size: part_size });
                store.put(saved);
            }
        }
        let interrupted = multipart::upload(&storage, &store, session.clone(), &file, "application/octet-stream", true, Arc::new(Progress::default()));
        assert!(tokio::time::timeout(Duration::from_millis(30), interrupted).await.is_err());

        let restarted = SessionStore::load(&directory);
        let candidate = Candidate { destination_id: "TEST", bucket: "aktar-test", file_size: size as u64, source: source.as_ref(), sha256: None };
        let found = restarted.matching(&candidate, &[]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].object_key, key);
        let progress = Arc::new(Progress::default());
        multipart::upload(&storage, &restarted, found[0].clone(), &file, "application/octet-stream", true, progress.clone())
            .await
            .unwrap();
        assert_eq!(progress.fraction(), Some(1.0));
        assert!(restarted.all().is_empty());
        assert_eq!(downloaded_sha256(&storage, key).await, sha256);
        storage.delete(key).await.unwrap();
        std::fs::remove_file(file).unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    #[ignore]
    async fn aborts_a_cancelled_upload() {
        use crate::multipart::{self, SourceFile};
        let Some(storage) = provider("aktar-test", "secret") else { return };
        let (store, directory) = session_store();
        let size = 40 * 1024 * 1024;
        let (file, _) = big_file("abort", size);
        let key = "live/aborted.bin";
        let session = multipart::start(&storage, &store, "TEST", "aktar-test", key, "application/octet-stream", size as u64, SourceFile::of(&file), None)
            .await
            .unwrap();
        let sender = storage.part_sender();
        sender.send(key, &session.upload_id, 1, &file, 0, 16 * 1024 * 1024, Arc::new(Progress::default())).await.unwrap();
        assert_eq!(storage.uploaded_parts(key, &session.upload_id).await.unwrap().unwrap().len(), 1);
        multipart::abort(&storage, &store, &session).await;
        assert!(store.all().is_empty());
        assert_eq!(storage.uploaded_parts(key, &session.upload_id).await.unwrap(), None);
        assert!(!storage.object_exists(key).await.unwrap());
        // Continuing it after all starts over under the same key.
        store.put(session.clone());
        multipart::upload(&storage, &store, session, &file, "application/octet-stream", true, Arc::new(Progress::default())).await.unwrap();
        assert!(storage.object_exists(key).await.unwrap());
        storage.delete(key).await.unwrap();
        std::fs::remove_file(file).unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    async fn lifecycle_xml(storage: &S3Provider) -> Option<String> {
        let response = storage.lifecycle_request(reqwest::Method::GET, None).await.unwrap();
        response.is_success().then_some(response.body)
    }

    #[tokio::test]
    #[ignore]
    async fn sets_up_and_removes_expiry_rules() {
        let Some(storage) = provider("aktar-test", "secret") else { return };

        // One of the user's own rules, which must survive both ways.
        let own = "<ID>user-logs</ID><Filter><Prefix>logs/</Prefix></Filter><Status>Enabled</Status><Expiration><Days>90</Days></Expiration>";
        let configuration = format!(r#"<LifecycleConfiguration xmlns="http://s3.amazonaws.com/doc/2006-03-01/"><Rule>{own}</Rule></LifecycleConfiguration>"#);
        storage.put_lifecycle(Some(configuration)).await.unwrap();

        assert_eq!(storage.ensure_expiry_rules().await.unwrap(), RulesStatus::Active);
        assert_eq!(storage.ensure_expiry_rules().await.unwrap(), RulesStatus::Active);
        let rules = crate::expiry::parse_rules(&lifecycle_xml(&storage).await.unwrap());
        let ids: Vec<&str> = rules.iter().filter_map(|rule| rule.id.as_deref()).collect();
        assert_eq!(ids, ["user-logs", "aktar-expire-1d", "aktar-expire-7d", "aktar-expire-14d", "aktar-expire-30d"]);
        assert_eq!(crate::expiry::merged_rules(&rules), None);

        // Removing keeps the user's rule; removing again changes nothing.
        storage.remove_expiry_rules().await.unwrap();
        let rules = crate::expiry::parse_rules(&lifecycle_xml(&storage).await.unwrap());
        assert_eq!(rules.iter().filter_map(|rule| rule.id.as_deref()).collect::<Vec<_>>(), ["user-logs"]);
        storage.remove_expiry_rules().await.unwrap();

        // A bucket with no rules at all answers NoSuchLifecycleConfiguration.
        let response = storage.lifecycle_request(reqwest::Method::DELETE, None).await.unwrap();
        assert!(response.is_success());
        assert_eq!(lifecycle_xml(&storage).await, None);
        storage.remove_expiry_rules().await.unwrap();
        assert_eq!(storage.ensure_expiry_rules().await.unwrap(), RulesStatus::Active);
        assert_eq!(crate::expiry::parse_rules(&lifecycle_xml(&storage).await.unwrap()).len(), 4);

        // With only Aktar's rules left, the configuration goes entirely.
        storage.remove_expiry_rules().await.unwrap();
        assert_eq!(lifecycle_xml(&storage).await, None);

        // Files already in a tmp/{N}d/ folder are reported before setting
        // the rules up, since the bucket would delete them too...
        storage.create_folder("tmp/7d/existing/").await.unwrap();
        assert_eq!(storage.expiry_prefixes_in_use().await.unwrap(), ["tmp/7d/"]);
        let written = storage.last_modified("tmp/7d/existing/").await.unwrap().unwrap();
        assert!((written - crate::util::now_millis()).abs() < 10 * 60_000);
        assert_eq!(storage.last_modified("tmp/7d/existing").await.unwrap(), None);
        // ...and not once the rules are in place.
        assert_eq!(storage.ensure_expiry_rules().await.unwrap(), RulesStatus::Active);
        assert!(storage.expiry_prefixes_in_use().await.unwrap().is_empty());
        storage.delete("tmp/7d/existing/").await.unwrap();
        storage.remove_expiry_rules().await.unwrap();
        assert_eq!(lifecycle_xml(&storage).await, None);
    }

    #[tokio::test]
    #[ignore]
    async fn lifecycle_requests_report_missing_buckets() {
        let Some(storage) = provider("no-such-bucket-aktar", "secret") else { return };
        match storage.ensure_expiry_rules().await {
            Err(StorageError::BucketNotFound(bucket)) => assert_eq!(bucket, "no-such-bucket-aktar"),
            other => panic!("unexpected {other:?}"),
        }
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
