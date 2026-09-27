use std::fmt;
use std::io::Read;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};

const KEYRING_SERVICE: &str = "clipway";
const KEYRING_USER: &str = "clipway-database-key";

/// How long to keep retrying while the login keyring is locked or not yet
/// available (for example right after an automatic login).
const KEYRING_WAIT: Duration = Duration::from_secs(90);
const KEYRING_RETRY: Duration = Duration::from_secs(3);

/// A history database exists but the keyring holds no key for it. A new key
/// is never generated in this case: that would make the existing history
/// permanently unreadable without telling anyone.
#[derive(Debug)]
pub struct KeyMissing;

impl fmt::Display for KeyMissing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the history database exists but its key is missing from the login keyring")
    }
}

impl std::error::Error for KeyMissing {}

pub fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

pub fn hex_decode(text: &str) -> Result<Vec<u8>> {
    let text = text.trim();
    anyhow::ensure!(text.len().is_multiple_of(2), "hex string has odd length");
    anyhow::ensure!(
        text.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "hex string contains non-hex characters"
    );
    (0..text.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&text[index..index + 2], 16)
                .with_context(|| format!("invalid hex byte at offset {index}"))
        })
        .collect()
}

fn entry() -> Result<keyring::Entry> {
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER).map_err(|e| anyhow!("keyring: {e}"))
}

/// Returns the database key. A key is generated only when no database exists
/// yet; if the database exists and the key is gone, this fails with
/// [`KeyMissing`].
pub fn database_key(database_exists: bool) -> Result<Vec<u8>> {
    // Development/test override only: a key in the environment is readable
    // through /proc/<pid>/environ and must never be used by release builds.
    #[cfg(any(test, debug_assertions))]
    if let Ok(hex) = std::env::var("CLIPWAY_DB_KEY") {
        return hex_decode(&hex);
    }

    let entry = entry()?;
    let started = Instant::now();
    loop {
        match entry.get_password() {
            Ok(hex) => return hex_decode(&hex),
            Err(keyring::Error::NoEntry) if database_exists => {
                return Err(anyhow::Error::new(KeyMissing));
            }
            Err(keyring::Error::NoEntry) => return store_new_key(&entry),
            Err(error) if started.elapsed() < KEYRING_WAIT => {
                eprintln!("clipway: login keyring not ready ({error}); retrying");
                std::thread::sleep(KEYRING_RETRY);
            }
            Err(error) => return Err(anyhow!("reading database key from keyring: {error}")),
        }
    }
}

/// Replaces the stored key with a fresh one. Used only after the old
/// database has been moved aside.
pub fn reset_key() -> Result<Vec<u8>> {
    let entry = entry()?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => {}
        Err(error) => return Err(anyhow!("removing old key from keyring: {error}")),
    }
    store_new_key(&entry)
}

fn store_new_key(entry: &keyring::Entry) -> Result<Vec<u8>> {
    let key = random_bytes(32)?;
    entry
        .set_password(&hex_encode(&key))
        .map_err(|e| anyhow!("storing database key in keyring: {e}"))?;
    Ok(key)
}

fn random_bytes(count: usize) -> Result<Vec<u8>> {
    let mut buffer = vec![0u8; count];
    let mut file =
        std::fs::File::open("/dev/urandom").context("opening /dev/urandom for key material")?;
    file.read_exact(&mut buffer)
        .context("reading key material from /dev/urandom")?;
    Ok(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_roundtrip() {
        let bytes = [0x00, 0x0f, 0xa5, 0xff];
        assert_eq!(hex_encode(&bytes), "000fa5ff");
        assert_eq!(hex_decode("000fa5ff").unwrap(), bytes);
    }

    #[test]
    fn hex_rejects_invalid_input() {
        assert!(hex_decode("abc").is_err());
        assert!(hex_decode("zz").is_err());
    }

    #[test]
    fn env_key_override_is_used_in_test_builds() {
        unsafe { std::env::set_var("CLIPWAY_DB_KEY", "00112233445566778899aabbccddeeff") };
        let key = database_key(true).unwrap();
        assert_eq!(key.len(), 16);
        unsafe { std::env::remove_var("CLIPWAY_DB_KEY") };
    }
}
