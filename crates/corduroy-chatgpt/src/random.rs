//! Random values for a sign-in, and the PKCE challenge.

use std::fmt::Write as _;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::rand::{SecureRandom as _, SystemRandom};

fn fill(bytes: &mut [u8]) -> Result<(), String> {
    SystemRandom::new()
        .fill(bytes)
        .map_err(|_| "the system gave no random bytes".to_owned())
}

/// 32 random bytes as URL-safe text: a `state`, a `nonce` or a PKCE verifier.
///
/// # Errors
///
/// If the system gives no random bytes.
pub fn text() -> Result<String, String> {
    let mut bytes = [0_u8; 32];
    fill(&mut bytes)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// The PKCE challenge of `verifier`: its SHA-256, base64url without padding.
#[must_use]
pub fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(ring::digest::digest(
        &ring::digest::SHA256,
        verifier.as_bytes(),
    ))
}

/// A random (version 4) UUID as a `urn:uuid:` URI: what names one host.
///
/// # Errors
///
/// If the system gives no random bytes.
pub fn host_id() -> Result<String, String> {
    let mut bytes = [0_u8; 16];
    fill(&mut bytes)?;
    let mut id = String::from("urn:uuid:");
    for (at, byte) in bytes.into_iter().enumerate() {
        let byte = match at {
            6 => byte & 0x0f | 0x40,
            8 => byte & 0x3f | 0x80,
            _ => byte,
        };
        if matches!(at, 4 | 6 | 8 | 10) {
            id.push('-');
        }
        let _written = write!(id, "{byte:02x}");
    }
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_challenge_is_the_one_the_specification_gives() {
        // RFC 7636, appendix B.
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn a_host_id_is_a_version_4_uuid_and_no_two_are_alike() {
        let id = host_id().unwrap();
        let uuid = id.strip_prefix("urn:uuid:").unwrap();
        let groups: Vec<usize> = uuid.split('-').map(str::len).collect();
        assert_eq!(groups, [8, 4, 4, 4, 12]);
        assert_eq!(uuid.chars().nth(14), Some('4'));
        assert!(matches!(uuid.chars().nth(19), Some('8' | '9' | 'a' | 'b')));
        assert_ne!(id, host_id().unwrap());
        assert_ne!(text().unwrap(), text().unwrap());
    }
}
