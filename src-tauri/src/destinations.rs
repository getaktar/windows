//! Non-secret destination configuration, persisted as JSON. Secrets never
//! live here, see `credentials`.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::t;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProviderPreset {
    #[serde(rename = "cloudflareR2")]
    CloudflareR2,
    #[serde(rename = "amazonS3")]
    AmazonS3,
    #[serde(rename = "minIO")]
    MinIO,
    #[serde(rename = "backblazeB2")]
    BackblazeB2,
    #[serde(rename = "digitalOceanSpaces")]
    DigitalOceanSpaces,
    #[serde(rename = "customS3")]
    CustomS3,
}

impl ProviderPreset {
    /// The raw value the Mac app uses, which the local API reports as `provider`.
    pub fn raw_value(self) -> &'static str {
        match self {
            ProviderPreset::CloudflareR2 => "cloudflareR2",
            ProviderPreset::AmazonS3 => "amazonS3",
            ProviderPreset::MinIO => "minIO",
            ProviderPreset::BackblazeB2 => "backblazeB2",
            ProviderPreset::DigitalOceanSpaces => "digitalOceanSpaces",
            ProviderPreset::CustomS3 => "customS3",
        }
    }

    pub fn display_name(self) -> String {
        match self {
            ProviderPreset::CloudflareR2 => "Cloudflare R2".into(),
            ProviderPreset::AmazonS3 => "Amazon S3".into(),
            ProviderPreset::MinIO => "MinIO".into(),
            ProviderPreset::BackblazeB2 => "Backblaze B2".into(),
            ProviderPreset::DigitalOceanSpaces => "DigitalOcean Spaces".into(),
            ProviderPreset::CustomS3 => t!("Other S3-Compatible"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DestinationConfig {
    pub id: String,
    pub name: String,
    pub preset: ProviderPreset,
    #[serde(rename = "accountID", default)]
    pub account_id: Option<String>,
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    #[serde(rename = "publicBaseURL")]
    pub public_base_url: String,
    pub object_path_template: String,
    pub force_path_style: bool,
    #[serde(default)]
    pub is_default: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Wrapper {
    destinations: Vec<DestinationConfig>,
    #[serde(rename = "defaultID")]
    default_id: Option<String>,
}

pub struct DestinationStore {
    path: PathBuf,
    inner: Mutex<Wrapper>,
}

impl DestinationStore {
    pub fn load(directory: &Path) -> Self {
        let path = directory.join("destinations.json");
        let mut wrapper: Wrapper = std::fs::read(&path)
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default();
        sync_default_flags(&mut wrapper);
        Self { path, inner: Mutex::new(wrapper) }
    }

    pub fn all(&self) -> Vec<DestinationConfig> {
        self.inner.lock().unwrap().destinations.clone()
    }

    pub fn default_destination(&self) -> Option<DestinationConfig> {
        let inner = self.inner.lock().unwrap();
        inner
            .destinations
            .iter()
            .find(|destination| Some(&destination.id) == inner.default_id.as_ref())
            .or_else(|| inner.destinations.first())
            .cloned()
    }

    /// Looks a destination up by ID (case-insensitively, since callers of
    /// the local API may lowercase it), or the default one for no ID.
    pub fn find(&self, id: Option<&str>) -> Option<DestinationConfig> {
        match id.map(str::trim).filter(|id| !id.is_empty()) {
            None => self.default_destination(),
            Some(id) => self
                .inner
                .lock()
                .unwrap()
                .destinations
                .iter()
                .find(|destination| destination.id.eq_ignore_ascii_case(id))
                .cloned(),
        }
    }

    pub fn add(&self, mut destination: DestinationConfig) {
        self.mutate(|wrapper| {
            if wrapper.destinations.is_empty() {
                destination.is_default = true;
                wrapper.default_id = Some(destination.id.clone());
            }
            wrapper.destinations.push(destination);
        });
    }

    pub fn update(&self, destination: DestinationConfig) {
        self.mutate(|wrapper| {
            if let Some(existing) = wrapper.destinations.iter_mut().find(|d| d.id == destination.id) {
                *existing = destination;
            }
        });
    }

    pub fn remove(&self, id: &str) {
        self.mutate(|wrapper| {
            wrapper.destinations.retain(|destination| destination.id != id);
            if wrapper.default_id.as_deref() == Some(id) {
                wrapper.default_id = wrapper.destinations.first().map(|d| d.id.clone());
            }
        });
        let _ = crate::credentials::delete(id);
    }

    pub fn set_default(&self, id: &str) {
        self.mutate(|wrapper| wrapper.default_id = Some(id.to_string()));
    }

    fn mutate(&self, change: impl FnOnce(&mut Wrapper)) {
        let mut inner = self.inner.lock().unwrap();
        change(&mut inner);
        sync_default_flags(&mut inner);
        crate::util::write_json_atomically(&self.path, &*inner);
    }
}

/// `default_id` is what uploads use, so it wins; the per-destination flags
/// (which Settings shows) only decide when it points nowhere.
fn sync_default_flags(wrapper: &mut Wrapper) {
    let points_somewhere = wrapper
        .destinations
        .iter()
        .any(|destination| Some(&destination.id) == wrapper.default_id.as_ref());
    if !points_somewhere {
        wrapper.default_id = wrapper
            .destinations
            .iter()
            .find(|destination| destination.is_default)
            .or_else(|| wrapper.destinations.first())
            .map(|destination| destination.id.clone());
    }
    for destination in &mut wrapper.destinations {
        destination.is_default = Some(&destination.id) == wrapper.default_id.as_ref();
    }
}
