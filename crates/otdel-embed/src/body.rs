//! Reading a response body without trusting what it says about its own size.
//!
//! Split out of [`super::openai`] because the property that matters here is testable
//! without a network and should stay that way: the bytes never exceed the ceiling, and
//! the refusal happens **before** any parsing.
//!
//! `Content-Length` is deliberately not consulted. A chunked response does not carry one,
//! and a hostile or broken endpoint can declare one it does not honour — either way,
//! calling `bytes()` after checking a header buffers whatever really arrives. Counting
//! the bytes as they come is the only bound that holds.

use crate::provider::{EmbedError, EmbedResult};

/// Read a body, stopping the moment it exceeds `limit`.
///
/// Streaming rather than `bytes()`, which buffers whatever arrives regardless of any
/// declared length — see the module note on `Content-Length`.
pub(crate) async fn read_bounded(
    mut response: reqwest::Response,
    limit: u64,
) -> EmbedResult<Vec<u8>> {
    let mut body = BoundedBody::new(limit);
    loop {
        let chunk = response.chunk().await.map_err(|error| {
            if error.is_timeout() {
                EmbedError::Timeout
            } else {
                EmbedError::Transport
            }
        })?;
        let Some(chunk) = chunk else { break };
        body.push(&chunk)?;
    }
    Ok(body.into_bytes())
}

/// An accumulator that refuses to grow past its ceiling.
///
/// Separate from the request so the ceiling itself is testable without a network: the
/// property that matters is that the bytes never exceed the limit *and* that the refusal
/// happens before any parsing.
#[derive(Debug)]
struct BoundedBody {
    limit: u64,
    bytes: Vec<u8>,
}

impl BoundedBody {
    fn new(limit: u64) -> Self {
        Self {
            limit,
            bytes: Vec::with_capacity(8 * 1024),
        }
    }

    fn push(&mut self, chunk: &[u8]) -> EmbedResult<()> {
        if self.bytes.len() as u64 + chunk.len() as u64 > self.limit {
            return Err(EmbedError::InvalidResponse(format!(
                "ответ больше разрешённых {} байт",
                self.limit
            )));
        }
        self.bytes.extend_from_slice(chunk);
        Ok(())
    }

    fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_oversized_body_is_refused_before_anything_is_parsed() {
        let mut body = BoundedBody::new(16);
        body.push(b"{\"data\":").unwrap();
        let error = body.push(&[b'x'; 64]).unwrap_err();
        assert!(matches!(error, EmbedError::InvalidResponse(_)), "{error:?}");
        assert!(
            error.diagnostic().contains("16"),
            "the refusal must name the ceiling: {}",
            error.diagnostic()
        );
        // What was kept is still under the ceiling: the oversized chunk was dropped, not
        // appended and then measured.
        assert!(body.into_bytes().len() <= 16);
    }

    #[test]
    fn a_body_within_the_ceiling_is_kept_whole() {
        let mut body = BoundedBody::new(8);
        body.push(b"1234").unwrap();
        body.push(b"5678").unwrap();
        assert_eq!(body.into_bytes(), b"12345678");
    }
}
