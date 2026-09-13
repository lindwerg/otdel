//! Object keys: the only thing that decides where bytes are written.
//!
//! A key is built from identifiers the server controls (bureau id, partner id) plus the
//! SHA-256 of the content. No part of it comes from a client-supplied file name, so
//! path traversal is impossible by construction — and [`ObjectKey::parse`] re-validates
//! keys read back from the database before they are ever turned into a path.

use std::path::{Component, Path, PathBuf};

use uuid::Uuid;

use crate::error::StorageError;

/// Tenant namespace of an object: bureau (workspace) and partner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectNamespace {
    pub bureau_id: Uuid,
    pub partner_id: Uuid,
}

impl ObjectNamespace {
    pub fn new(bureau_id: Uuid, partner_id: Uuid) -> Self {
        Self {
            bureau_id,
            partner_id,
        }
    }

    /// `bureau-<uuid>/partner-<uuid>/<first two hex chars>/<sha256>`
    pub fn key_for_digest(&self, sha256_hex: &str) -> Result<ObjectKey, StorageError> {
        if !is_sha256_hex(sha256_hex) {
            return Err(StorageError::InvalidKey(
                "content digest is not a 64-character lowercase hex string".to_owned(),
            ));
        }
        ObjectKey::parse(&format!(
            "bureau-{}/partner-{}/{}/{}",
            self.bureau_id,
            self.partner_id,
            &sha256_hex[..2],
            sha256_hex
        ))
    }
}

/// A validated storage key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ObjectKey(String);

impl ObjectKey {
    /// Parse and fully validate a key. Every component is checked against a fixed shape;
    /// nothing else is accepted (no `..`, no absolute paths, no separators in segments).
    pub fn parse(raw: &str) -> Result<Self, StorageError> {
        let parts: Vec<&str> = raw.split('/').collect();
        if parts.len() != 4 {
            return Err(StorageError::InvalidKey(
                "key must have exactly four segments".to_owned(),
            ));
        }

        let bureau = parts[0].strip_prefix("bureau-").ok_or_else(|| {
            StorageError::InvalidKey("first segment must be `bureau-<uuid>`".into())
        })?;
        Uuid::parse_str(bureau)
            .map_err(|_| StorageError::InvalidKey("bureau segment is not a UUID".into()))?;

        let partner = parts[1].strip_prefix("partner-").ok_or_else(|| {
            StorageError::InvalidKey("second segment must be `partner-<uuid>`".into())
        })?;
        Uuid::parse_str(partner)
            .map_err(|_| StorageError::InvalidKey("partner segment is not a UUID".into()))?;

        if parts[2].len() != 2 || !parts[2].bytes().all(is_lower_hex) {
            return Err(StorageError::InvalidKey(
                "third segment must be two lowercase hex characters".to_owned(),
            ));
        }
        if !is_sha256_hex(parts[3]) {
            return Err(StorageError::InvalidKey(
                "fourth segment must be a 64-character lowercase hex digest".to_owned(),
            ));
        }
        if parts[2] != &parts[3][..2] {
            return Err(StorageError::InvalidKey(
                "shard segment does not match the digest".to_owned(),
            ));
        }

        Ok(Self(raw.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn digest(&self) -> &str {
        self.0.rsplit('/').next().unwrap_or_default()
    }

    /// Namespace this key belongs to, recovered from the key itself.
    ///
    /// Used by maintenance to decide which bureau/partner an object on disk claims to
    /// belong to; the claim is then checked against the database.
    pub fn namespace(&self) -> Result<ObjectNamespace, StorageError> {
        let mut parts = self.0.split('/');
        let bureau = parts
            .next()
            .and_then(|part| part.strip_prefix("bureau-"))
            .and_then(|value| Uuid::parse_str(value).ok())
            .ok_or_else(|| StorageError::InvalidKey("missing bureau segment".to_owned()))?;
        let partner = parts
            .next()
            .and_then(|part| part.strip_prefix("partner-"))
            .and_then(|value| Uuid::parse_str(value).ok())
            .ok_or_else(|| StorageError::InvalidKey("missing partner segment".to_owned()))?;
        Ok(ObjectNamespace::new(bureau, partner))
    }

    /// Resolve the key against `root`, rejecting anything that is not strictly inside it.
    ///
    /// Belt and braces: the key shape already excludes traversal, and this check also
    /// rejects `..`/absolute components should a key ever be constructed another way.
    pub fn resolve_within(&self, root: &Path) -> Result<PathBuf, StorageError> {
        let mut path = root.to_path_buf();
        for segment in self.0.split('/') {
            let candidate = Path::new(segment);
            let mut components = candidate.components();
            match (components.next(), components.next()) {
                (Some(Component::Normal(part)), None) => path.push(part),
                _ => {
                    return Err(StorageError::InvalidKey(
                        "key segment is not a plain path component".to_owned(),
                    ))
                }
            }
        }
        if !path.starts_with(root) {
            return Err(StorageError::InvalidKey(
                "resolved path escapes the storage root".to_owned(),
            ));
        }
        Ok(path)
    }
}

impl std::fmt::Display for ObjectKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn is_lower_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(is_lower_hex)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

    fn namespace() -> ObjectNamespace {
        ObjectNamespace::new(Uuid::from_u128(1), Uuid::from_u128(2))
    }

    #[test]
    fn key_is_derived_from_namespace_and_digest() {
        let key = namespace().key_for_digest(DIGEST).unwrap();
        assert_eq!(
            key.as_str(),
            format!(
                "bureau-{}/partner-{}/9f/{DIGEST}",
                Uuid::from_u128(1),
                Uuid::from_u128(2)
            )
        );
        assert_eq!(key.digest(), DIGEST);
    }

    #[test]
    fn namespace_round_trips_through_the_key() {
        let namespace = namespace();
        let key = namespace.key_for_digest(DIGEST).unwrap();
        assert_eq!(key.namespace().unwrap(), namespace);
    }

    #[test]
    fn invalid_digests_are_refused() {
        assert!(namespace().key_for_digest("../../etc/passwd").is_err());
        assert!(namespace().key_for_digest(&DIGEST.to_uppercase()).is_err());
        assert!(namespace().key_for_digest("abc").is_err());
    }

    #[test]
    fn traversal_keys_never_parse() {
        for raw in [
            "../../../etc/passwd",
            "bureau-x/partner-y/aa/bb",
            &format!("bureau-{}/partner-{}/../{DIGEST}", Uuid::nil(), Uuid::nil()),
            &format!(
                "bureau-{}/partner-{}/9f/{}",
                Uuid::nil(),
                Uuid::nil(),
                "z".repeat(64)
            ),
            &format!(
                "/bureau-{}/partner-{}/9f/{DIGEST}",
                Uuid::nil(),
                Uuid::nil()
            ),
            &format!("bureau-{}/partner-{}/ab/{DIGEST}", Uuid::nil(), Uuid::nil()),
        ] {
            assert!(
                ObjectKey::parse(raw).is_err(),
                "key must be rejected: {raw}"
            );
        }
    }

    #[test]
    fn resolution_stays_inside_the_root() {
        let key = namespace().key_for_digest(DIGEST).unwrap();
        let root = Path::new("/tmp/otdel-storage/objects");
        let resolved = key.resolve_within(root).unwrap();
        assert!(resolved.starts_with(root));
        assert!(resolved.ends_with(DIGEST));
    }
}
