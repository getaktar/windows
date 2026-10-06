//! Clean URLs: the `{short}` path token, and writing a key with it without
//! replacing a file that already has it. Amazon S3 and Cloudflare R2 refuse
//! the write themselves when the key is taken (`If-None-Match: *`, on the
//! PUT or on completing a multipart upload), so nothing can slip in
//! between; other providers are asked first, which is practically as safe
//! at 62^7 codes but not guaranteed. A taken key gets a whole new key with
//! a new code, never a number, and after `MAX_SHORT_KEY_ATTEMPTS` the
//! upload fails instead of replacing anything. Ported from the Mac app's
//! ShortCode and ShortKeys (docs/short-links.md there).

use rand::RngCore;

use crate::storage::StorageError;

/// Keys tried before an upload with `{short}` gives up.
pub const MAX_SHORT_KEY_ATTEMPTS: usize = 5;
/// The code's length: 62^7, about 3.5 trillion codes.
pub const SHORT_CODE_LENGTH: usize = 7;
pub const SHORT_CODE_ALPHABET: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
/// Bytes from here up are drawn again: 248 is the largest multiple of 62
/// that fits in a byte, so `value % 62` favors no character.
const REJECT_FROM: u8 = 248;

/// A new `{short}` code from the system's secure random generator.
pub fn short_code() -> String {
    short_code_from(|bytes| rand::rngs::OsRng.fill_bytes(bytes))
}

/// A code from the random bytes `fill` writes; tests pass their own.
pub fn short_code_from(mut fill: impl FnMut(&mut [u8])) -> String {
    let mut code = String::with_capacity(SHORT_CODE_LENGTH);
    let mut bytes = [0u8; SHORT_CODE_LENGTH];
    while code.len() < SHORT_CODE_LENGTH {
        fill(&mut bytes);
        for byte in bytes {
            if byte < REJECT_FROM && code.len() < SHORT_CODE_LENGTH {
                code.push(SHORT_CODE_ALPHABET[(byte % 62) as usize] as char);
            }
        }
    }
    code
}

/// Where a key with `{short}` goes: the bucket, or a fake one in tests.
pub trait ShortKeyTarget {
    /// Whether something is at `key` already.
    async fn exists(&mut self, key: &str) -> Result<bool, StorageError>;
    /// Writes the file to `key`; with `only_if_new`, only if nothing is
    /// there, answering `StorageError::AlreadyExists` when something is.
    async fn write(&mut self, key: &str, only_if_new: bool) -> Result<(), StorageError>;
}

