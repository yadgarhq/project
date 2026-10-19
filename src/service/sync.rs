//! `ProjectService.SyncProjects` — the seeder's one write verb (ADR-0936).
//!
//! Its own file because it is the one place this tier WRITES and the one place
//! it decides who may call it: the read verbs in the parent module relay
//! whatever the gateway asks, and this verb refuses everyone but the seeder.
//! The gRPC handler stays with the others in `ProjectService`'s impl and calls
//! [`Project::sync_as`].

use tonic::Status;

use super::Project;
use crate::pb::yadgar::common::v1::Scope;
use crate::pb::yadgar::project::v1 as pb;

impl Project {
    /// THE SYNC, for a caller whose identity has already been read.
    ///
    /// **THE GRPC HANDLER IS THE ONLY PRODUCTION CALLER**, and it passes the
    /// common name of the verified client leaf. The identity is a parameter so
    /// the authorisation and the relay can be tested without a handshake; over
    /// the wire there is no way to supply it except by presenting the
    /// certificate. `project-db`'s `sync_as` has the same shape, for the same
    /// reason.
    ///
    /// **AUTHORISE, THEN RELAY, AND NOTHING ELSE.** No validation of the
    /// declaration happens here: the store owns it (path grammar, class, roots,
    /// duplicates, the mass-archive guard), and a second copy in this tier
    /// would drift from the one that decides.
    pub async fn sync_as(
        &self,
        caller: Option<&str>,
        req: pb::ProjectServiceSyncProjectsRequest,
    ) -> Result<pb::ProjectServiceSyncProjectsResponse, Status> {
        admit(caller, req.scope.as_ref())?;

        // EXHAUSTIVE, so a field added to the contract fails to compile here
        // rather than being dropped on the relay without a word.
        let pb::ProjectServiceSyncProjectsRequest {
            scope,
            roots,
            projects,
            allow_mass_archive,
        } = req;
        let counts = self
            .db
            .clone()
            .sync_projects(pb::SyncProjectsRequest {
                // THE SEEDER'S SCOPE, AS RECEIVED. Its `request_id` was minted
                // by the seeder and is relayed unchanged so D67's records join
                // across both hops (ADR-0942).
                scope,
                roots,
                projects,
                allow_mass_archive,
            })
            .await
            .map_err(relay_refusal)?
            .into_inner();

        // EXHAUSTIVE for the same reason as the request.
        let pb::SyncProjectsResponse {
            inserted,
            updated,
            archived,
            reactivated,
            unchanged,
        } = counts;
        Ok(pb::ProjectServiceSyncProjectsResponse {
            inserted,
            updated,
            archived,
            reactivated,
            unchanged,
        })
    }
}

/// The ONE caller `SyncProjects` admits: the common name on the seeder's
/// client leaf, as `platform` issues it (`seeder-client-tls`, ADR-0936).
/// Compared EXACTLY: the name `platform` writes is lower-case, and a leaf
/// spelled otherwise is not that leaf.
pub const SYNC_CALLER: &str = "seeder-caller";

/// The seeder's fixed system principal (ADR-0942): the `scope.user_id` a
/// seeder call must carry, and the value the store stamps as `created_by` and
/// `updated_by`. The seeder's repository shares no crate with this one, so it
/// pins the same literal in its own suite.
pub const SEEDER_PRINCIPAL: &str = "yadgar:system:seeder";

/// The authorisation step ADR-0936 places in this tier. Both checks, and
/// neither one trusted alone (ADR-0942):
///
/// 1. **The caller.** The verified client leaf's common name is
///    [`SYNC_CALLER`]. mTLS proves "one of ours" — every module's leaf
///    verifies against the same authority — and this proves "which one".
///    `None` (no TLS, no client certificate, an ambiguous subject) is a
///    refusal.
/// 2. **The principal.** `scope.user_id` is [`SEEDER_PRINCIPAL`]. An absent
///    Scope carries no principal and is refused the same way: the request is
///    not malformed so much as not the seeder's.
///
/// Both refusals are `PERMISSION_DENIED`, and both run before the relay, so a
/// refused call never reaches the store.
fn admit(caller: Option<&str>, scope: Option<&Scope>) -> Result<(), Status> {
    if caller != Some(SYNC_CALLER) {
        return Err(Status::permission_denied(format!(
            "SyncProjects is callable by the seeder only (ADR-0936): the client certificate \
             must name {SYNC_CALLER:?}, and this one names {}. Every module's leaf verifies \
             against the same authority, so verification alone admits nobody here",
            caller.map_or_else(|| "no caller".to_string(), |c| format!("{c:?}"))
        )));
    }
    let principal = scope.map(|s| s.user_id.as_str());
    if principal != Some(SEEDER_PRINCIPAL) {
        return Err(Status::permission_denied(format!(
            "SyncProjects from the seeder's leaf must carry scope.user_id {SEEDER_PRINCIPAL:?} \
             (ADR-0942), and this call carries {}",
            principal.map_or_else(|| "no scope".to_string(), |p| format!("{p:?}"))
        )));
    }
    Ok(())
}

/// A store refusal on `SyncProjects`, relayed with its CODE AND ITS MESSAGE
/// unchanged — unlike `super::passthrough`, which withholds the message on the two
/// read verbs.
///
/// **THE CONTRACT SAYS SO, AND THE CALLER IS WHY.** `ProjectService.SyncProjects`
/// relays the store's refusals "with their code and message unchanged". The
/// caller is the seeder, an internal Job, not a public client: its log line is
/// the operator's only view of WHICH declared path the store refused, and
/// `INVALID_ARGUMENT` without the path is a refusal nobody can act on.
///
/// A NEW `Status` from the code and the message, not the client-side one
/// itself: that one carries the store hop's response metadata, which is not
/// this hop's to forward.
fn relay_refusal(status: Status) -> Status {
    tracing::warn!(
        op = "sync",
        code = ?status.code(),
        db_message = status.message(),
        "project-db refused the sync"
    );
    Status::new(status.code(), status.message().to_owned())
}
