//! `ProjectService`. Business rules here; storage over the `-db` API, never
//! directly.
//!
//! **THREE RPCS, AND THE OTHER FIVE ARE ABSENT FOR STATED REASONS.**
//! `project.proto` gives them in its own words and they are not restated here:
//! `RegisterProject`, `RenameProject` and `ArchiveProject` are absent by
//! decision (ADR-0705, kept by ADR-0936); `GetProject` has no caller; and
//! `TouchProjects` is a call this tier MAKES rather than a verb it EXPOSES, and
//! is staged behind `queue`. Adding any of them is a contract change first —
//! they are on no `ProjectService` at any tag, so there is nothing to
//! implement.
//!
//! **`SyncProjects` IS THE ONE WRITE VERB, AND THE CALLER CHECK IS THE POLICY.**
//! Only the seeder may call it (ADR-0936): its verified leaf must name
//! [`SYNC_CALLER`] and its Scope must carry [`SEEDER_PRINCIPAL`] (ADR-0942).
//! Everything else about the sync — validation, the transaction, the
//! mass-archive guard — belongs to the store, and this tier relays it.
//!
//! **`ListProjects` CHECKS ITS CALLER TOO (ledger 1643).** The load carries the
//! owner of every private row, so only the gateway's leaf may call it — see
//! [`LIST_CALLER`]. `ResolveProject` checks no caller.
//!
//! **THE TIER IS NOT A PROXY, AND `ListProjects` IS WHERE THAT SHOWS.**
//! `ResolveProject` is a pass-through on the response — the two response
//! messages are field-for-field identical and are separate types only because
//! buf's `RPC_REQUEST_RESPONSE_UNIQUE` refuses to let two rpcs share one. What
//! the tier owns is the REQUEST it sends, and, on the list, three obligations
//! the contract states on the response: the bare reserved segment is dropped,
//! the set is not filtered by status, and the page token is forwarded verbatim.
//! The list also relays each private row's owner from `Meta.owner_user_id`.

use tonic::{Request, Response, Status};
use yadgar_dial::Deadline;
use yadgar_lifecycle::serve_tls::caller_common_name;
use yadgar_telemetry::estimator::Class;
use yadgar_telemetry::grpc::status_name;
use yadgar_telemetry::observe::{Call, Outcome};
use yadgar_telemetry::pb::yadgar::telemetry::v1::Kind;

use crate::pb::yadgar::common::v1::Scope;
use crate::pb::yadgar::project::v1 as pb;
use crate::pb::yadgar::project::v1::project_db_service_client::ProjectDbServiceClient;
use crate::pb::yadgar::project::v1::project_service_server::ProjectService;

/// The one segment RESERVED from the organisation namespace, and the value
/// `project-db/src/path.rs` calls `RESERVED_ROOT`.
///
/// It is duplicated rather than imported because the two repositories share no
/// crate — D4 keeps `project-db`'s code out of this dependency tree — and the
/// contract states the obligation on THIS tier in its own words:
/// `ProjectServiceListProjectsResponse` says the bare segment "is never in this
/// set", and calls emitting it this tier's half of a guard the store can only
/// half-close. The store refuses to MINT such a row and drops the segment from
/// its ancestor chain; a code-only guard does not delete a row an earlier build
/// already accepted, so the row can still arrive here.
const RESERVED_ROOT: &str = "local";

/// Whether a listed path, or one of a listed row's aliases, is the bare
/// reserved segment and must be dropped.
///
/// **PATHS AND ALIASES, BOTH (ledger 764).** `project-db/src/read.rs` drops the
/// segment from the ancestor chain it binds against live paths AND aliases, so
/// one line there closes both arms. A walk over this set is the same walk and
/// consults both inputs, so an alias at `local` re-opens the hole exactly as a
/// live path there would.
///
/// **THE COMPARISON FOLDS ASCII CASE, and both halves of that matter.**
/// `project.path` carries `COLLATE utf8mb4_general_ci`, declared by
/// `project-db`'s `create_project` migration (migration 1) and reasserted by
/// `pin_the_path_collation` (migration 4), which is still the formal
/// declaration for `project_alias.alias_path`. `project-db`'s own guard is
/// `eq_ignore_ascii_case`. A byte-exact filter here would let `LOCAL` into a set
/// the store's ancestor walk excludes, and the two walks would then answer one
/// candidate differently depending on which route the caller took. The segment
/// alphabet is `[A-Za-z0-9._-]`, so ASCII case is the whole of what that
/// collation folds.
///
/// **ONLY THE BARE SEGMENT.** `local/home` IS the private class the segment is
/// reserved to carry, and dropping it would delete that class rather than
/// protect it. `locals` is a different organisation that merely shares five
/// characters.
fn is_reserved_root(path: &str) -> bool {
    path.eq_ignore_ascii_case(RESERVED_ROOT)
}

