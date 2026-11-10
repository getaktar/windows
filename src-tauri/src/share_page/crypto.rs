//! Encrypted links (docs/share-pages.md, part 5): the file is encrypted on
//! this PC, record by record, and the page decrypts it in the browser.
//! The format, shared with every app and the page itself:
//!
//! ```text
//! header (42 bytes): "AKE1", version 1, kdf (0 key in the link, 1 password),
//!   iterations u32, salt 16, nonce prefix 8, record size u32 (65536),
//!   metadata length u32 (ciphertext length of record 0)
//! record i: AES-256-GCM(key, nonce prefix || i (u32), AAD = header || final)
//! record 0: {"name","type","size"(,"syntax")} JSON; records 1...n: the file
//! ```
//!
//! All integers big-endian. Never the whole file in memory, and never
//! logged.

use std::io::{Read, Write};
use std::path::Path;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::RngCore;
use serde::Serialize;

pub const HEADER_LENGTH: usize = 42;
pub const RECORD_SIZE: usize = 65_536;
pub const PASSWORD_ITERATIONS: u32 = 600_000;
const TAG_LENGTH: usize = 16;
const MAGIC: [u8; 4] = *b"AKE1";

/// The record 0 plaintext: exactly `{"name":..,"type":..,"size":..}`, plus
/// `"syntax"` for text with a known language.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct Metadata {
    pub name: String,
    #[serde(rename = "type")]
    pub mime_type: String,
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub syntax: Option<String>,
}

impl Metadata {
    pub fn json(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }
}

/// Where the key comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kdf {
    /// 32 random bytes, in the link's fragment.
    Link,
    /// PBKDF2-HMAC-SHA256 of the password.
    Password { salt: [u8; 16], iterations: u32 },
}

/// The key a file is encrypted with, and how it was made.
#[derive(Clone, PartialEq, Eq)]
pub struct FileKey {
    pub key: [u8; 32],
    pub kdf: Kdf,
}

impl std::fmt::Debug for FileKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("FileKey").field("kdf", &self.kdf).finish_non_exhaustive()
    }
}

impl FileKey {
    /// A new random key for "Key in the Link".
    pub fn random() -> Self {
        let mut key = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut key);
        Self { key, kdf: Kdf::Link }
    }

    /// "Ask for a Password": a new salt and the derived key. Takes a few
    /// seconds on purpose, so run it off the async runtime's workers.
    pub fn from_password(password: &str) -> Self {
        let mut salt = [0u8; 16];
        rand::rngs::OsRng.fill_bytes(&mut salt);
        Self::with_salt(password, salt, PASSWORD_ITERATIONS)
    }

    pub fn with_salt(password: &str, salt: [u8; 16], iterations: u32) -> Self {
        Self { key: password_key(password, &salt, iterations), kdf: Kdf::Password { salt, iterations } }
    }

    /// From what history keeps (`encryptionKey`, `encryptionSalt`), for
    /// Replace File.
    pub fn from_stored(key: &str, salt: Option<&str>) -> Option<Self> {
        let key: [u8; 32] = URL_SAFE_NO_PAD.decode(key.trim_end_matches('=')).ok()?.try_into().ok()?;
        let kdf = match salt {
            None => Kdf::Link,
            Some(salt) => Kdf::Password { salt: from_hex(salt)?.try_into().ok()?, iterations: PASSWORD_ITERATIONS },
        };
        Some(Self { key, kdf })
    }

    /// base64url of the key, no padding: the link's `#k=` and history's
    /// `encryptionKey`.
    pub fn encoded_key(&self) -> String {
        base64url(&self.key)
    }

    /// Hex of the salt, for history's `encryptionSalt` (password only).
    pub fn encoded_salt(&self) -> Option<String> {
        match self.kdf {
            Kdf::Link => None,
            Kdf::Password { salt, .. } => Some(crate::util::hex(&salt)),
        }
    }

    pub fn is_password(&self) -> bool {
        matches!(self.kdf, Kdf::Password { .. })
    }

    /// The link to share: the page's URL, plus the key as its fragment
    /// unless a password opens it.
    #[cfg(test)]
    pub fn link(&self, page_url: &str) -> String {
        match self.kdf {
            Kdf::Link => format!("{page_url}#k={}", self.encoded_key()),
            Kdf::Password { .. } => page_url.to_string(),
        }
    }
}

