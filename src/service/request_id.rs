//! D67's request id on the two read verbs: read from `x-yadgar-request-id`,
//! capped, and forwarded to `project-db` as the same header (ledger 1254).
//!
//! **A HEADER, BECAUSE NEITHER READ REQUEST CARRIES A `Scope`.** The gateway
//! stamps the id on every upstream call (`gateway/src/upstream/request_id.rs`,
//! ledger 1248) for exactly that reason, and `iam` → `iam-db` already relay it
//! the same way. Its own file because it is the one seam of this tier that is
//! transport metadata rather than contract.

use tonic::Request;

/// The metadata key every hop reads D67's id from — the gateway's `HEADER`
/// (`gateway/src/upstream/request_id.rs`), `iam`'s and `iam-db`'s.
const REQUEST_ID_HEADER: &str = "x-yadgar-request-id";

/// How many of a caller's characters `x-yadgar-request-id` may put on a record.
///
/// A cut value is at most `REQUEST_ID_CAP + 3` characters, the marker included.
///
/// **THE SAME BOUND AND MARKER `iam` AND `iam-db` APPLY** (`iam`'s
/// `REQUEST_ID_CAP`, `CUT_MARKER` and `capped`). A copy rather than a shared
/// crate because none holds it yet — TODO(1255): replace this copy, the marker
/// and [`capped`] with the shared crate's when ledger 1255 lands. Until then the
/// copies must stay identical, or two hops cut one over-long id differently and
/// their records stop joining.
const REQUEST_ID_CAP: usize = 256;

/// What marks a cut. **ASCII, and that is load-bearing**: the capped id is
/// forwarded in a header, and `project-db` reads it with `to_str()`, which
/// refuses any non-ASCII byte.
const CUT_MARKER: &str = "...";

/// `s` cut to [`REQUEST_ID_CAP`] characters, with [`CUT_MARKER`] appended if it
/// was cut. Capping a capped value changes nothing, so the next hop's re-cap
/// leaves the forwarded id as it is.
fn capped(s: &str) -> std::borrow::Cow<'_, str> {
    match s.char_indices().nth(REQUEST_ID_CAP) {
        Some((at, _)) => format!("{}{CUT_MARKER}", &s[..at]).into(),
        None => s.into(),
    }
}

/// D67's join key, read from the header at its one read point (ledger 1254).
///
/// **NEITHER READ REQUEST CARRIES A `Scope`**, so the header the gateway stamps
/// on every upstream call is the only place the id can come from. Read BEFORE
/// the request is consumed: `into_inner` drops the metadata.
///
/// Capped rather than refused, and an absent or unreadable value is `""`,
/// because a correlation id must never fail a call (D67).
pub(super) fn request_id_of<T>(req: &Request<T>) -> String {
    req.metadata()
        .get(REQUEST_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(|v| capped(v).into_owned())
        .unwrap_or_default()
}

/// A store request carrying `request_id` as `x-yadgar-request-id`, so
/// `project-db`'s record joins this hop's.
///
/// `request_id` is the id ALREADY ON this call's record — capped, so the wire is
/// bounded and carries the same string. An empty id forwards NO header rather
/// than an empty one, which `project-db` reads exactly as it reads none. A value
/// that cannot be a header is dropped, never an error (D67); it cannot happen,
/// since `to_str()` admitted the inbound value and the marker is ASCII.
pub(super) fn carrying<T>(message: T, request_id: &str) -> Request<T> {
    let mut req = Request::new(message);
    if !request_id.is_empty() {
        if let Ok(v) = request_id.parse() {
            req.metadata_mut().insert(REQUEST_ID_HEADER, v);
        }
    }
    req
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An id of exactly [`REQUEST_ID_CAP`] characters is not cut, one more is,
    /// and re-capping a cut id changes nothing — the property that lets
    /// `project-db` re-cap the forwarded value and still join.
    #[test]
    fn the_cap_cuts_only_past_the_bound_and_is_idempotent() {
        let at = "a".repeat(REQUEST_ID_CAP);
        assert_eq!(capped(&at), at);
        let over = "a".repeat(REQUEST_ID_CAP + 1);
        let cut = capped(&over).into_owned();
        assert_eq!(cut, format!("{at}{CUT_MARKER}"));
        assert_eq!(capped(&cut), cut);
    }
}
