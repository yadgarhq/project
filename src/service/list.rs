//! `ProjectService.ListProjects` — the registry load.
//!
//! Its own file for the same reason as `sync.rs`: it is the second verb of this
//! tier that decides who may call it. The gRPC handler stays with the others in
//! `ProjectService`'s impl, reads the caller's verified common name, and calls
//! [`Project::list_as`].

use tonic::{Request, Response, Status};
use yadgar_telemetry::estimator::Class;
use yadgar_telemetry::grpc::status_name;
use yadgar_telemetry::observe::{Call, Outcome};
use yadgar_telemetry::pb::yadgar::telemetry::v1::Kind;

use super::request_id::{carrying, request_id_of};
use super::{
    is_reserved_root, passthrough, tel_scope_joined, unattested_placeholder_scope, Project,
};
use crate::pb::yadgar::project::v1 as pb;

/// The ONE caller `ListProjects` admits: the common name on the gateway's
/// client leaf, as `platform` issues it (`gateway-client-tls`). Compared
/// EXACTLY, as [`super::SYNC_CALLER`] is.
///
/// **A CONSTANT, AS `SYNC_CALLER` IS, AND NOT A KNOB.** Which workload may
/// load the registry is policy, not configuration: no installation needs to
/// admit a different caller, and the name is the one `platform` writes on the
/// leaf. `project-db`'s own caller check pins its name the same way.
///
/// **WHY THE LOAD CHECKS ITS CALLER AT ALL (ledger 1643).** Since proto
/// v1.20.0 the load carries `RegisteredPath.owner_user_id`: the user id of
/// every private row's owner. Before this check, any workload the
/// NetworkPolicy admits, holding any leaf the internal CA issued, received
/// all of them. mTLS proves "one of ours"; this proves "which one". The gateway
/// is the load's only production caller.
pub const LIST_CALLER: &str = "gateway-caller";

impl Project {
    /// THE LOAD, for a caller whose identity has already been read.
    ///
    /// **THE GRPC HANDLER IS THE ONLY PRODUCTION CALLER**, and it passes the
    /// common name of the verified client leaf. The identity is a parameter so
    /// the check and the relay can be tested without a handshake, as
    /// [`Project::sync_as`] is. The whole `Request` is taken rather than its
    /// message, because the request id travels in its metadata.
    pub async fn list_as(
        &self,
        caller: Option<&str>,
        request: Request<pb::ProjectServiceListProjectsRequest>,
    ) -> Result<Response<pb::ProjectServiceListProjectsResponse>, Status> {
        // Started BEFORE the work, so the duration covers the handler — and as
        // the FIRST statement, which is what lets `observe-coverage` credit the
        // delegating gRPC handler with this record. So the header is read here
        // and again below, rather than once into a local ahead of the start.
        let call = Call::start(
            "project",
            "ListProjects",
            Kind::Read,
            tel_scope_joined(
                unattested_placeholder_scope().as_ref(),
                request_id_of(&request),
            ),
        );
        let request_id = request_id_of(&request);
        let req = request.into_inner();
        let scope = unattested_placeholder_scope();

        call.run(
            async move {
                // INSIDE the observed call, so a refusal is recorded (D67),
                // and BEFORE the relay, so a refused call never reaches the
                // store.
                admit(caller)?;
                let listed = self
                    .db
                    .clone()
                    .list_projects(carrying(
                        pb::ListProjectsRequest {
                            scope,
                            // THE WHOLE REGISTRY, NEVER A SLICE. `under_path` is the
                            // only field that could narrow it, and the caller of
                            // this rpc is loading the set it will resolve every
                            // future candidate against — a subtree would answer some
                            // of them wrongly and none of them detectably.
                            under_path: String::new(),
                            // NOT FILTERED BY STATUS, AND IT MUST NOT BE.
                            // `RegisteredPath`'s own comment: the store's resolution
                            // walks live paths and aliases without consulting status
                            // and reports the status it found, so a caller that
                            // dropped archived rows from its own walk would resolve
                            // a candidate to a different ancestor than
                            // `ResolveProject` resolves it to — two answers for one
                            // path, differing only in which route the caller took.
                            status: None,
                            page_size: req.page_size,
                            page_token: req.page_token,
                        },
                        &request_id,
                    ))
                    .await
                    .map_err(|e| passthrough(e, "list"))?
                    .into_inner();

                let paths = listed
                    .projects
                    .into_iter()
                    // The bare reserved segment, dropped as a path here and as
                    // an alias below. See [`is_reserved_root`].
                    .filter(|project| !is_reserved_root(&project.path))
                    .map(registered)
                    .collect();

                Ok(pb::ProjectServiceListProjectsResponse {
                    paths,
                    // VERBATIM, and never recomputed from the rows that
                    // survived the filter. The store's token is a keyset cursor
                    // over ITS last row; deriving one from a filtered page would
                    // skip every row between the two. Nor does this handler loop
                    // on the caller's behalf — the contract puts that obligation
                    // on the caller, and a set truncated here is not a smaller
                    // registry but a wrong one.
                    next_page_token: listed.next_page_token,
                })
            },
            |r| Outcome {
                status: "OK",
                payload: list_payload(r),
                encoded_bytes: Some(prost::Message::encoded_len(r) as u64),
                class: Class::Envelope,
                // The row count for a list is the LIST, not one.
                rows: r.paths.len() as u32,
                ..Default::default()
            },
            status_name,
        )
        .await
        .map(Response::new)
    }
}

