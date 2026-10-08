//! How THIS binary adopts the shared listener type: its prefix, its chart key,
//! and the refusals an operator reads in this service's crash log.
//!
//! The parsing itself is `yadgar-lifecycle`'s and is tested there. What can go
//! wrong HERE is the wiring — a wrong prefix, a chart key the shared type
//! appends `.enabled` to twice — so every case passes [`LISTEN`] and
//! [`CHART_KEY`], exactly as `boot.rs` does, and asserts the literal chart key
//! the operator edits.
//!
//! STATIC FAILURE MESSAGES throughout: CodeQL's cleartext-logging query reads
//! this error type's certificate- and key-named variants as sensitive when
//! interpolated into an assert message. Nothing here is secret, and naming what
//! was EXPECTED is exactly as strict.

use std::path::Path;

use super::*;

/// SENTINELS: nothing in `serve.rs` could produce either of them, so a test
/// that sees one saw it travel from the lookup.
const SENTINEL_CERT: &str = "/etc/yadgar/pangolin-7c21/serving.crt";
const SENTINEL_KEY: &str = "/etc/yadgar/pangolin-7c21/serving.key";
const SENTINEL_CA: &str = "/etc/yadgar/pangolin-7c21/client-ca.crt";

fn lookup<'a>(pairs: &'a [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> + 'a {
    move |key| {
        pairs
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.to_string())
    }
}

fn read(pairs: &[(&'static str, &'static str)]) -> Result<Option<ServerTls>, ServeTlsError> {
    ServerTls::from_lookup(LISTEN, CHART_KEY, lookup(pairs))
}

/// ADR-0845: an absent `LISTEN_TLS_ENABLED` refuses, naming the variable and
/// `tls.enabled` — the chart key the shared type DERIVES from [`CHART_KEY`].
/// Passing `"tls.enabled"` (this module's old constant) would render
/// `tls.enabled.enabled`, and this is the case that notices.
#[test]
fn absent_tls_enabled_refuses_naming_the_variable_and_the_chart_key() {
    let message = read(&[("LISTEN_TLS_CLIENT_AUTH", "off")])
        .expect_err("an absent LISTEN_TLS_ENABLED must refuse, never silently pick cleartext")
        .to_string();
    assert!(
        message.contains("LISTEN_TLS_ENABLED"),
        "the refusal must name the LISTEN_TLS_ENABLED variable"
    );
    assert!(
        message.contains("`tls.enabled`"),
        "the refusal must name the chart key tls.enabled exactly"
    );
}

/// ADR-0854: an absent `LISTEN_TLS_CLIENT_AUTH` refuses, naming the variable
/// and `tls.clientAuth`, whether or not TLS is on.
#[test]
fn absent_client_auth_refuses_naming_the_variable_and_the_chart_key() {
    for enabled in ["0", "1"] {
        let pairs = [
            ("LISTEN_TLS_ENABLED", enabled),
            ("LISTEN_TLS_CERT_FILE", SENTINEL_CERT),
            ("LISTEN_TLS_KEY_FILE", SENTINEL_KEY),
        ];
        let message = read(&pairs)
            .expect_err("an absent LISTEN_TLS_CLIENT_AUTH must refuse the boot")
            .to_string();
        assert!(
            message.contains("LISTEN_TLS_CLIENT_AUTH"),
            "the refusal must name the LISTEN_TLS_CLIENT_AUTH variable"
        );
        assert!(
            message.contains("`tls.clientAuth`"),
            "the refusal must name the chart key tls.clientAuth exactly"
        );
    }
}

/// A certificate without the flag is NOT the reverted state by itself —
/// `LISTEN_TLS_ENABLED` must still be stated.
#[test]
fn a_certificate_alone_with_no_flag_still_refuses_the_boot() {
    let pairs = [
        ("LISTEN_TLS_CLIENT_AUTH", "off"),
        ("LISTEN_TLS_CERT_FILE", SENTINEL_CERT),
        ("LISTEN_TLS_KEY_FILE", SENTINEL_KEY),
    ];
    assert!(
        matches!(read(&pairs), Err(ServeTlsError::EnabledMissing { .. })),
        "a certificate with no LISTEN_TLS_ENABLED must refuse as EnabledMissing"
    );
}

