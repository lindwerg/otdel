//! Secret handling: opaque session tokens, token fingerprints, password hashing.
//!
//! Rules enforced here:
//!
//! * session and CSRF tokens are opaque random values (no user data, no signatures to
//!   forge) produced by the operating system CSPRNG;
//! * the database stores only a SHA-256 fingerprint of the session token, so a database
//!   dump does not hand out live sessions;
//! * passwords are verified with Argon2id via the PHC string format;
//! * obviously-unset/example secrets are recognised so the server can refuse to start.

use argon2::password_hash::rand_core::{OsRng, RngCore};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::error::AppError;

/// 32 bytes of entropy per token.
pub const TOKEN_BYTES: usize = 32;

/// Minimum length accepted for the local owner password.
pub const PASSWORD_MIN_CHARS: usize = 12;

/// Generate an opaque, URL-safe token (43 characters of base64url).
pub fn generate_token() -> String {
    let mut bytes = [0u8; TOKEN_BYTES];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Fingerprint stored in the database instead of the token itself.
pub fn token_fingerprint(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hex_lower(&hasher.finalize())
}

/// Constant-time comparison for tokens supplied by the client (CSRF header).
pub fn tokens_match(expected: &str, provided: &str) -> bool {
    let expected = expected.as_bytes();
    let provided = provided.as_bytes();
    if expected.len() != provided.len() {
        // Length is not secret for fixed-size tokens; this only avoids a panic.
        return false;
    }
    expected.ct_eq(provided).into()
}

/// Hash a password with Argon2id (default OWASP-ish parameters of the `argon2` crate).
pub fn hash_password(password: &str) -> Result<String, AppError> {
    if password.chars().count() < PASSWORD_MIN_CHARS {
        return Err(AppError::validation(format!(
            "password must be at least {PASSWORD_MIN_CHARS} characters"
        )));
    }
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|_| AppError::internal("could not hash the password"))
}

/// Verify a password against a stored PHC hash. Any malformed hash verifies as `false`.
pub fn verify_password(phc_hash: &str, password: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(phc_hash) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

/// Whether a string is a valid Argon2 PHC hash this server can verify against.
pub fn is_supported_password_hash(value: &str) -> bool {
    match PasswordHash::new(value) {
        Ok(parsed) => parsed.algorithm.as_str().starts_with("argon2"),
        Err(_) => false,
    }
}

/// Recognise values that are clearly placeholders rather than real secrets.
///
/// Used by configuration loading so the server refuses to start with an example value
/// copied from `.env.example` instead of silently running with a known secret.
pub fn looks_like_placeholder(value: &str) -> bool {
    let normalised = value.trim().to_ascii_lowercase();
    if normalised.is_empty() {
        return true;
    }
    const MARKERS: [&str; 8] = [
        "changeme",
        "change-me",
        "placeholder",
        "example",
        "your-",
        "secret-here",
        "todo",
        "xxxxx",
    ];
    MARKERS.iter().any(|marker| normalised.contains(marker))
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_unique_and_opaque() {
        let a = generate_token();
        let b = generate_token();
        assert_ne!(a, b);
        assert_eq!(a.len(), 43);
        assert!(a
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn fingerprint_is_stable_and_not_the_token() {
        let token = generate_token();
        let fingerprint = token_fingerprint(&token);
        assert_eq!(fingerprint, token_fingerprint(&token));
        assert_eq!(fingerprint.len(), 64);
        assert!(!fingerprint.contains(&token));
        assert_ne!(fingerprint, token_fingerprint(&generate_token()));
    }

    #[test]
    fn password_round_trip() {
        let hash = hash_password("correct horse battery").unwrap();
        assert!(is_supported_password_hash(&hash));
        assert!(verify_password(&hash, "correct horse battery"));
        assert!(!verify_password(&hash, "correct horse batteryy"));
        assert!(!verify_password("not-a-hash", "correct horse battery"));
        assert!(hash_password("short").is_err());
    }

    #[test]
    fn token_comparison_rejects_mismatches() {
        let token = generate_token();
        assert!(tokens_match(&token, &token.clone()));
        assert!(!tokens_match(&token, "x"));
        assert!(!tokens_match(&token, &generate_token()));
    }

    #[test]
    fn placeholders_are_recognised() {
        assert!(looks_like_placeholder(""));
        assert!(looks_like_placeholder("   "));
        assert!(looks_like_placeholder("changeme"));
        assert!(looks_like_placeholder(
            "postgres://otdel:changeme@localhost:5432/otdel_dev"
        ));
        assert!(!looks_like_placeholder(
            "postgres://otdel_app:9f3c@127.0.0.1:58432/otdel"
        ));
    }
}
