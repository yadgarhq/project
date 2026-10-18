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

/// The placeholder is PRESENT and EMPTY, and both halves are the point:
/// present because `scope_of` refuses an absent one, empty because this tier
/// has nothing to attest.
///
/// **THE PREMISE IS PINNED IN `project-db`, NOT HERE (ledger 762).** This
/// test cannot fail when the store stops refusing an absent `Scope`; the
/// tests that can are `project-db`'s `an_absent_scope_is_refused`
/// (`tests/resolve.rs`) and `list_refuses_an_absent_scope`
/// (`tests/list.rs`). Flipping them is route (c), and the same PR train
/// deletes this test and the placeholder.
#[test]
fn the_placeholder_scope_is_present_and_attests_nothing() {
    let scope = unattested_placeholder_scope().expect(
        "the store refuses an absent scope — pinned by project-db's \
         an_absent_scope_is_refused and list_refuses_an_absent_scope (ledger 762)",
    );
    assert_eq!(scope.request_id, "");
    assert_eq!(scope.instance_id, "");
    assert_eq!(scope.user_id, "");
    assert_eq!(scope.project_id, "");
}

/// The header stands in ONLY when the `Scope` carries no id. On the read
/// verbs the placeholder is empty, so the header always wins there; a real
/// `Scope` reaching this tier would carry the attested, stronger claim.
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
        tel_scope_joined(
            unattested_placeholder_scope().as_ref(),
            "from-the-header".into()
        )
        .request_id,
        "from-the-header"
    );
    assert_eq!(tel_scope_joined(None, String::new()).request_id, "");
}
