//! Everything that is not `run`'s own wiring: installing the logging
//! subscriber, checking every boot input before a socket of any kind opens,
//! and running the server until it has drained.
//!
//! **Declared by `main.rs`, and NOT part of `yadgar_project`.** This file sits
//! beside four library modules while belonging to neither crate root but the
//! binary's: `lib.rs` does not declare it, so nothing outside this binary can
//! reach it and adding it changed no public surface.
//!
//! **`prepare`'s SEAM IS THE FIRST SOCKET.** Everything it runs reads a file
//! or an environment variable and validates what it read; nothing in it
//! dials, binds, or spawns. `run` picks up at the dial to `project-db`, which
//! is the first line in the sequence that can fail for a reason a restart
//! would not fix. Splitting there keeps the property `main.rs` already
//! documents — a deployment that got a mount or a knob wrong exits before
//! anything is listening — expressible as one call rather than as an ordering a
//! reader has to reconstruct. [`serve_until_drained`], below it, is the
//! opposite half: it binds the listener and spawns the server once the
//! upstream channel exists.
//!
//! **WHAT IS DELIBERATELY LEFT BEHIND.** `METRICS_LISTEN` and `LISTEN` are read
//! and parsed in [`serve_until_drained`], AFTER the dial, exactly where they
//! were. They are configuration and they would fit in [`prepare`], but moving
//! them would make a deployment with a malformed `METRICS_LISTEN` fail before
//! the dial instead of after it. That is a change to which error an operator
//! sees first, which is behaviour, and this split changes none.

use std::net::SocketAddr;

use tonic::transport::{Channel, Server};
use yadgar_lifecycle::{drain_within, shutdown, Drain, DRAIN_BUDGET};
use yadgar_project::pb::yadgar::project::v1::project_service_server::ProjectServiceServer;
use yadgar_project::rotate::{self, Inputs, Schedule};
use yadgar_project::serve::{self, ServeTls, LISTEN};
use yadgar_project::service::Project;
use yadgar_project::upstream::{UpstreamTls, PROJECT_DB};

// `main.rs`'s own helper, reached through the binary's crate root: this module
// is its descendant, so the private item is in scope. It stays there because
// `serve_until_drained` still reads `METRICS_LISTEN` and `LISTEN` through it.
use crate::env_required;

/// The JSON subscriber, installed before anything else can want to log.
pub fn install_logging() {
    tracing_subscriber::fmt()
        .json()
        // A DEFAULT, because from_default_env() with RUST_LOG unset enables
        // NOTHING — the service runs silently and its boot sequence, its
        // capability probe result and its errors all vanish. Found by deploying:
        // two replicas were Running and `kubectl logs` returned nothing at all,
        // so the only way to see why one had restarted was the previous
        // container's exit output.
        //
        // A service nobody can observe is one D67 cannot measure either.
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
}

/// The resolved inputs, handed to `run` in the order `run` consumes them.
///
/// **A struct rather than a tuple** because seven values of which two are
/// `Option`s and two are `u16`-or-`String` is a call site nobody can read, and
/// because a field gained later is a compile error at the destructuring rather
/// than a silently reordered pair.
pub struct Prepared {
    /// Not `mut` here — `serve_until_drained` rebinds it mutably to add the
    /// service.
    pub server: Server,
    /// Retained past its use below so `serve_until_drained` can log whether
    /// the listener is encrypted.
    pub tls: Option<ServeTls>,
    pub db_host: String,
    pub db_port: u16,
    pub db_tls: Option<UpstreamTls>,
    pub watch_inputs: rotate::Inputs,
    pub schedule: rotate::Schedule,
}

