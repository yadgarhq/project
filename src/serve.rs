//! The transport this service SERVES on — the other half of [`crate::upstream`].
//!
//! `upstream` decides how this service dials `project-db`; this decides what a
//! caller reaching this service gets.
//!
//! # TLS, and client authentication, from `yadgar-lifecycle` (ADR-0846)
//!
//! **ONE IMPLEMENTATION OF A SECURITY CONTROL.** This module held its own
//! `ServeTls` — a near-identical copy of the one in `iam`, `iam-db`, `task`,
//! `task-db` and `project-db` — and none of the six called
//! `ServerTlsConfig::client_ca_root`, so no hop in the estate verified a
//! client certificate. The type is lifted into
//! [`yadgar_lifecycle::serve_tls`] and adopted here (B-U5, ledger 925): six
//! copies of a verifier are how six services come to verify six slightly
//! different things.
//!
//! **What this binary reads, and none of it has a default** (ADR-0845,
//! ADR-0854):
//!
//! | variable | chart value | values |
//! | --- | --- | --- |
//! | `LISTEN_TLS_ENABLED` | `tls.enabled` | exactly `1` or `0` |
//! | `LISTEN_TLS_CERT_FILE`, `LISTEN_TLS_KEY_FILE` | `tls.certSecret` | paths |
//! | `LISTEN_TLS_CLIENT_AUTH` | `tls.clientAuth` | exactly `off`, `optional`, `required` |
//! | `LISTEN_TLS_CLIENT_CA_FILE` | `tls.clientCaSecret` | a path |
//!
//! An absent switch or an absent mode refuses the boot, naming the variable
//! AND the chart key. `off` is the emergency value for client auth; deleting
//! the variable is a refusal, not a way to turn verification off. `optional`
//! verifies a certificate a caller presents and admits a caller presenting
//! none — a staging step, not a control. `required` refuses a caller without
//! a certificate the mounted authority issued.
//!
//! **Configuration is file paths, never an issuer-specific resource** (D80).
//! cert-manager writes them in the reference deployment and a hand-assembled
//! Secret anywhere else; nothing here can tell the difference.
//!
//! **A misconfiguration refuses the boot; it never opens a plaintext
//! listener.** [`builder`] is the ONLY place in this binary that constructs a
//! server, so a cleartext fallback cannot be reached by forgetting something,
//! only by adding it.
//!
//! # Shutdown moved to `yadgar-lifecycle`
//!
//! [`yadgar_lifecycle::shutdown`], [`yadgar_lifecycle::DRAIN_BUDGET`] and
//! [`yadgar_lifecycle::drain_within`] were three items in this module, and the
//! same three in `iam` and in `gateway`. They are one decision rather than
//! three: `terminationGracePeriodSeconds` bounds a drain KUBELET started, the
//! rotation watcher ends the serve on its own, so kubelet's clock never runs and
//! a budget this process holds is the only thing bounding what follows.

use tonic::transport::Server;

pub use yadgar_lifecycle::serve_tls::{ClientAuth, ServeTlsError, ServerTls, LISTEN};

/// The chart values block this listener's keys render from. The shared type
/// appends the leaf to it, so a refusal names `tls.enabled`, `tls.clientAuth`,
/// `tls.certSecret` or `tls.clientCaSecret` beside the variable (ADR-0845).
///
/// **ONE SOURCE, READ BY `boot.rs` AND BY EVERY TEST THAT NAMES IT.** A string
/// literal typed out at the call site and again in a test could diverge from
/// the one `boot.rs` actually passes with nothing to notice.
pub const CHART_KEY: &str = "tls";

/// Build the gRPC server this service listens with.
///
/// **THE ONLY SERVER CONSTRUCTION IN THIS BINARY, and that is structural rather
/// than stylistic.** `None` is the cleartext listener, and it is only ever the
/// answer [`ServerTls::from_env`] gives an EXPLICIT `LISTEN_TLS_ENABLED=0` with
/// client auth `off`. `Some` is TLS — with the caller verified when the mode
/// asks for it — or an error, never a cleartext server.
///
/// **EAGER.** Every file is read and the rustls acceptor and client verifier
/// are built here, so a bad mount refuses at boot rather than failing a
/// stranger's first handshake. The error keeps tonic's reason as its
/// `source()`; `boot.rs` flattens the chain (ADR-0591).
///
/// **ALPN is tonic's, not ours.** `ServerTlsConfig` pushes `h2` onto the
/// acceptor's protocol list; the handshake tests in `tests/serve_tls.rs` fail
/// if it ever stops being offered, because tonic's own client refuses a channel
/// that negotiated anything else.
pub fn builder(tls: Option<&ServerTls>) -> Result<Server, ServeTlsError> {
    yadgar_lifecycle::serve_tls::server(tls)
}

#[cfg(test)]
mod tests;