/// Writes to `first_key`, then to keys from `new_key` while the key is
/// taken, and returns the key written. With `conditional` every write is
/// only-if-new; without it each key is asked about first (a key that may
/// not list the bucket writes without asking, as before).
pub async fn write_short_key<T: ShortKeyTarget>(
    target: &mut T,
    first_key: String,
    conditional: bool,
    mut new_key: impl FnMut() -> String,
) -> Result<String, StorageError> {
    let mut key = first_key;
    for attempt in 0..MAX_SHORT_KEY_ATTEMPTS {
        if attempt > 0 {
            key = new_key();
        }
        if !conditional {
            let taken = match target.exists(&key).await {
                Ok(taken) => taken,
                Err(StorageError::AccessDenied) => false,
                Err(error) => return Err(error),
            };
            if taken {
                continue;
            }
        }
        match target.write(&key, conditional).await {
            Ok(()) => return Ok(key),
            Err(StorageError::AlreadyExists) => continue,
            Err(error) => return Err(error),
        }
    }
    Err(StorageError::NoFreeShortKey)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap, VecDeque};

    use chrono::Local;

    use super::*;
    use crate::output::{generate_key_with, KeyPlace};

    #[test]
    fn codes_are_seven_base62_characters() {
        let alphabet: std::collections::HashSet<u8> = SHORT_CODE_ALPHABET.iter().copied().collect();
        assert_eq!(alphabet.len(), 62);
        for _ in 0..1_000 {
            let code = short_code();
            assert_eq!(code.len(), SHORT_CODE_LENGTH);
            assert!(code.bytes().all(|byte| alphabet.contains(&byte)), "{code}");
        }
    }

    #[test]
    fn codes_are_roughly_uniform() {
        let mut counts: HashMap<char, usize> = HashMap::new();
        let codes = 20_000;
        for _ in 0..codes {
            for character in short_code().chars() {
                *counts.entry(character).or_default() += 1;
            }
        }
        assert_eq!(counts.len(), 62);
        let expected = (codes * SHORT_CODE_LENGTH) as f64 / 62.0;
        for (character, count) in counts {
            assert!((count as f64 - expected).abs() <= expected * 0.2, "{character}: {count}");
        }
    }

    #[test]
    fn bytes_from_248_up_are_drawn_again() {
        let mut batches = VecDeque::from([[248, 255, 0, 61, 62, 247, 250], [1, 2, 3, 4, 5, 6, 7]]);
        let code = short_code_from(|bytes| bytes.copy_from_slice(&batches.pop_front().unwrap()));
        // 0, 61, 62 % 62, 247 % 62, then the next batch fills the rest.
        assert_eq!(code, "0z0z123");
    }

    /// A bucket that refuses a conditional write to a taken key, as S3 and
    /// R2 do with `If-None-Match: *`.
    struct FakeBucket {
        objects: BTreeMap<String, String>,
        writes: usize,
    }

    impl ShortKeyTarget for FakeBucket {
        async fn exists(&mut self, key: &str) -> Result<bool, StorageError> {
            Ok(self.objects.contains_key(key))
        }

        async fn write(&mut self, key: &str, only_if_new: bool) -> Result<(), StorageError> {
            self.writes += 1;
            if only_if_new && self.objects.contains_key(key) {
                return Err(StorageError::AlreadyExists);
            }
            self.objects.insert(key.to_string(), "new".into());
            Ok(())
        }
    }

    fn bucket() -> FakeBucket {
        FakeBucket { objects: BTreeMap::from([("ABC1234.png".to_string(), "old".to_string())]), writes: 0 }
    }

    /// Uploads "photo.png" with `{short}.{ext}`, the codes handed out in order.
    async fn upload(bucket: &mut FakeBucket, codes: &mut VecDeque<&str>, conditional: bool) -> Result<String, StorageError> {
        let mut make_key = || {
            let code = codes.pop_front().expect("ran out of codes").to_string();
            generate_key_with("{short}.{ext}", "photo.png", None, &KeyPlace::default(), Local::now(), || code)
        };
        let first = make_key();
        write_short_key(bucket, first, conditional, make_key).await
    }

    async fn assert_skips_taken_code(conditional: bool) {
        let mut bucket = bucket();
        let mut codes = VecDeque::from(["ABC1234", "ABC1234", "XYZ9876"]);
        let key = upload(&mut bucket, &mut codes, conditional).await.unwrap();
        assert_eq!(key, "XYZ9876.png");
        assert_eq!(bucket.objects.get("ABC1234.png").map(String::as_str), Some("old"));
        assert_eq!(bucket.objects.get("XYZ9876.png").map(String::as_str), Some("new"));
        assert_eq!(bucket.objects.len(), 2);
        assert!(codes.is_empty());
    }

    async fn assert_fails_after_five_collisions(conditional: bool) {
        let mut bucket = bucket();
        let mut codes = VecDeque::from(vec!["ABC1234"; MAX_SHORT_KEY_ATTEMPTS]);
        let result = upload(&mut bucket, &mut codes, conditional).await;
        assert!(matches!(result, Err(StorageError::NoFreeShortKey)), "{result:?}");
        assert_eq!(bucket.objects, BTreeMap::from([("ABC1234.png".to_string(), "old".to_string())]));
        assert!(codes.is_empty());
        // Only conditional writes are tried on a taken key.
        assert_eq!(bucket.writes, if conditional { MAX_SHORT_KEY_ATTEMPTS } else { 0 });
    }

    #[tokio::test]
    async fn collision_gets_a_new_code_with_conditional_writes() {
        assert_skips_taken_code(true).await;
    }

    #[tokio::test]
    async fn collision_gets_a_new_code_when_asking_first() {
        assert_skips_taken_code(false).await;
    }

    #[tokio::test]
    async fn five_collisions_fail_without_overwriting_conditional() {
        assert_fails_after_five_collisions(true).await;
    }

    #[tokio::test]
    async fn five_collisions_fail_without_overwriting_asking_first() {
        assert_fails_after_five_collisions(false).await;
    }
}
