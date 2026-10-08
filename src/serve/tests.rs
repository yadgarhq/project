use super::*;

/// SENTINELS: nothing in `serve.rs` could produce either of them, so a test
/// that sees one saw it travel from the lookup.
const SENTINEL_CERT: &str = "/etc/yadgar/pangolin-7c21/serving.crt";
const SENTINEL_KEY: &str = "/etc/yadgar/pangolin-7c21/serving.key";

fn lookup<'a>(pairs: &'a [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> + 'a {
    move |key| {
        pairs
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.to_string())
    }
}

/// ADR-0845: THERE IS NO DEFAULT. An absent `LISTEN_TLS_ENABLED` used to
/// mean the cleartext listener; it now refuses the boot, naming the
/// variable and the chart key, because a knob with no compiled-in
/// fallback cannot answer "nothing configured" with a transport choice.
#[test]
fn absent_tls_enabled_refuses_the_boot() {
    let error = ServeTls::from_lookup(LISTEN, CHART_KEY, lookup(&[]))
        .expect_err("an absent LISTEN_TLS_ENABLED must refuse, never silently pick cleartext");
    // STATIC FAILURE MESSAGES: CodeQL's cleartext-logging query reads
    // this error type's variant names (`NotStated`, and siblings like
    // `NoKeyFile` elsewhere in this enum) as sensitive-looking
    // identifiers when interpolated into an assert message. Nothing
    // here is secret — it is this module's own config-shape error —
    // but naming what was EXPECTED costs nothing and the assertion is
    // exactly as strict either way.
    assert!(
        matches!(
            error,
            ServeTlsError::NotStated {
                var: "LISTEN",
                chart_key: CHART_KEY,
                ..
            }
        ),
        "an absent LISTEN_TLS_ENABLED must produce ServeTlsError::NotStated naming LISTEN and the chart key"
    );
    let message = error.to_string();
    assert!(
        message.contains("LISTEN_TLS_ENABLED"),
        "the refusal must name the LISTEN_TLS_ENABLED variable"
    );
    assert!(
        message.contains(CHART_KEY),
        "the refusal must name the chart key"
    );
}

/// A certificate without the flag is NOT the reverted state by itself —
/// `LISTEN_TLS_ENABLED` must still be stated. Leaving the files in place
/// while the flag is explicitly `"0"` is the revert lever
/// (`a_certificate_survives_an_explicit_off` below); leaving the flag
/// unstated at all is the thing ADR-0845 refuses.
#[test]
fn a_certificate_alone_with_no_flag_still_refuses_the_boot() {
    let vars = [
        ("LISTEN_TLS_CERT_FILE", SENTINEL_CERT),
        ("LISTEN_TLS_KEY_FILE", SENTINEL_KEY),
    ];
    assert!(matches!(
        ServeTls::from_lookup(LISTEN, CHART_KEY, lookup(&vars)),
        Err(ServeTlsError::NotStated { .. })
    ));
}

/// THE REVERT LEVER. Explicit `"0"` is the reverted state, not an error:
/// leaving the certificate paths in place while the flag is off is how
/// the cut-over gets pulled back.
#[test]
fn a_certificate_survives_an_explicit_off() {
    let vars = [
        ("LISTEN_TLS_ENABLED", "0"),
        ("LISTEN_TLS_CERT_FILE", SENTINEL_CERT),
        ("LISTEN_TLS_KEY_FILE", SENTINEL_KEY),
    ];
    assert_eq!(
        ServeTls::from_lookup(LISTEN, CHART_KEY, lookup(&vars)).unwrap(),
        None
    );
}

/// Only "1" enables and only "0" is the stated-off default; every other
/// spelling — including the empty string, which `get` treats as unset —
/// refuses rather than silently choosing a transport. A permissive parse
/// here is how a setting meant to be off ends up on, and this flag is the
/// cut-over's revert lever, so a lever that moves on the wrong input is
/// not one.
#[test]
fn only_exactly_one_or_zero_are_stated_values() {
    for value in ["false", "no", "true", "yes", "", " ", "01", "2"] {
        let vars = [
            ("LISTEN_TLS_ENABLED", value),
            ("LISTEN_TLS_CERT_FILE", SENTINEL_CERT),
            ("LISTEN_TLS_KEY_FILE", SENTINEL_KEY),
        ];
        assert!(
            matches!(
                ServeTls::from_lookup(LISTEN, CHART_KEY, lookup(&vars)),
                Err(ServeTlsError::NotStated { .. })
            ),
            "{value:?} must refuse rather than pick a transport"
        );
    }
}