/// THE REVERT LEVER. Explicit `"0"` with client auth `off` is the cleartext
/// listener, not an error: leaving the certificate paths in place while the
/// flag is off is how the cut-over gets pulled back.
#[test]
fn a_certificate_survives_an_explicit_off() {
    let pairs = [
        ("LISTEN_TLS_ENABLED", "0"),
        ("LISTEN_TLS_CLIENT_AUTH", "off"),
        ("LISTEN_TLS_CERT_FILE", SENTINEL_CERT),
        ("LISTEN_TLS_KEY_FILE", SENTINEL_KEY),
    ];
    assert_eq!(
        read(&pairs).expect("an explicit 0 with clientAuth off is cleartext"),
        None
    );
}

/// A cleartext listener cannot verify a caller, so `optional` or `required`
/// beside `LISTEN_TLS_ENABLED=0` refuses rather than serving unverified.
#[test]
fn a_verifying_mode_on_a_cleartext_listener_refuses_the_boot() {
    for mode in ["optional", "required"] {
        let pairs = [
            ("LISTEN_TLS_ENABLED", "0"),
            ("LISTEN_TLS_CLIENT_AUTH", mode),
            ("LISTEN_TLS_CLIENT_CA_FILE", SENTINEL_CA),
        ];
        assert!(
            matches!(
                read(&pairs),
                Err(ServeTlsError::ClientAuthWithoutTls { .. })
            ),
            "a verifying clientAuth with LISTEN_TLS_ENABLED=0 must refuse the boot"
        );
    }
}

/// THE FAILURE THAT MUST NOT DEGRADE. Asking for TLS and naming no
/// certificate or no key is a deployment mistake, and the refusal names
/// `tls.certSecret`, the value that mounts both.
#[test]
fn asking_for_tls_without_a_certificate_or_a_key_is_an_error() {
    for pairs in [
        vec![
            ("LISTEN_TLS_ENABLED", "1"),
            ("LISTEN_TLS_CLIENT_AUTH", "off"),
            ("LISTEN_TLS_KEY_FILE", SENTINEL_KEY),
        ],
        vec![
            ("LISTEN_TLS_ENABLED", "1"),
            ("LISTEN_TLS_CLIENT_AUTH", "off"),
            ("LISTEN_TLS_CERT_FILE", SENTINEL_CERT),
            ("LISTEN_TLS_KEY_FILE", "   "),
        ],
    ] {
        let outcome = read(&pairs);
        assert!(
            matches!(outcome, Err(ServeTlsError::NoServingFile { .. })),
            "half a serving pair must be refused, not silently downgraded"
        );
        assert!(
            outcome
                .expect_err("half a serving pair must refuse")
                .to_string()
                .contains("`tls.certSecret`"),
            "the refusal must name the chart key tls.certSecret"
        );
    }
}

/// Every path reaches the settings, proved with names the module could not
/// have chosen for itself.
#[test]
fn the_certificate_the_key_and_the_client_ca_all_arrive() {
    let pairs = [
        ("LISTEN_TLS_ENABLED", "1"),
        ("LISTEN_TLS_CLIENT_AUTH", "required"),
        ("LISTEN_TLS_CERT_FILE", SENTINEL_CERT),
        ("LISTEN_TLS_KEY_FILE", SENTINEL_KEY),
        ("LISTEN_TLS_CLIENT_CA_FILE", SENTINEL_CA),
    ];
    let tls = read(&pairs)
        .expect("a complete mutual-TLS configuration is accepted")
        .expect("the flag is set");
    assert_eq!(tls.cert_file(), Path::new(SENTINEL_CERT));
    assert_eq!(tls.key_file(), Path::new(SENTINEL_KEY));
    assert_eq!(tls.client_auth(), ClientAuth::Required);
    assert_eq!(tls.client_ca_file(), Some(Path::new(SENTINEL_CA)));
}

/// The prefix is what selects the variables, so the upstream's transport
/// cannot configure the listener. `PROJECT_DB_TLS_*` is a real setting in this
/// process — [`crate::upstream`] reads it — which is what makes this worth
/// pinning rather than obvious.
#[test]
fn the_upstreams_variables_do_not_configure_the_listener() {
    let pairs = [
        ("LISTEN_TLS_ENABLED", "0"),
        ("LISTEN_TLS_CLIENT_AUTH", "off"),
        ("PROJECT_DB_TLS_ENABLED", "1"),
        ("PROJECT_DB_TLS_CA_FILE", SENTINEL_CERT),
        ("TLS_ENABLED", "1"),
        ("TLS_CERT_FILE", SENTINEL_CERT),
        ("TLS_CLIENT_AUTH", "required"),
    ];
    assert_eq!(
        read(&pairs).expect("the listener's own keys state cleartext"),
        None
    );
}