/// Copy the scope fields a record needs.
///
/// On the two read verbs this tier holds no `Scope`, so every field is empty,
/// and [`tel_scope_joined`] supplies the `request_id` from the header instead.
/// It is still built, and built at the one place `task` builds its own, so
/// the shape of the seam is the estate's rather than this module's — and so
/// that the day a real `Scope` reaches this tier, the telemetry follows it with
/// no second change.
fn tel_scope(scope: Option<&Scope>) -> yadgar_telemetry::observe::Scope {
    yadgar_telemetry::observe::Scope {
        request_id: scope.map(|s| s.request_id.clone()).unwrap_or_default(),
        instance_id: scope.map(|s| s.instance_id.clone()).unwrap_or_default(),
        user_id: scope.map(|s| s.user_id.clone()).unwrap_or_default(),
        project_id: scope.map(|s| s.project_id.clone()).unwrap_or_default(),
    }
}

/// [`tel_scope`], with the header's id standing in when the `Scope` carries
/// none (ledger 1254).
///
/// **THE `Scope`'S OWN ID WINS WHEN IT HAS ONE.** On the two read verbs there is
/// no `Scope`, so the header is the id. The order exists
/// for the day a real `Scope` reaches this tier: an attested id is the stronger
/// claim, and `project-db` resolves the two in the same order.
fn tel_scope_joined(scope: Option<&Scope>, header: String) -> yadgar_telemetry::observe::Scope {
    let mut tel = tel_scope(scope);
    if tel.request_id.is_empty() {
        tel.request_id = header;
    }
    tel
}

#[derive(Clone)]
pub struct Project {
    db: ProjectDbServiceClient<tonic::transport::Channel>,
    /// The request timeout the channel was dialled with (ADR-1019): the most any
    /// one `project-db` call may be given. A caller's deadline can only shorten
    /// it.
    ceiling: std::time::Duration,
}

impl Project {
    /// `ceiling` is the `project-db` request timeout the chart sets, the value
    /// `upstream::request_timeout_from_env` read and `upstream::connect` dialled
    /// with. Every call to the store is bounded by `min(ceiling, caller's
    /// remaining time)`.
    pub fn new(channel: tonic::transport::Channel, ceiling: std::time::Duration) -> Self {
        Self {
            db: ProjectDbServiceClient::new(channel),
            ceiling,
        }
    }
}

pub use list::LIST_CALLER;
pub use sync::{SEEDER_PRINCIPAL, SYNC_CALLER};

mod list;
mod request_id;
mod sync;

use request_id::{carrying, request_id_of};

/// A `-db` failure is passed through with its CODE intact but not its message.
///
/// The code is the caller's contract — `NOT_FOUND` on this API means what
/// `project.proto` says it means, "no registered ancestor", and it says so on
/// THIS contract precisely so a consumer need not read the store's half. The
/// message is not passed through: it may name tables and columns, and a client
/// of the public API has no business seeing the storage layer's vocabulary.
///
/// It IS logged, and that is not in tension with withholding it. Keeping the
/// message out of the RESPONSE is the point; discarding it altogether leaves the
/// operator with a code and no reason. The log never reaches the client.
///
/// The field is `db_message` rather than `message` because the event's own text
/// is already emitted under `message`, and a JSON object carrying one key twice
/// loses one of the two values with nothing to say which.
fn passthrough(status: Status, op: &str) -> Status {
    tracing::warn!(
        op,
        code = ?status.code(),
        db_message = status.message(),
        "project-db returned an error"
    );
    match status.code() {
        // NO REGISTERED ANCESTOR. `project.proto` states the rule on
        // `ResolveProject` and adds the hazard: IT IS A CALLER ERROR AND MUST
        // NOT BE RENDERED AS `401`. The credential is fine; a workspace fact is
        // missing or names nothing.
        tonic::Code::NotFound => {
            Status::not_found("no registered project is that path or an ancestor of it")
        }
        // The candidate path was malformed, was the reserved segment, or the
        // page token was not one the store issued. Which of those it was is the
        // store's vocabulary; that the request was the problem is not.
        tonic::Code::InvalidArgument => Status::invalid_argument("invalid request"),
        // THE STORE WAS UNREACHABLE, WHICH IS THE ONE FAILURE THIS SERVICE
        // ALREADY PROMISES TO REPORT AS ITSELF. The boot dial is LAZY
        // (ADR-0532), so this pod is Ready before `project-db` is, and the whole
        // justification for that is that a request arriving in the gap fails
        // RECOVERABLY. Collapsing it into `INTERNAL` would make the recoverable
        // failure indistinguishable from a bug, and no client would retry it.
        //
        tonic::Code::Unavailable => {
            Status::unavailable("the project registry could not be reached in time — retry")
        }
        // DEADLINE_EXCEEDED STAYS ITSELF (ADR-1019). The store's time ran out,
        // and either the caller's deadline set it (`Deadline::call` labels its
        // own cut-off this way) or the store's own did. A caller that reads
        // UNAVAILABLE retries into a budget that is already spent; one that
        // reads DEADLINE_EXCEEDED knows it is.
        tonic::Code::DeadlineExceeded => {
            Status::deadline_exceeded("the project registry did not answer in time")
        }
        _ => Status::internal("the project registry is unavailable"),
    }
}

