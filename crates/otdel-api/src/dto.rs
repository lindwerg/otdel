//! Request and response bodies.
//!
//! These types are the wire contract (`docs/implementation-contract.md`). Unknown fields
//! are rejected so a typo in a client does not silently do nothing.

use serde::{Deserialize, Deserializer, Serialize};

/// `{ "items": [...] }` — the list shape used by every collection endpoint.
#[derive(Debug, Serialize)]
pub struct ItemsResponse<T> {
    pub items: Vec<T>,
}

impl<T> ItemsResponse<T> {
    pub fn new(items: Vec<T>) -> Self {
        Self { items }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoginRequest {
    pub password: String,
}

#[derive(Debug, Serialize)]
pub struct SessionResponse {
    pub authenticated: bool,
    pub csrf_token: String,
}

#[derive(Debug, Serialize)]
pub struct SessionEndedResponse {
    pub authenticated: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreatePartnerRequest {
    pub name: String,
    #[serde(default)]
    pub note: Option<String>,
}

/// `PATCH` body. `note` distinguishes three cases: absent (leave as is), `null` (clear)
/// and a string (replace) — hence the nested `Option`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchPartnerRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, deserialize_with = "present_option")]
    pub note: Option<Option<String>>,
}

fn present_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// Readiness payload. Every dependency is reported explicitly, including the state of
/// pgvector, which phase 1A does not use yet and therefore does not claim to have.
#[derive(Debug, Serialize)]
pub struct ReadyResponse {
    pub status: &'static str,
    pub checks: ReadyChecks,
}

#[derive(Debug, Serialize)]
pub struct ReadyChecks {
    pub database: &'static str,
    pub object_store: &'static str,
    pub bureau: &'static str,
    pub pgvector: &'static str,
}

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_distinguishes_absent_from_null_note() {
        let absent: PatchPartnerRequest = serde_json::from_str(r#"{"name":"BASIS"}"#).unwrap();
        assert_eq!(absent.name.as_deref(), Some("BASIS"));
        assert!(absent.note.is_none());

        let cleared: PatchPartnerRequest = serde_json::from_str(r#"{"note":null}"#).unwrap();
        assert_eq!(cleared.note, Some(None));

        let replaced: PatchPartnerRequest = serde_json::from_str(r#"{"note":"hi"}"#).unwrap();
        assert_eq!(replaced.note, Some(Some("hi".to_owned())));
    }

    #[test]
    fn unknown_fields_are_rejected() {
        assert!(serde_json::from_str::<CreatePartnerRequest>(r#"{"name":"a","x":1}"#).is_err());
        assert!(
            serde_json::from_str::<LoginRequest>(r#"{"password":"a","role":"admin"}"#).is_err()
        );
    }
}
