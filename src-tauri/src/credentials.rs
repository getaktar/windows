//! Secrets (Access Key ID, Secret Access Key, session token, and the local
//! API token) live only in Windows Credential Manager, never in the JSON
//! config or the SQLite history.

use serde::{Deserialize, Serialize};

use crate::t;

const CREDENTIALS_SERVICE: &str = "com.getaktar.windows.credentials";
const LOCAL_API_SERVICE: &str = "com.getaktar.windows.local-api";
const LOCAL_API_ACCOUNT: &str = "token";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageCredentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    #[serde(default)]
    pub session_token: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    #[error("{}", t!("No credentials found in Credential Manager for this destination."))]
    NotFound,
    #[error("{}", t!("Credential Manager error ({0}).", .0))]
    Store(String),
}

impl From<keyring::Error> for CredentialError {
    fn from(error: keyring::Error) -> Self {
        match error {
            keyring::Error::NoEntry => CredentialError::NotFound,
            other => CredentialError::Store(other.to_string()),
        }
    }
}

fn entry(service: &str, account: &str) -> Result<keyring::Entry, CredentialError> {
    Ok(keyring::Entry::new(service, account)?)
}

pub fn save(credentials: &StorageCredentials, destination_id: &str) -> Result<(), CredentialError> {
    let json = serde_json::to_vec(credentials).map_err(|error| CredentialError::Store(error.to_string()))?;
    // Raw UTF-8 bytes: Credential Manager caps a secret at 2560 bytes, and
    // storing it as a password (UTF-16) would halve the room a long STS
    // session token needs.
    entry(CREDENTIALS_SERVICE, destination_id)?.set_secret(&json)?;
    Ok(())
}

pub fn load(destination_id: &str) -> Result<StorageCredentials, CredentialError> {
    let json = entry(CREDENTIALS_SERVICE, destination_id)?.get_secret()?;
    serde_json::from_slice(&json).map_err(|error| CredentialError::Store(error.to_string()))
}

pub fn delete(destination_id: &str) -> Result<(), CredentialError> {
    match entry(CREDENTIALS_SERVICE, destination_id)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub fn load_api_token() -> Option<String> {
    entry(LOCAL_API_SERVICE, LOCAL_API_ACCOUNT)
        .ok()?
        .get_password()
        .ok()
        .filter(|token| !token.is_empty())
}

pub fn save_api_token(token: &str) {
    if let Ok(entry) = entry(LOCAL_API_SERVICE, LOCAL_API_ACCOUNT) {
        let _ = entry.set_password(token);
    }
}