/// PBKDF2-HMAC-SHA256 of the NFC-normalized UTF-8 password, 32 bytes.
pub fn password_key(password: &str, salt: &[u8], iterations: u32) -> [u8; 32] {
    let normalized = icu_normalizer::ComposingNormalizerBorrowed::new_nfc().normalize(password);
    let mut key = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<sha2::Sha256>(normalized.as_bytes(), salt, iterations, &mut key);
    key
}

pub fn base64url(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn from_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len()).step_by(2).map(|at| u8::from_str_radix(text.get(at..at + 2)?, 16).ok()).collect()
}

/// A random nonce prefix, new for every file and every Replace File.
pub fn new_nonce_prefix() -> [u8; 8] {
    let mut prefix = [0u8; 8];
    rand::rngs::OsRng.fill_bytes(&mut prefix);
    prefix
}

/// The length of the encrypted file for `size` plaintext bytes.
pub fn encrypted_size(size: u64, metadata_length: usize) -> u64 {
    let records = size.div_ceil(RECORD_SIZE as u64).max(1);
    (HEADER_LENGTH + metadata_length + TAG_LENGTH) as u64 + size + records * TAG_LENGTH as u64
}

fn header(key: &FileKey, nonce_prefix: [u8; 8], metadata_length: usize) -> [u8; HEADER_LENGTH] {
    let (kdf, iterations, salt) = match key.kdf {
        Kdf::Link => (0u8, 0u32, [0u8; 16]),
        Kdf::Password { salt, iterations } => (1, iterations, salt),
    };
    let mut header = [0u8; HEADER_LENGTH];
    header[0..4].copy_from_slice(&MAGIC);
    header[4] = 1;
    header[5] = kdf;
    header[6..10].copy_from_slice(&iterations.to_be_bytes());
    header[10..26].copy_from_slice(&salt);
    header[26..34].copy_from_slice(&nonce_prefix);
    header[34..38].copy_from_slice(&(RECORD_SIZE as u32).to_be_bytes());
    header[38..42].copy_from_slice(&((metadata_length + TAG_LENGTH) as u32).to_be_bytes());
    header
}

fn nonce(header: &[u8; HEADER_LENGTH], index: u32) -> [u8; 12] {
    let mut nonce = [0u8; 12];
    nonce[..8].copy_from_slice(&header[26..34]);
    nonce[8..].copy_from_slice(&index.to_be_bytes());
    nonce
}

fn aad(header: &[u8; HEADER_LENGTH], last: bool) -> [u8; HEADER_LENGTH + 1] {
    let mut aad = [0u8; HEADER_LENGTH + 1];
    aad[..HEADER_LENGTH].copy_from_slice(header);
    aad[HEADER_LENGTH] = u8::from(last);
    aad
}

fn io_error(message: &str) -> std::io::Error {
    std::io::Error::other(message.to_string())
}

