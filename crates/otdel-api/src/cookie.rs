//! Minimal cookie handling.
//!
//! The session cookie is built by hand instead of pulling in a cookie library: the
//! attributes are fixed by the contract (`HttpOnly`, `SameSite=Strict`, `Path=/`,
//! bounded `Max-Age`, `Secure` outside the plain-HTTP local pilot) and there is exactly
//! one cookie in the whole application.

/// Build the `Set-Cookie` value for a newly opened session.
pub fn session_cookie(name: &str, token: &str, max_age_seconds: u64, secure: bool) -> String {
    let mut cookie =
        format!("{name}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age_seconds}");
    if secure {
        cookie.push_str("; Secure");
    }
    cookie
}

/// Build the `Set-Cookie` value that removes the session cookie.
pub fn cleared_cookie(name: &str, secure: bool) -> String {
    let mut cookie = format!("{name}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0");
    if secure {
        cookie.push_str("; Secure");
    }
    cookie
}

/// Read one cookie value out of a `Cookie` header.
pub fn read_cookie<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    header.split(';').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        if key.trim() == name {
            Some(value.trim())
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_cookie_has_the_contract_attributes() {
        let cookie = session_cookie("otdel_session", "abc", 3600, false);
        assert!(cookie.starts_with("otdel_session=abc;"));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Strict"));
        assert!(cookie.contains("Path=/"));
        assert!(cookie.contains("Max-Age=3600"));
        assert!(!cookie.contains("Secure"));

        assert!(session_cookie("otdel_session", "abc", 3600, true).contains("; Secure"));
        assert!(cleared_cookie("otdel_session", false).contains("Max-Age=0"));
    }

    #[test]
    fn reads_the_right_cookie_only() {
        let header = "other=1; otdel_session=token-value; trailing=2";
        assert_eq!(read_cookie(header, "otdel_session"), Some("token-value"));
        assert_eq!(read_cookie(header, "otdel"), None);
        assert_eq!(read_cookie("", "otdel_session"), None);
        assert_eq!(read_cookie("otdel_session=", "otdel_session"), Some(""));
        assert_eq!(read_cookie("malformed", "otdel_session"), None);
    }
}