#[tonic::async_trait]
impl ProjectService for Project {
    /// THE REGISTRY LOAD. A caller answers "what does this path resolve to" in
    /// memory rather than over the network on every request, and this is where
    /// it gets the set to answer from. See [`Project::list_as`].
    async fn list_projects(
        &self,
        request: Request<pb::ProjectServiceListProjectsRequest>,
    ) -> Result<Response<pb::ProjectServiceListProjectsResponse>, Status> {
        // READ BEFORE THE REQUEST IS CONSUMED, as `sync_projects` reads it: the
        // peer chain lives on the request's extensions.
        let caller = caller_common_name(&request);
        self.list_as(caller.as_deref(), request).await
    }

    /// THE FALLBACK, not the hot path — asked when the caller has no loaded set
    /// to walk. A pass-through on the response, and `exact` is the field that
    /// must survive it: `false` is neither an error nor a detail but the whole
    /// safety property that makes resolving upward acceptable instead of
    /// refusing.
    async fn resolve_project(
        &self,
        request: Request<pb::ProjectServiceResolveProjectRequest>,
    ) -> Result<Response<pb::ProjectServiceResolveProjectResponse>, Status> {
        let request_id = request_id_of(&request);
        // READ BEFORE THE REQUEST IS CONSUMED, as the request id is (ADR-1019).
        let deadline = Deadline::of(&request);
        let req = request.into_inner();
        let call = Call::start(
            "project",
            "ResolveProject",
            Kind::Read,
            tel_scope_joined(None, request_id.clone()),
        );

        call.run(
            async move {
                let store = carrying(
                    pb::ResolveProjectRequest {
                        // NO `Scope`: this tier holds none on this rpc, and
                        // `project-db`'s reads accept its absence (ledger 762).
                        scope: None,
                        candidate_path: req.candidate_path,
                    },
                    &request_id,
                );
                let mut db = self.db.clone();
                let resolved = deadline
                    .call(self.ceiling, store, |r| db.resolve_project(r))
                    .await
                    .map_err(|e| passthrough(e, "resolve"))?
                    .into_inner();

                // FIELD FOR FIELD. The two messages are separate types only
                // because buf refuses to let two rpcs share a response type, so
                // anything changed here would be this tier inventing an answer
                // the store did not give.
                Ok(pb::ProjectServiceResolveProjectResponse {
                    resolved_path: resolved.resolved_path,
                    exact: resolved.exact,
                    via_alias: resolved.via_alias,
                    status: resolved.status,
                    // Crosses unchanged, including empty. `source_repo` is a
                    // bare proto3 `string`, so an unpopulated value arrives as
                    // `""`, not absent, and `""` means the resolved row is
                    // PRIVATE-class or predates the column — a legitimate
                    // answer, not a gap this tier should paper over. The
                    // gateway is the consumer and decides what an empty value
                    // means; do not substitute `resolved.resolved_path` or any
                    // other value here.
                    source_repo: resolved.source_repo,
                })
            },
            |r| Outcome {
                status: "OK",
                payload: format!("{r:?}"),
                encoded_bytes: Some(prost::Message::encoded_len(r) as u64),
                class: Class::Envelope,
                rows: 1,
                ..Default::default()
            },
            status_name,
        )
        .await
        .map(Response::new)
    }

    /// THE SEEDER'S ONE WRITE VERB (ADR-0936): authorise, then relay the whole
    /// declaration to `ProjectDbService.SyncProjects` in one call. This tier
    /// holds no registry cache, so a committed sync is what every replica's
    /// next `ListProjects` relays — there is nothing here to refresh.
    async fn sync_projects(
        &self,
        request: Request<pb::ProjectServiceSyncProjectsRequest>,
    ) -> Result<Response<pb::ProjectServiceSyncProjectsResponse>, Status> {
        // READ BEFORE THE REQUEST IS CONSUMED: the peer chain lives on the
        // request's extensions, and `into_inner` drops them.
        let caller = caller_common_name(&request);
        let deadline = Deadline::of(&request);
        let req = request.into_inner();
        let call = Call::start(
            "project",
            "SyncProjects",
            Kind::Write,
            tel_scope(req.scope.as_ref()),
        );

        call.run(
            async move { self.sync_as(caller.as_deref(), deadline, req).await },
            |r| Outcome {
                status: "OK",
                payload: format!("{r:?}"),
                encoded_bytes: Some(prost::Message::encoded_len(r) as u64),
                class: Class::Envelope,
                // Every declared row lands in exactly one count, and
                // `archived` counts the rows the declaration omitted.
                rows: r.inserted + r.updated + r.archived + r.reactivated + r.unchanged,
                ..Default::default()
            },
            status_name,
        )
        .await
        .map(Response::new)
    }
}

#[cfg(test)]
mod tests;