/// Read and CHECK every input, in the order `run` reads them in.
///
/// The sequence is unchanged from the one this function was lifted out of, and
/// the order is load-bearing: each refusal below is the first thing an operator
/// sees for that misconfiguration, and reordering them changes which one that
/// is. See the comments on each step for why it refuses rather than defaults.
pub fn prepare() -> Result<Prepared, Box<dyn std::error::Error>> {
    // FIRST, before a socket of any kind is opened. The identity this service
    // presents is read and CHECKED here — the PEM decoded, the certificate
    // matched against its key — so a deployment that asked for TLS and got the
    // mount wrong exits now rather than after something is already listening.
    //
    // `serve::builder` is the only server construction in this binary, which is
    // structural rather than tidy: the downgrade this car removes is a listener
    // that opens in cleartext because TLS configuration failed, and with one
    // construction site there is nowhere else to write it.
    let tls = ServeTls::from_env(LISTEN).map_err(|e| e.to_string())?;
    let server = serve::builder(tls.as_ref()).map_err(|e| e.to_string())?;

    // The HEADLESS Service name (D23). Resolving it yields every ready pod
    // address rather than one virtual IP.
    let db_host = env_required("PROJECT_DB_HOST")?;
    // STRINGIFIED AND NAMED. A bare `?` here yields `ParseIntError { kind:
    // InvalidDigit }`, and the two addresses in `serve_until_drained` yield
    // `AddrParseError(())` — neither names which variable was wrong or what it
    // held. It dates from when `main` returned `Result` and Rust printed a
    // bare `?` here with Debug: `main` prints Display now (ledger 1258), but
    // naming the variable is still something Display alone cannot do. These
    // three were the last bare `?`s left beside the comments explaining why
    // nothing else is one.
    let db_port: u16 = env_required("PROJECT_DB_PORT")?
        .parse()
        .map_err(|e| format!("PROJECT_DB_PORT is not a port number: {e}"))?;

    // OPT-IN, and OFF unless a deployment asks for it. Nothing configured means
    // the cleartext dial this service has always done — no server in the estate
    // serves TLS yet, so the cut-over is a later change that can be reverted on
    // its own.
    //
    // `.to_string()` on the way out. It dates from when `main` returned
    // `Result` and Rust printed a bare `?` here with Debug, as
    // `NoCaFile("PROJECT_DB")`. `main` prints Display now, so the conversion
    // no longer changes what the operator reads; it stays as the sentence it
    // always produced. The same reason the gateway stringifies `Limits::parse`.
    let db_tls = UpstreamTls::from_env(PROJECT_DB).map_err(|e| e.to_string())?;

    // THE ROTATION SCHEDULE, READ FROM THE MOUNTED DOCUMENT (ADR-0569,
    // ADR-0570). `yadgarhq/config` renders it into the `shared` ConfigMap,
    // mounted at `/etc/yadgar/config/shared/shared.yaml`. There is no
    // compiled-in default behind it: an absent, empty, or half-written document
    // refuses the boot and names the file.
    //
    // NO ENV FALLBACK, AND THIS MODULE NEVER HAD ONE. `task` reached this state
    // through a cut-over and its chart carried TLS_ROTATION_POLL_SECS and
    // TLS_ROTATION_SPLAY_MAX_SECS through the rollout that straddled it. There
    // is no earlier digest of this binary to straddle, so this module starts on
    // the far side of that migration and its chart renders neither knob.
    //
    // ADR-0523-WATCHED: the document is `rotate::Configuration`, a member of the
    // watch set assembled below.
    let config = rotate::Configuration::mounted();

    // THE WATCH SET, ASSEMBLED FROM THE RESOLVED CONFIGURATION AND BEFORE THE
    // DIAL (ADR-0523). The baseline is the bytes each file held when this
    // process read them, and every entry is hashed as `watch_set` folds it —
    // deferring the first reading to the watcher's first poll would put the rest
    // of boot inside a window where a kubelet swap quietly becomes the baseline,
    // and the real rotation would never be noticed.
    //
    // THE MOUNTED DOCUMENT JOINS THE SAME SET, as a third `Material` — an
    // operator editing `shared.yaml` now restarts this pod exactly as editing a
    // certificate would.
    //
    // ONE CALL, AND THE SAME ONE A TEST MAKES. This used to be two builder calls
    // forty lines apart in this function, where nothing could reach them: no
    // test spawned this binary then, so deleting either compiled and passed
    // everything. `tests/exit_chain.rs` spawns it now (ledger 748), and the
    // list lives in `rotate::watch_set`, which `tests/assembly.rs` also calls.
    let watch_inputs = rotate::watch_set(tls.as_ref(), db_tls.as_ref(), &config);

    // READ FROM THE SAME DOCUMENT THE WATCH SET JUST JOINED, whether or not any
    // TLS is configured. A value the document names and this binary cannot use
    // is a mistake to refuse, not one to paper over with a default nobody
    // chose — and refusing it here means it is refused on a cleartext
    // deployment too, which is where it would otherwise sit unnoticed until the
    // cut-over.
    let schedule = config.schedule().map_err(|e| e.to_string())?;

    Ok(Prepared {
        server,
        tls,
        db_host,
        db_port,
        db_tls,
        watch_inputs,
        schedule,
    })
}

