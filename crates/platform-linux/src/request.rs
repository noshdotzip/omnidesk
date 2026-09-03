//! The XDG portal Request/Response handshake.
//!
//! Portal methods do not return their result. They return the object path of a
//! `org.freedesktop.portal.Request`, and the real answer arrives later as a `Response`
//! signal on that path. Two consequences drive this module:
//!
//! 1. **You must subscribe before you call.** The portal is free to emit `Response`
//!    before the method call returns, so a caller that calls first and subscribes
//!    second can lose the signal and hang forever.
//! 2. **You can predict the path.** The caller supplies a `handle_token`, and the
//!    request path is derived from that token plus the caller's unique bus name. This
//!    lets step 1 happen before the call, and it is pure string manipulation, so it is
//!    unit-tested here on every platform.
//!
//! Spec: <https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.Request.html>

/// Outcome codes carried by a `Response` signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseCode {
    /// The request succeeded.
    Success,
    /// The user dismissed the dialog. Not an error — a decision.
    Cancelled,
    /// Ended some other way (portal closed it, backend failed).
    Ended,
    Unknown(u32),
}

impl ResponseCode {
    pub fn from_raw(code: u32) -> Self {
        match code {
            0 => ResponseCode::Success,
            1 => ResponseCode::Cancelled,
            2 => ResponseCode::Ended,
            other => ResponseCode::Unknown(other),
        }
    }

    pub fn is_success(self) -> bool {
        matches!(self, ResponseCode::Success)
    }
}

/// Derive the object path a `Request` will appear at.
///
/// The rule from the portal specification: take the caller's unique bus name, drop the
/// leading `:`, replace every `.` with `_`, and interpolate it with the handle token
/// into `/org/freedesktop/portal/desktop/request/<name>/<token>`.
///
/// Getting this wrong does not fail loudly — it produces a path nothing ever signals
/// on, and the caller blocks until it times out. Hence the tests.
pub fn request_path(unique_bus_name: &str, handle_token: &str) -> String {
    let sender = unique_bus_name.trim_start_matches(':').replace('.', "_");
    format!("/org/freedesktop/portal/desktop/request/{sender}/{handle_token}")
}

/// A token must be a valid D-Bus path element: ASCII alphanumerics and `_` only.
/// Anything else would produce an unroutable path, so callers get a sanitized token.
pub fn sanitize_token(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    if cleaned.is_empty() {
        "ultidesk".to_string()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_the_documented_request_path() {
        assert_eq!(
            request_path(":1.42", "ultidesk_1"),
            "/org/freedesktop/portal/desktop/request/1_42/ultidesk_1"
        );
    }

    #[test]
    fn strips_leading_colon_and_replaces_every_dot() {
        // A bus name with more than one dot must have all of them replaced, not just
        // the first — a path with a '.' left in it is silently never signalled on.
        assert_eq!(
            request_path(":1.234.567", "t"),
            "/org/freedesktop/portal/desktop/request/1_234_567/t"
        );
    }

    #[test]
    fn tolerates_a_name_that_already_lacks_the_colon() {
        assert_eq!(
            request_path("1.42", "t"),
            "/org/freedesktop/portal/desktop/request/1_42/t"
        );
    }

    #[test]
    fn sanitize_strips_path_breaking_characters() {
        assert_eq!(sanitize_token("ab-cd.ef/gh"), "abcdefgh");
        assert_eq!(sanitize_token("keep_1"), "keep_1");
    }

    #[test]
    fn sanitize_never_yields_an_empty_path_element() {
        // An empty token would produce a trailing-slash path that D-Bus rejects.
        assert_eq!(sanitize_token("---"), "ultidesk");
        assert_eq!(sanitize_token(""), "ultidesk");
    }

    #[test]
    fn response_codes_map_and_cancelled_is_not_success() {
        assert_eq!(ResponseCode::from_raw(0), ResponseCode::Success);
        assert_eq!(ResponseCode::from_raw(1), ResponseCode::Cancelled);
        assert_eq!(ResponseCode::from_raw(2), ResponseCode::Ended);
        assert_eq!(ResponseCode::from_raw(9), ResponseCode::Unknown(9));
        assert!(ResponseCode::from_raw(0).is_success());
        // A user declining the permission dialog must never read as success.
        assert!(!ResponseCode::from_raw(1).is_success());
        assert!(!ResponseCode::from_raw(9).is_success());
    }
}