/// Encrypts `size` bytes from `reader` into `writer`, one 64 KB record at a
/// time. `metadata.size` must be `size`. Returns the bytes written.
pub fn encrypt(
    mut reader: impl Read,
    size: u64,
    metadata: &Metadata,
    key: &FileKey,
    nonce_prefix: [u8; 8],
    mut writer: impl Write,
) -> std::io::Result<u64> {
    if metadata.size != size {
        return Err(io_error("The file changed while it was read."));
    }
    let cipher = Aes256Gcm::new(&key.key.into());
    let metadata_plain = metadata.json();
    let header = header(key, nonce_prefix, metadata_plain.len());
    let seal = |index: u32, last: bool, plain: &[u8]| -> std::io::Result<Vec<u8>> {
        cipher
            .encrypt(&Nonce::from(nonce(&header, index)), Payload { msg: plain, aad: &aad(&header, last) })
            .map_err(|_| io_error("The file couldn't be encrypted."))
    };
    writer.write_all(&header)?;
    writer.write_all(&seal(0, false, &metadata_plain)?)?;
    let records = size.div_ceil(RECORD_SIZE as u64).max(1);
    let mut buffer = vec![0u8; RECORD_SIZE];
    let mut remaining = size;
    for index in 1..=records {
        let length = remaining.min(RECORD_SIZE as u64) as usize;
        reader.read_exact(&mut buffer[..length])?;
        remaining -= length as u64;
        let index = u32::try_from(index).map_err(|_| io_error("The file is too large to encrypt."))?;
        writer.write_all(&seal(index, index as u64 == records, &buffer[..length])?)?;
    }
    writer.flush()?;
    Ok(encrypted_size(size, metadata_plain.len()))
}

/// Encrypts the file at `source` into a new file at `target`.
pub fn encrypt_file(source: &Path, target: &Path, metadata: &Metadata, key: &FileKey, nonce_prefix: [u8; 8]) -> std::io::Result<u64> {
    let input = std::fs::File::open(source)?;
    let size = input.metadata()?.len();
    let output = std::io::BufWriter::new(std::fs::File::create(target)?);
    let result = encrypt(std::io::BufReader::new(input), size, metadata, key, nonce_prefix, output);
    if result.is_err() {
        let _ = std::fs::remove_file(target);
    }
    result
}

/// Why a file couldn't be decrypted.
#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
pub enum DecryptError {
    NotEncrypted,
    WrongKey,
    Damaged,
}

/// Decrypts what `encrypt` wrote, streaming, checking what the page checks:
/// every record's tag, the final record, and the metadata's size. Only the
/// tests need it (the page decrypts in the browser).
#[cfg(test)]
pub fn decrypt(mut reader: impl Read, key_for: impl FnOnce(&[u8; HEADER_LENGTH]) -> [u8; 32], mut writer: impl Write) -> Result<Metadata, DecryptError> {
    let mut header = [0u8; HEADER_LENGTH];
    reader.read_exact(&mut header).map_err(|_| DecryptError::NotEncrypted)?;
    if header[0..4] != MAGIC || header[4] != 1 || u32::from_be_bytes(header[34..38].try_into().unwrap()) as usize != RECORD_SIZE {
        return Err(DecryptError::NotEncrypted);
    }
    let metadata_length = u32::from_be_bytes(header[38..42].try_into().unwrap()) as usize;
    if !(TAG_LENGTH..=TAG_LENGTH + 65_536).contains(&metadata_length) {
        return Err(DecryptError::Damaged);
    }
    let cipher = Aes256Gcm::new(&key_for(&header).into());
    let open = |index: u32, last: bool, sealed: &[u8]| {
        cipher.decrypt(&Nonce::from(nonce(&header, index)), Payload { msg: sealed, aad: &aad(&header, last) })
    };
    let mut sealed = vec![0u8; metadata_length];
    reader.read_exact(&mut sealed).map_err(|_| DecryptError::Damaged)?;
    let metadata: Metadata = serde_json::from_slice(&open(0, false, &sealed).map_err(|_| DecryptError::WrongKey)?).map_err(|_| DecryptError::Damaged)?;
    // Each record is read with one byte past it, to know whether it's the last.
    let mut record = vec![0u8; RECORD_SIZE + TAG_LENGTH];
    let mut filled = fill(&mut reader, &mut record)?;
    let mut written = 0u64;
    let mut index = 1u32;
    loop {
        let mut next = [0u8; 1];
        let more = filled == record.len() && fill(&mut reader, &mut next)? == 1;
        let plain = open(index, !more, &record[..filled]).map_err(|_| DecryptError::Damaged)?;
        writer.write_all(&plain).map_err(|_| DecryptError::Damaged)?;
        written += plain.len() as u64;
        if !more {
            break;
        }
        record[0] = next[0];
        filled = 1 + fill(&mut reader, &mut record[1..])?;
        index += 1;
    }
    if written != metadata.size {
        return Err(DecryptError::Damaged);
    }
    Ok(metadata)
}