/// EVERYTHING AFTER THE UPSTREAM CHANNEL EXISTS: the metrics exporter, the
/// listener, and the bounded drain.
///
/// One helper rather than several, because this is one stretch of a sequence
/// whose ORDER is the behaviour. The exporter is installed before the gauge
/// that feeds it, the signal handlers are armed before the server is spawned,
/// and the drain's budget starts when shutdown is requested. Cutting between
/// those steps would produce parts that cannot be read — or reordered — apart.
///
/// The two remaining `env_required` reads stay HERE, at the point in the
/// sequence they were always at, rather than moving up beside the others: a
/// deployment with a bad `METRICS_LISTEN` must still be refused where it was
/// refused before.
pub async fn serve_until_drained(
    mut server: Server,
    tls: Option<ServeTls>,
    channel: Channel,
    watch_inputs: Inputs,
    schedule: Schedule,
) -> Result<(), Box<dyn std::error::Error>> {
    // The BINARY installs the exporter, never the library — a library that
    // installs one picks the backend for every service linking it. A failure here
    // is logged and ignored: a service that cannot export metrics should still
    // serve traffic, which is D25's rule applied to the metrics path too.
    // Named on the way out, for the reason given on PROJECT_DB_PORT above.
    let metrics_addr: SocketAddr = env_required("METRICS_LISTEN")?
        .parse()
        .map_err(|e| format!("METRICS_LISTEN is not a host:port address: {e}"))?;
    if let Err(e) = yadgar_telemetry::metrics::install_prometheus(metrics_addr) {
        tracing::warn!(error = %e, "metrics endpoint unavailable; continuing without it");
    }

    // AFTER THE EXPORTER, NEVER BEFORE IT. A value recorded before there is a
    // recorder is a value nobody ever sees.
    watch_inputs.export_not_after();

    // Named on the way out, for the reason given on PROJECT_DB_PORT above.
    let addr: SocketAddr = env_required("LISTEN")?
        .parse()
        .map_err(|e| format!("LISTEN is not a host:port address: {e}"))?;

    // ARMED BEFORE THE SERVER IS SPAWNED, and that ordering is the fix rather
    // than an accident of where the line sits. `yadgar_lifecycle::shutdown`
    // installs both signal handlers when it is CALLED — a SIGTERM arriving between here and
    // the first poll of the future would otherwise take the process's default
    // disposition and kill it outright.
    let signals = shutdown().map_err(|e| {
        format!("the SIGTERM and SIGINT handlers could not be installed: {e}. Refusing to start: a server that cannot hear SIGTERM cannot drain, and Kubernetes ends every pod with one")
    })?;

    tracing::info!(
        %addr,
        tls = tls.is_some(),
        watching = watch_inputs.watched().len(),
        rotation_poll_secs = schedule.poll().as_secs(),
        rotation_splay_max_secs = schedule.splay_max().as_secs(),
        drain_budget_secs = DRAIN_BUDGET.as_secs(),
        "project listening"
    );

    // THE SERVER IS SPAWNED AND ASKED TO STOP THROUGH A CHANNEL, rather than
    // handed the shutdown future directly, because the drain has to be BOUNDED
    // and a budget's clock must start when shutdown is REQUESTED. A `timeout`
    // around the serving future itself would bound the server's whole life
    // instead, and end the process one budget after boot, on every boot.
    let (ask_to_stop, stop_requested) = tokio::sync::oneshot::channel();
    let serving = tokio::spawn(
        server
            .add_service(ProjectServiceServer::new(Project::new(channel)))
            .serve_with_shutdown(addr, async {
                let _ = stop_requested.await;
            }),
    );
    let stop = async {
        tokio::select! {
            () = signals => {}
            () = rotate::watch(watch_inputs, schedule) => {}
        }
    };
    match drain_within(serving, ask_to_stop, stop, DRAIN_BUDGET).await {
        Drain::Finished(result) => result?,
        Drain::Overran => tracing::error!(
            budget_secs = DRAIN_BUDGET.as_secs(),
            "the drain did not finish within its budget; ending anyway with calls still in \
             flight. A request blocked this long is the thing to look at"
        ),
    }

    Ok(())
}
