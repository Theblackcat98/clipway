use std::io::Read;

use anyhow::{Context, Result, anyhow};

const KEYRING_SERVICE: &str = "clipway";
const KEYRING_USER: &str = "clipway-database-key";

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

pub fn database_key() -> Result<Vec<u8>> {
    if let Ok(hex) = std::env::var("CLIPWAY_DB_KEY") {
        return hex_decode(&hex);
    }
    let entry =
        keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER).map_err(|e| anyhow!("keyring: {e}"))?;
    match entry.get_password() {
        Ok(hex) => hex_decode(&hex),
        Err(keyring::Error::NoEntry) => {
            let key = random_bytes(32)?;
            entry
                .set_password(&hex_encode(&key))
                .map_err(|e| anyhow!("storing database key in keyring: {e}"))?;
            Ok(key)
        }
        Err(error) => Err(anyhow!("reading database key from keyring: {error}")),
    }
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
    fn env_key_override_is_used() {
        unsafe { std::env::set_var("CLIPWAY_DB_KEY", "00112233445566778899aabbccddeeff") };
        let key = database_key().unwrap();
        assert_eq!(key.len(), 16);
        unsafe { std::env::remove_var("CLIPWAY_DB_KEY") };
    }
}
