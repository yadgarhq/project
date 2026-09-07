//! `ProjectService`. Business rules here; storage over the `-db` API, never
//! directly.
//!
//! **TWO RPCS, AND THE OTHER FIVE ARE ABSENT FOR STATED REASONS.**
//! `project.proto` gives them in its own words and they are not restated here:
//! `RegisterProject`, `RenameProject` and `ArchiveProject` are administrative
//! and reached by port-forwarding `project-db`; `GetProject` has no caller; and
//! `TouchProjects` is a call this tier MAKES rather than a verb it EXPOSES, and
//! is staged behind `queue`. Adding any of them is a contract change first —
//! they are on no `ProjectService` at any tag, so there is nothing to
//! implement.
//!
//! **THE TIER IS NOT A PROXY, AND `ListProjects` IS WHERE THAT SHOWS.**
//! `ResolveProject` is a pass-through on the response — the two response
//! messages are field-for-field identical and are separate types only because
//! buf's `RPC_REQUEST_RESPONSE_UNIQUE` refuses to let two rpcs share one. What
//! the tier owns is the REQUEST it sends, and, on the list, three obligations
//! the contract states on the response: the bare reserved segment is dropped,
//! the set is not filtered by status, and the page token is forwarded verbatim.

use tonic::{Request, Response, Status};
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

/// Whether a listed path is the bare reserved segment and must be dropped.
///
/// **THE COMPARISON FOLDS ASCII CASE, and both halves of that matter.**
/// `project.path` is `utf8mb4` with no `COLLATE`, so it takes the server default
/// — measured `utf8mb4_uca1400_ai_ci` — and `project-db`'s own guard is
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

/// The `Scope` this tier sends to the store, and it ATTESTS NOTHING.
///
/// **READ THIS BEFORE COPYING IT ANYWHERE.** `project-db/src/sql.rs`'s
/// `scope_of` refuses an ABSENT `Scope` with the message "scope is required and
/// is attested by the gateway", and **that sentence is false for this caller**.
/// No gateway attested this value. It is `Scope::default()` — four empty strings
/// — minted here because the store's check is `Option::as_ref` plus
/// `ok_or_else` and nothing more: a presence check, not an authorisation one.
/// Nothing in `project-db/src/read.rs` filters, keys or partitions on the value;
/// all three call sites bind it as `let _scope = scope_of(&req.scope)?;` and
/// never read it.
///
/// **NEITHER `ProjectService` REQUEST CARRIES A `Scope`, AND THAT IS THE
/// POINT.** `ListProjects`'s caller is a BOOT LOAD holding no token, no header
/// and no request; `ResolveProject` is asked by the gateway WHILE CONSTRUCTING
/// the estate's only `Scope`, so a `Scope`-carrying request would force a second
/// provisional literal. There is therefore nothing here to forward, and this is
/// a placeholder standing where a real value cannot yet exist.
///
/// **THE RULING THAT REPLACES IT IS OPEN.** `plans/the-project-logic-tier.md` §6
/// names three routes and picks none: (a) the tier mints one — this, made
/// deliberate; (b) both `ProjectService` requests gain a `Scope`, overruling the
/// argument the contract file itself makes; (c) `ProjectDbService`'s `list` and
/// `resolve` stop demanding one, which deletes a control that today refuses a
/// caller who reached the store without passing the gateway. Whichever is ruled,
/// it is ONE edit here, which is why this is a named function rather than
/// `Default::default()` at two call sites.
///
/// **AND THE TELEMETRY COST IS THE SAME EITHER WAY, so it does not choose
/// between the routes.** `project-db`'s `tel_scope` resolves an absent field
/// with `unwrap_or_default()`, so a minted default and a deleted check both
/// leave D67's `request_id` join empty for these two RPCs. `request_id` is read
/// only out of the `Scope` MESSAGE and never out of gRPC metadata, and neither
/// request carries a `Scope` at all — so nothing this repository can do fills
/// it.
fn unattested_placeholder_scope() -> Option<Scope> {
    Some(Scope::default())
}