#[cfg(test)]
fn fill(reader: &mut impl Read, buffer: &mut [u8]) -> Result<usize, DecryptError> {
    let mut filled = 0;
    while filled < buffer.len() {
        match reader.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(_) => return Err(DecryptError::Damaged),
        }
    }
    Ok(filled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Digest;

    const VECTORS: &str = include_str!("../../tests/fixtures/share-page-vectors.json");

    fn pattern(size: usize) -> Vec<u8> {
        (0..size).map(|i| (i % 251) as u8).collect()
    }

    fn hex_bytes(text: &str) -> Vec<u8> {
        from_hex(text).unwrap()
    }

    fn sha256(data: &[u8]) -> String {
        crate::util::hex(&sha2::Sha256::digest(data))
    }

    fn sealed(plain: &[u8], metadata: &Metadata, key: &FileKey, prefix: [u8; 8]) -> Vec<u8> {
        let mut out = Vec::new();
        let length = encrypt(plain, plain.len() as u64, metadata, key, prefix, &mut out).unwrap();
        assert_eq!(length, out.len() as u64);
        out
    }

    #[test]
    fn reproduces_the_link_vectors() {
        let vectors: serde_json::Value = serde_json::from_str(VECTORS).unwrap();
        let key = FileKey { key: hex_bytes(vectors["rawKey"].as_str().unwrap()).try_into().unwrap(), kdf: Kdf::Link };
        let prefix: [u8; 8] = hex_bytes(vectors["noncePrefix"].as_str().unwrap()).try_into().unwrap();
        let cases = vectors["link"].as_array().unwrap();
        assert_eq!(cases.len(), 6);
        for case in cases {
            let size = case["size"].as_u64().unwrap() as usize;
            let metadata: Metadata = serde_json::from_str(case["meta"].as_str().unwrap()).unwrap();
            assert_eq!(metadata.json(), case["meta"].as_str().unwrap().as_bytes());
            let plain = pattern(size);
            let out = sealed(&plain, &metadata, &key, prefix);
            assert_eq!(out.len() as u64, case["encryptedLength"].as_u64().unwrap(), "size {size}");
            assert_eq!(out.len() as u64, encrypted_size(size as u64, metadata.json().len()));
            assert_eq!(sha256(&out), case["sha256"].as_str().unwrap(), "size {size}");
            if let Some(hex) = case["hex"].as_str() {
                assert_eq!(crate::util::hex(&out), hex);
            }
            let mut opened = Vec::new();
            assert_eq!(decrypt(out.as_slice(), |_| key.key, &mut opened), Ok(metadata.clone()));
            assert_eq!(opened, plain);
            // Cut short (at a record boundary or inside one) or altered:
            // never accepted.
            for cut in [out.len() - 16, out.len() - 1, HEADER_LENGTH + metadata.json().len() + 16] {
                if cut > HEADER_LENGTH && cut < out.len() {
                    assert!(decrypt(&out[..cut], |_| key.key, std::io::sink()).is_err(), "cut {cut} of {size}");
                }
            }
            let mut flipped = out.clone();
            let at = flipped.len() - 3;
            flipped[at] ^= 1;
            assert!(decrypt(flipped.as_slice(), |_| key.key, std::io::sink()).is_err());
        }
        assert_eq!(key.link("https://x/e/a.html"), format!("https://x/e/a.html#k={}", base64url(&key.key)));
    }

    #[test]
    fn reproduces_the_password_vector() {
        let vectors: serde_json::Value = serde_json::from_str(VECTORS).unwrap();
        let case = &vectors["password"];
        let prefix: [u8; 8] = hex_bytes(vectors["noncePrefix"].as_str().unwrap()).try_into().unwrap();
        let salt: [u8; 16] = hex_bytes(case["salt"].as_str().unwrap()).try_into().unwrap();
        let password = case["password"].as_str().unwrap();
        let key = FileKey::with_salt(password, salt, case["iterations"].as_u64().unwrap() as u32);
        assert_eq!(crate::util::hex(&key.key), case["derivedKey"].as_str().unwrap());
        let metadata: Metadata = serde_json::from_str(case["meta"].as_str().unwrap()).unwrap();
        assert_eq!(metadata.json(), case["meta"].as_str().unwrap().as_bytes());
        let plain = pattern(case["size"].as_u64().unwrap() as usize);
        let out = sealed(&plain, &metadata, &key, prefix);
        assert_eq!(out.len() as u64, case["encryptedLength"].as_u64().unwrap());
        assert_eq!(sha256(&out), case["sha256"].as_str().unwrap());
        let mut opened = Vec::new();
        let derived = |header: &[u8; HEADER_LENGTH]| {
            assert_eq!(header[5], 1);
            password_key(password, &header[10..26], u32::from_be_bytes(header[6..10].try_into().unwrap()))
        };
        assert_eq!(decrypt(out.as_slice(), derived, &mut opened), Ok(metadata));
        assert_eq!(opened, plain);
        assert_eq!(decrypt(out.as_slice(), |header| password_key("wrong", &header[10..26], 600_000), std::io::sink()), Err(DecryptError::WrongKey));
        // A password opens the page itself: no key in the link.
        assert_eq!(key.link("https://x/e/a.html"), "https://x/e/a.html");
    }

    #[test]
    fn normalizes_passwords() {
        let salt = [0x5a; 16];
        assert_eq!(password_key("caf\u{e9}", &salt, 1000), password_key("cafe\u{301}", &salt, 1000));
        assert_ne!(password_key("cafe", &salt, 1000), password_key("caf\u{e9}", &salt, 1000));
    }

    #[test]
    fn wrong_keys_and_stored_keys() {
        let key = FileKey::random();
        let metadata = Metadata { name: "a".into(), mime_type: "text/plain".into(), size: 10, syntax: None };
        let out = sealed(&pattern(10), &metadata, &key, new_nonce_prefix());
        assert_eq!(decrypt(out.as_slice(), |_| [7u8; 32], std::io::sink()), Err(DecryptError::WrongKey));
        assert_eq!(decrypt(&b"not a file at all, longer than a header....."[..], |_| key.key, std::io::sink()), Err(DecryptError::NotEncrypted));
        // Round trip through what history keeps.
        assert_eq!(FileKey::from_stored(&key.encoded_key(), None), Some(key.clone()));
        let password = FileKey::with_salt("pw", [1; 16], PASSWORD_ITERATIONS);
        let stored = FileKey::from_stored(&password.encoded_key(), password.encoded_salt().as_deref()).unwrap();
        assert_eq!(stored, password);
        assert!(stored.is_password());
        assert_eq!(FileKey::from_stored("short", None), None);
        // The metadata must say the file's size.
        assert!(encrypt(&b"abc"[..], 3, &metadata, &key, [0; 8], std::io::sink()).is_err());
    }

    #[test]
    fn encrypts_files_streaming() {
        let directory = std::env::temp_dir().join(format!("aktar-encrypt-{}", crate::util::new_id()));
        std::fs::create_dir_all(&directory).unwrap();
        let source = directory.join("plain");
        let plain = pattern(3 * RECORD_SIZE + 17);
        std::fs::write(&source, &plain).unwrap();
        let key = FileKey::random();
        let metadata = Metadata { name: "x.bin".into(), mime_type: "application/octet-stream".into(), size: plain.len() as u64, syntax: None };
        let target = directory.join("sealed");
        let length = encrypt_file(&source, &target, &metadata, &key, new_nonce_prefix()).unwrap();
        let out = std::fs::read(&target).unwrap();
        assert_eq!(length, out.len() as u64);
        let mut opened = Vec::new();
        decrypt(out.as_slice(), |_| key.key, &mut opened).unwrap();
        assert_eq!(opened, plain);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