/// THE FAILURE THAT MUST NOT DEGRADE. Asking for TLS and naming no
/// certificate is a deployment mistake, and the answer to it is an error
/// rather than a plaintext listener.
#[test]
fn asking_for_tls_without_a_certificate_is_an_error() {
    for vars in [
        vec![
            ("LISTEN_TLS_ENABLED", "1"),
            ("LISTEN_TLS_KEY_FILE", SENTINEL_KEY),
        ],
        vec![
            ("LISTEN_TLS_ENABLED", "1"),
            ("LISTEN_TLS_CERT_FILE", ""),
            ("LISTEN_TLS_KEY_FILE", SENTINEL_KEY),
        ],
        vec![
            ("LISTEN_TLS_ENABLED", "1"),
            ("LISTEN_TLS_CERT_FILE", "   "),
            ("LISTEN_TLS_KEY_FILE", SENTINEL_KEY),
        ],
    ] {
        assert!(
            matches!(
                ServeTls::from_lookup(LISTEN, CHART_KEY, lookup(&vars)),
                Err(ServeTlsError::NoCertFile("LISTEN"))
            ),
            "{vars:?} must be refused, not silently downgraded"
        );
    }
}

/// The same for the key. Half a pair is not an identity, and the message
/// has to name the half that is missing.
#[test]
fn asking_for_tls_without_a_private_key_is_an_error() {
    for vars in [
        vec![
            ("LISTEN_TLS_ENABLED", "1"),
            ("LISTEN_TLS_CERT_FILE", SENTINEL_CERT),
        ],
        vec![
            ("LISTEN_TLS_ENABLED", "1"),
            ("LISTEN_TLS_CERT_FILE", SENTINEL_CERT),
            ("LISTEN_TLS_KEY_FILE", "   "),
        ],
    ] {
        assert!(
            matches!(
                ServeTls::from_lookup(LISTEN, CHART_KEY, lookup(&vars)),
                Err(ServeTlsError::NoKeyFile("LISTEN"))
            ),
            "{vars:?} must be refused, not silently downgraded"
        );
    }
}

/// Both paths reach the settings, proved with names the module could not
/// have chosen for itself.
#[test]
fn the_certificate_and_the_key_both_arrive() {
    let vars = [
        ("LISTEN_TLS_ENABLED", "1"),
        ("LISTEN_TLS_CERT_FILE", SENTINEL_CERT),
        ("LISTEN_TLS_KEY_FILE", SENTINEL_KEY),
    ];
    let tls = ServeTls::from_lookup(LISTEN, CHART_KEY, lookup(&vars))
        .unwrap()
        .expect("a flag, a certificate and a key enable TLS");
    assert_eq!(tls.cert_file(), Path::new(SENTINEL_CERT));
    assert_eq!(tls.key_file(), Path::new(SENTINEL_KEY));
}

/// The prefix is what selects the variables, so the upstream's transport
/// cannot configure the listener. `PROJECT_DB_TLS_*` is a real setting in this
/// process — [`crate::upstream`] reads it — which is what makes this worth
/// pinning rather than obvious. `LISTEN_TLS_ENABLED` is stated explicitly
/// as `"0"` here, because an absent one now refuses before the prefix
/// isolation this case exists to prove is ever reached.
#[test]
fn the_upstreams_variables_do_not_configure_the_listener() {
    let vars = [
        ("LISTEN_TLS_ENABLED", "0"),
        ("PROJECT_DB_TLS_ENABLED", "1"),
        ("PROJECT_DB_TLS_CA_FILE", SENTINEL_CERT),
        ("TLS_ENABLED", "1"),
        ("TLS_CERT_FILE", SENTINEL_CERT),
    ];
    assert_eq!(
        ServeTls::from_lookup(LISTEN, CHART_KEY, lookup(&vars)).unwrap(),
        None
    );
}
