//! Everything that must be right BEFORE a socket of any kind is opened.
//!
//! **Declared by `main.rs`, and NOT part of `yadgar_project`.** This file sits
//! beside four library modules while belonging to neither crate root but the
//! binary's: `lib.rs` does not declare it, so nothing outside this binary can
//! reach it and adding it changed no public surface.
//!
//! **THE SEAM IS THE FIRST SOCKET, and it is the file's own.** Everything here
//! reads a file or an environment variable and validates what it read; nothing
//! here dials, binds, or spawns. `main` picks up at the dial to `project-db`,
//! which is the first line in the sequence that can fail for a reason a restart
//! would not fix. Splitting there keeps the property `main.rs` already
//! documents — a deployment that got a mount or a knob wrong exits before
//! anything is listening — expressible as one call rather than as an ordering a
//! reader has to reconstruct.
//!
//! **WHAT IS DELIBERATELY LEFT BEHIND.** `METRICS_LISTEN` and `LISTEN` are read
//! and parsed in `main`, AFTER the dial, exactly where they were. They are
//! configuration and they would fit here, but moving them would make a
//! deployment with a malformed `METRICS_LISTEN` fail before the dial instead of
//! after it. That is a change to which error an operator sees first, which is
//! behaviour, and this split changes none.

use tonic::transport::Server;

use yadgar_project::rotate;
use yadgar_project::serve::{self, ServeTls, LISTEN};
use yadgar_project::upstream::{UpstreamTls, PROJECT_DB};

// `main.rs`'s own helper, reached through the binary's crate root: this module
// is its descendant, so the private item is in scope. It stays there because
// `main` still reads `METRICS_LISTEN` and `LISTEN` through it.
use crate::env_required;

/// The resolved inputs, handed to `main` in the order `main` consumes them.
///
/// **A struct rather than a tuple** because seven values of which two are
/// `Option`s and two are `u16`-or-`String` is a call site nobody can read, and
/// because a field gained later is a compile error at the destructuring rather
/// than a silently reordered pair.
pub struct Prepared {
    /// Not `mut` here — `main` rebinds it mutably to add the service.
    pub server: Server,
    /// Retained past its use below so `main` can log whether the listener is
    /// encrypted.
    pub tls: Option<ServeTls>,
    pub db_host: String,
    pub db_port: u16,
    pub db_tls: Option<UpstreamTls>,
    pub watch_inputs: rotate::Inputs,
    pub schedule: rotate::Schedule,
}

/// Read and CHECK every input, in the order `main` read them in.
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
    // STRINGIFIED AND NAMED, for the same reason as every other error in this
    // function: `main` returns `Box<dyn Error>`, which Rust prints with DEBUG. A
    // bare `?` here yields `ParseIntError { kind: InvalidDigit }`, and the two
    // addresses below yield `AddrParseError(())` — a CrashLoop whose entire
    // output is `AddrParseError(())` tells an operator neither which variable was
    // wrong nor what it held. These three were the last bare `?`s left beside the
    // comments explaining why nothing else is one.
    let db_port: u16 = env_required("PROJECT_DB_PORT")?
        .parse()
        .map_err(|e| format!("PROJECT_DB_PORT is not a port number: {e}"))?;

    // OPT-IN, and OFF unless a deployment asks for it. Nothing configured means
    // the cleartext dial this service has always done — no server in the estate
    // serves TLS yet, so the cut-over is a later change that can be reverted on
    // its own.
    //
    // `.to_string()` on the way out, and not decoration: `main` returns
    // `Box<dyn Error>`, which Rust prints with DEBUG — so a bare `?` would put
    // `NoCaFile("PROJECT_DB")` on the operator's terminal instead of the sentence
    // saying which variable is missing and why cleartext is not the answer. The
    // same reason the gateway stringifies `Limits::parse`.
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
    // test spawns this binary, so deleting either compiled and passed
    // everything. The list lives in `rotate::watch_set` now and
    // `tests/assembly.rs` calls it.
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