/// One stored row as the load reports it.
///
/// **FOUR FIELDS, AND THE FOURTH COMES FROM `Meta`.** `display_name`,
/// `last_seen_at` and the rest of `Meta` are not on `RegisteredPath` and are not
/// put on the wire: this rpc answers what resolves and who owns a private row,
/// not what a project is.
fn registered(project: pb::Project) -> pb::RegisteredPath {
    pb::RegisteredPath {
        path: project.path,
        status: project.status,
        // A rename is an ALIAS and never a rewrite (D53), so a walk consulting
        // live paths only would strand every descendant of a renamed parent.
        // The store binds its ancestor chain against live paths AND aliases; a
        // walk over this set is the same walk and needs both inputs.
        //
        // So the reserved-root guard runs on both inputs too (ledger 764): the
        // row stays, its alias at the bare segment does not.
        aliases: project
            .aliases
            .into_iter()
            .filter(|alias| !is_reserved_root(alias))
            .collect(),
        // THE OWNER OF A PRIVATE ROW, AS THE STORE HOLDS IT (proto v1.20.0,
        // ADR-0947). `""` on every org row, and `""` when the store sent no
        // `Meta` — which the contract tells the gateway to read, on a private
        // row, as owned by nobody. Never derived here from the path or from
        // `created_by`: D12 calls those audit identity, distinct from
        // ownership.
        owner_user_id: project
            .meta
            .map(|meta| meta.owner_user_id)
            .unwrap_or_default(),
    }
}

/// The check that makes the load the gateway's alone. `None` (no TLS, no
/// client certificate, an ambiguous subject) is a refusal, as it is on
/// `SyncProjects`.
fn admit(caller: Option<&str>) -> Result<(), Status> {
    if caller == Some(LIST_CALLER) {
        return Ok(());
    }
    Err(Status::permission_denied(format!(
        "ListProjects is callable by the gateway only (ledger 1643): the client certificate \
         must name {LIST_CALLER:?}, and this one names {}. Every module's leaf verifies \
         against the same authority, so verification alone admits nobody here",
        caller.map_or_else(|| "no caller".to_string(), |c| format!("{c:?}"))
    )))
}

/// The text the telemetry crate measures for the WORD count of a list: the
/// paths, one word each, and nothing else.
///
/// **NOT `{r:?}` (ledger 1643).** The response carries every private row's
/// owner id, and a rendering of the whole of it would put those ids in the
/// payload. The telemetry crate (v0.2.0) uses the payload only to count bytes
/// and words — the bytes come from `encoded_bytes` here, and no text reaches
/// the record — so the paths alone are enough, and an owner id never enters
/// the string, so a later telemetry change cannot leak one. The row count is
/// `Outcome.rows`.
pub(super) fn list_payload(r: &pb::ProjectServiceListProjectsResponse) -> String {
    r.paths
        .iter()
        .map(|p| p.path.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}
