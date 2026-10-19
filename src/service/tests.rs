//! Unit tests for the tier's guards, without a server.

use super::*;

/// The precision of the guard, asserted without a server: only the BARE
/// segment, and folded for ASCII case.
#[test]
fn the_reserved_root_guard_catches_the_bare_segment_in_any_ascii_case() {
    for path in ["local", "LOCAL", "LoCaL", "lOcAl"] {
        assert!(is_reserved_root(path), "{path} is the reserved segment");
    }
}

#[test]
fn the_reserved_root_guard_catches_nothing_else() {
    for path in [
        "locals",
        "local/home",
        "LOCAL/home",
        "lo-cal",
        "l.ocal",
        "lo_cal",
        "acme/local",
        "",
    ] {
        assert!(!is_reserved_root(path), "{path} is registrable");
    }
}

/// The header stands in ONLY when the `Scope` carries no id. On the read
/// verbs there is no `Scope`, so the header always wins there; a real `Scope`
/// reaching this tier would carry the attested, stronger claim.
#[test]
fn a_scope_request_id_wins_over_the_header() {
    let attested = Scope {
        request_id: "from-the-scope".into(),
        ..Default::default()
    };
    assert_eq!(
        tel_scope_joined(Some(&attested), "from-the-header".into()).request_id,
        "from-the-scope"
    );
    assert_eq!(
        tel_scope_joined(None, "from-the-header".into()).request_id,
        "from-the-header"
    );
    assert_eq!(tel_scope_joined(None, String::new()).request_id, "");
}

/// LEDGER 1643. The list's telemetry payload carries no owner id: the load now
/// relays every private row's owner, and `Outcome.payload` is a rendering of
/// what was returned. The payload keeps the paths, which is what the word
/// count measures; the row count is `Outcome.rows`.
///
/// **THE MUTATION THAT REDS IT:** render the whole response with `{r:?}`.
#[test]
fn the_list_payload_carries_no_owner_id() {
    let owner = "yadgar:user:0192f3c1-an-owner-telemetry-must-not-carry";
    let response = pb::ProjectServiceListProjectsResponse {
        paths: vec![
            pb::RegisteredPath {
                path: "local/the-owner/notes".into(),
                owner_user_id: owner.into(),
                ..Default::default()
            },
            pb::RegisteredPath {
                path: "acme/widgets".into(),
                ..Default::default()
            },
        ],
        next_page_token: String::new(),
    };

    let payload = list::list_payload(&response);
    assert!(!payload.contains(owner), "{payload}");
    assert!(!payload.contains("0192f3c1"), "{payload}");
    assert!(payload.contains("local/the-owner/notes"), "{payload}");
    assert!(payload.contains("acme/widgets"), "{payload}");
}