/// Copy the scope fields a record needs.
///
/// The `Scope` this tier holds is the placeholder above, so every field is
/// empty. It is still built, and built at the one place `task` builds its own,
/// so the shape of the seam is the estate's rather than this module's — and so
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

#[derive(Clone)]
pub struct Project {
    db: ProjectDbServiceClient<tonic::transport::Channel>,
}

impl Project {
    pub fn new(channel: tonic::transport::Channel) -> Self {
        Self {
            db: ProjectDbServiceClient::new(channel),
        }
    }
}

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
        // DEADLINE_EXCEEDED belongs with it rather than with the opaque arm: the
        // store did not refuse the work, it did not answer in time — a transient
        // condition of the same shape, and one a caller acts on identically.
        tonic::Code::Unavailable | tonic::Code::DeadlineExceeded => {
            Status::unavailable("the project registry could not be reached in time — retry")
        }
        _ => Status::internal("the project registry is unavailable"),
    }
}

#[tonic::async_trait]
impl ProjectService for Project {
    /// THE REGISTRY LOAD. A caller answers "what does this path resolve to" in
    /// memory rather than over the network on every request, and this is where
    /// it gets the set to answer from.
    async fn list_projects(
        &self,
        request: Request<pb::ProjectServiceListProjectsRequest>,
    ) -> Result<Response<pb::ProjectServiceListProjectsResponse>, Status> {
        let req = request.into_inner();
        let scope = unattested_placeholder_scope();
        // Started BEFORE the work, so the duration covers the handler.
        let call = Call::start(
            "project",
            "ListProjects",
            Kind::Read,
            tel_scope(scope.as_ref()),
        );

        call.run(
            async move {
                let listed = self
                    .db
                    .clone()
                    .list_projects(pb::ListProjectsRequest {
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
                    })
                    .await
                    .map_err(|e| passthrough(e, "list"))?
                    .into_inner();

                let paths = listed
                    .projects
                    .into_iter()
                    // The bare reserved segment, dropped. See
                    // [`is_reserved_root`].
                    .filter(|project| !is_reserved_root(&project.path))
                    // THREE FIELDS OF SIX. `meta`, `display_name` and
                    // `last_seen_at` are not on `RegisteredPath` and are not
                    // put on the wire: this rpc answers what resolves, not what
                    // a project is.
                    .map(|project| pb::RegisteredPath {
                        path: project.path,
                        status: project.status,
                        // A rename is an ALIAS and never a rewrite (D53), so a
                        // walk consulting live paths only would strand every
                        // descendant of a renamed parent. The store binds its
                        // ancestor chain against live paths AND aliases; a walk
                        // over this set is the same walk and needs both inputs.
                        aliases: project.aliases,
                    })
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
                payload: format!("{r:?}"),
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

    /// THE FALLBACK, not the hot path — asked when the caller has no loaded set
    /// to walk. A pass-through on the response, and `exact` is the field that
    /// must survive it: `false` is neither an error nor a detail but the whole
    /// safety property that makes resolving upward acceptable instead of
    /// refusing.
    async fn resolve_project(
        &self,
        request: Request<pb::ProjectServiceResolveProjectRequest>,
    ) -> Result<Response<pb::ProjectServiceResolveProjectResponse>, Status> {
        let req = request.into_inner();
        let scope = unattested_placeholder_scope();
        let call = Call::start(
            "project",
            "ResolveProject",
            Kind::Read,
            tel_scope(scope.as_ref()),
        );

        call.run(
            async move {
                let resolved = self
                    .db
                    .clone()
                    .resolve_project(pb::ResolveProjectRequest {
                        scope,
                        candidate_path: req.candidate_path,
                    })
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
}

#[cfg(test)]
mod tests {
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
    #[test]
    fn the_placeholder_scope_is_present_and_attests_nothing() {
        let scope = unattested_placeholder_scope().expect("the store refuses an absent scope");
        assert_eq!(scope.request_id, "");
        assert_eq!(scope.instance_id, "");
        assert_eq!(scope.user_id, "");
        assert_eq!(scope.project_id, "");
    }
}
