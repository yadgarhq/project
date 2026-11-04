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

/// A Scope claiming every field, as a refused `SyncProjects` might.
fn a_claimed_scope(user_id: &str) -> Scope {
    Scope {
        request_id: "the-seeder-s-request-id".into(),
        user_id: user_id.into(),
        instance_id: "a-claimed-instance".into(),
        project_id: "a/claimed-project".into(),
        ..Default::default()
    }
}

/// YAADGAAR 204. An ADMITTED sync records its Scope whole: both checks passed,
/// so the principal is the verified seeder's.
///
/// **THE MUTATION THAT REDS IT:** blank the identity fields on every call.
#[test]
fn an_admitted_sync_records_its_principal() {
    let scope = a_claimed_scope(SEEDER_PRINCIPAL);
    let tel = sync::recorded_scope(Some(SYNC_CALLER), Some(&scope));
    assert_eq!(tel.user_id, SEEDER_PRINCIPAL);
    assert_eq!(tel.request_id, "the-seeder-s-request-id");
    assert_eq!(tel.instance_id, "a-claimed-instance");
    assert_eq!(tel.project_id, "a/claimed-project");
}

/// YAADGAAR 204. A REFUSED sync records the `request_id`, D67's join key, and
/// no identity claim. Each case fails exactly one of the two checks, so a
/// helper that checked only one of them records a claim on the other.
///
/// **THE MUTATIONS THAT RED IT:** record the claimed Scope; check only the
/// caller; check only the principal.
#[test]
fn a_refused_sync_records_only_its_request_id() {
    let seeder = a_claimed_scope(SEEDER_PRINCIPAL);
    let other = a_claimed_scope("yadgar:system:not-the-seeder");
    for (caller, scope) in [
        (Some("gateway-caller"), Some(&seeder)),
        (None, Some(&seeder)),
        (Some(SYNC_CALLER), Some(&other)),
        (Some(SYNC_CALLER), None),
    ] {
        let tel = sync::recorded_scope(caller, scope);
        let want = scope.map_or("", |_| "the-seeder-s-request-id");
        assert_eq!(tel.request_id, want, "{caller:?}");
        assert_eq!(tel.user_id, "", "{caller:?} {scope:?}");
        assert_eq!(tel.instance_id, "", "{caller:?} {scope:?}");
        assert_eq!(tel.project_id, "", "{caller:?} {scope:?}");
    }
}
