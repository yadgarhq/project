//! What an operator reads when the boot refuses (ledger 1258).
//!
//! `main` used to return `Result<(), Box<dyn Error>>`, and Rust prints a `main`
//! that returns `Err` with DEBUG. Every refusal in `main.rs` and `boot.rs` was already
//! converted to its sentence, so it did not arrive as a variant name — it
//! arrived as a quoted, escaped string: `Error: "LISTEN_TLS_ENABLED is set but
//! …"`, every inner quote a `\"`. `main` now calls `run()` and prints
//! `Error: {e}` with Display. The unit tests in `src/` prove the sentences
//! exist; only running the BINARY proves they reach the operator as written.
//!
//! **The exit code is not what these tests discriminate.** An `Err` from
//! `main` exits 1 and so does `ExitCode::FAILURE`, so reverting to
//! `-> Result` turns the Display assertion red and leaves the exit code alone.

mod support;

use std::net::TcpListener;
use std::process::Command;
use std::time::Duration;

use support::{cleartext_env, run_to_exit, Booted, BIN};

/// The `Error: ` line from a refused boot, held to plain Display.
fn refusal_line(status: std::process::ExitStatus, stderr: &str) -> String {
    assert!(
        !status.success(),
        "a refused boot must exit non-zero: {status:?}"
    );
    let line = stderr
        .lines()
        .rfind(|l| l.starts_with("Error: "))
        .unwrap_or_else(|| panic!("no `Error: ` line on stderr: {stderr}"))
        .to_string();
    assert!(
        !line.starts_with("Error: \"") && !line.contains("\\\""),
        "the refusal was printed as a Debug string: {line}"
    );
    line
}

/// Run the binary directly, for refusals that come before `shared.yaml` is
/// read and so need no mount namespace.
fn refusal_without_mounts(vars: &[(&str, &str)]) -> String {
    let out = Command::new(BIN)
        .env_clear()
        .envs(vars.iter().copied())
        .output()
        .expect("the test rig could not start the binary");
    refusal_line(out.status, &String::from_utf8_lossy(&out.stderr))
}

/// A TYPED error: `ServeTlsError::NoCertFile`, the first refusal `run` can
/// reach. Debug of the error would print `NoCertFile("LISTEN")`; Debug of its
/// sentence printed the sentence in quotes. Display prints the sentence.
#[test]
fn a_typed_refusal_is_printed_as_its_sentence() {
    let line = refusal_without_mounts(&[("LISTEN_TLS_ENABLED", "1")]);
    assert!(
        line.contains("LISTEN_TLS_ENABLED is set but LISTEN_TLS_CERT_FILE names no certificate"),
        "the refusal must name both variables: {line}"
    );
    assert!(
        !line.contains("NoCertFile"),
        "the operator got the Debug variant name, not the sentence: {line}"
    );
}

/// A bare parse, named on the way out: the port is read before the shared
/// document, so this also needs no mount namespace.
#[test]
fn an_unparsable_db_port_names_the_variable() {
    let mut vars = cleartext_env();
    vars.retain(|(k, _)| *k != "PROJECT_DB_PORT");
    vars.push(("PROJECT_DB_PORT", "abc"));
    let line = refusal_without_mounts(&vars);
    assert!(
        line.contains("PROJECT_DB_PORT is not a port number"),
        "the refusal must name the variable: {line}"
    );
}

/// `LISTEN` is parsed after the shared document is read and the schedule is
/// resolved, so this boot needs the document mounted to get that far.
#[test]
fn an_unparsable_listen_names_the_variable() {
    let mut vars = cleartext_env();
    vars.retain(|(k, _)| *k != "LISTEN");
    vars.push(("LISTEN", "notanaddr"));
    let (status, stderr) = run_to_exit(&vars);
    let line = refusal_line(status, &stderr);
    assert!(
        line.contains("LISTEN is not a host:port address"),
        "the refusal must name the variable: {line}"
    );
    assert!(
        !line.contains("AddrParseError"),
        "the operator got the Debug of the parse error: {line}"
    );
}

/// `METRICS_LISTEN` is parsed in `serve_until_drained`, after the shared
/// document, so this also needs the mount. Nothing currently exercises this
/// one bare parse: without this test, turning its `map_err` back into a bare
/// `?` would compile, pass every other suite, and only show up as a Debug
/// string in a crash loop nobody runs this harness against.
#[test]
fn an_unparsable_metrics_listen_names_the_variable() {
    let mut vars = cleartext_env();
    vars.retain(|(k, _)| *k != "METRICS_LISTEN");
    vars.push(("METRICS_LISTEN", "notanaddr"));
    let (status, stderr) = run_to_exit(&vars);
    let line = refusal_line(status, &stderr);
    assert!(
        line.contains("METRICS_LISTEN is not a host:port address"),
        "the refusal must name the variable: {line}"
    );
    assert!(
        !line.contains("AddrParseError"),
        "the operator got the Debug of the parse error: {line}"
    );
}

/// A TRANSPORT failure rather than a misconfiguration: `LISTEN` names a real
/// `host:port`, but another socket already holds it. `boot::prepare` cannot
/// see this — it only validates TLS, never binds — so the refusal has to come
/// from the server's own bind, inside the task `serve_until_drained` spawns.
///
/// **THAT TASK'S FAILURE IS INVISIBLE UNTIL SOMETHING ASKS THE PROCESS TO
/// STOP.** `yadgar_lifecycle::drain_within` awaits `stop` FIRST and only then
/// joins the spawned server — see its own doc comment — so a bind that fails
/// before any signal arrives leaves the process running (silently, having
/// bound nothing) rather than exiting (ledger 1334 tracks the fix, in
/// `yadgar-lifecycle`: `drain_within` should `select!` on the server
/// alongside `stop`, not await `stop` alone).
///
/// **TWO REAL MISCONFIGURATIONS PRODUCE THIS, and the chart catches neither
/// automatically.** A port already held by another container in THIS SAME
/// POD (pods never share a network namespace with each other, so this is
/// never a cross-pod collision) leaves the `tcpSocket` readinessProbe on
/// `grpc` passing against that OTHER container — this is the case below, and
/// the pod is Ready while this service answers nothing. An address this host
/// cannot use (`EADDRNOTAVAIL`) or cannot bind (`EACCES`) instead fails that
/// same probe, so the pod is NotReady forever. There is no `livenessProbe` in
/// this chart ("failing readiness never kills a pod",
/// `chart/templates/deployment.yaml`), so NEITHER case ever gets the SIGTERM
/// that would surface this error — an operator or a rollout has to notice and
/// replace the pod by hand. This test supplies that missing SIGTERM itself,
/// after giving the bind attempt — synchronous, effectively instant — time to
/// fail.
///
/// Held in-process rather than by a second `unshare` child: the listener and
/// the binary under test must share one network namespace for the collision
/// to be real, and `unshare -rm` only ever isolates the mount namespace
/// (`-m`), never the network one, so a plain `TcpListener` here collides with
/// the child exactly as a sidecar container in the same pod would.
#[test]
fn a_bind_failure_names_the_listen_address() {
    let holder = TcpListener::bind("127.0.0.1:0").expect("a free port must be available to hold");
    let addr = holder
        .local_addr()
        .expect("a bound listener has an address");
    let listen = addr.to_string();

    let mut vars = cleartext_env();
    vars.retain(|(k, _)| *k != "LISTEN");
    vars.push(("LISTEN", listen.as_str()));

    let mut project = Booted::start(&vars);
    // Fires regardless of whether the real bind that follows succeeds — see
    // `serve_until_drained`, which logs it before spawning the server task.
    project.wait_until_listening();
    // The bind itself is a single synchronous syscall; this is slack for the
    // scheduler to have polled the spawned task at least once, not a wait for
    // anything slow.
    std::thread::sleep(Duration::from_millis(200));
    project.terminate();
    let status = project.wait_for_exit(
        "after SIGTERM surfaced the bind failure",
        Duration::from_secs(10),
    );
    let seen = project.seen().join("\n");
    drop(holder);

    let line = refusal_line(status, &seen);
    assert!(
        line.contains(&format!(
            "the gRPC server on LISTEN={listen} stopped with an error"
        )),
        "the refusal must name the LISTEN address: {line}"
    );
    assert!(
        line.contains("Address already in use"),
        "the refusal must carry the OS reason, not just tonic's own `transport error`: {line}"
    );
}
