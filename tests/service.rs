//! What a caller of the public API actually sees.
//!
//! THESE RUN A REAL SERVER. The mock is a `ProjectDbService` served over
//! loopback, not a hand-rolled stand-in for the client, because the properties
//! under test are wire properties: that a `-db` status code survives the hop
//! with its code intact and its MESSAGE dropped, and that what the store put in
//! a row is what the caller gets back. A fake that hands a `Status` over by
//! value touches neither the codec nor the trailers, so it could not tell
//! either of those apart from a handler that invents the answer.
//!
//! FIXTURE DISCIPLINE. Every value the store returns here is one the handler
//! could not plausibly have produced from its own inputs — a resolved path the
//! request does not name, aliases nothing asked for, a page token no caller
//! sent. A fixture the implementation could have chosen proves nothing: the
//! assertion passes for the correct handler and for the one that echoes its own
//! request back.
//!
//! WHAT THE ASSERTIONS OVER `Recorded` ARE FOR. Three of this tier's four
//! obligations are about the request it SENDS rather than the response it
//! returns — `under_path` empty so the set is the whole registry, `status`
//! unset so archived rows survive, and a `Scope` present because the store
//! refuses an absent one. None of them is visible in a response, so the
//! recorded request is the only place they can be pinned.

use std::sync::{Arc, Mutex};

use tonic::{Code, Request, Response, Status};
use yadgar_project::pb::yadgar::common::v1 as common;
use yadgar_project::pb::yadgar::project::v1 as pb;
use yadgar_project::pb::yadgar::project::v1::project_db_service_server::{
    ProjectDbService, ProjectDbServiceServer,
};
use yadgar_project::pb::yadgar::project::v1::project_service_server::ProjectService;
use yadgar_project::service::{Project, LIST_CALLER};

// ---------------------------------------------------------------------------
// Fixtures the implementation could not have chosen for itself.
// ---------------------------------------------------------------------------

/// What the CALLER asks about. Never what the store answers.
const CALLER_CANDIDATE: &str = "quinyx/qwfm/forecast/the-path-the-caller-sent";

/// What the STORE resolves it to. Deliberately not an ancestor of the candidate
/// above, so a handler echoing its own request back fails rather than
/// coincidentally passing.
const STORE_RESOLVED: &str = "acme/assigned-by-the-store";

/// The repo the store names as governing the resolved row's namespace.
/// Deliberately not equal to [`STORE_RESOLVED`], so a handler that
/// "helpfully" substitutes the resolved path for an absent source repo is
/// caught rather than passing by coincidence.
const STORE_SOURCE_REPO: &str = "yadgarhq/estate";

/// A page token the caller never sends, so a handler synthesising one — or
/// dropping the store's — is caught either way.
const STORE_TOKEN: &str = "acme/the-token-the-store-issued";

/// The token a CALLER sends. Never what the store returns.
const CALLER_TOKEN: &str = "quinyx/the-token-the-caller-sent";

/// What `-db` says when it fails: storage vocabulary, exactly the kind a public
/// client has no business seeing.
const DB_DETAIL: &str = "column path of table project_0007 rejected 'mauve porcupine 4711'";

/// A stored row carrying every field the tier must NOT forward, so a
/// pass-through of the whole message is caught rather than passing.
fn a_stored_project(path: &str, status: pb::ProjectStatus) -> pb::Project {
    pb::Project {
        meta: Some(common::Meta {
            id: "yadgar:project:0192f3c1-assigned-by-the-store".into(),
            version: 41,
            project_id: "quinyx/qwfm/assigned-by-the-store".into(),
            ..Default::default()
        }),
        path: path.into(),
        display_name: "The Display Name The Store Holds".into(),
        status: status as i32,
        last_seen_at: Some(prost_types::Timestamp {
            seconds: 1_757_000_000,
            nanos: 0,
        }),
        aliases: vec!["acme/the-former-path".into()],
    }
}

// ---------------------------------------------------------------------------
// The mock store.
// ---------------------------------------------------------------------------

/// A canned outcome. The error side is `(code, message)` rather than a `Status`
/// because the message has to be BUILT ON THE SERVER and cross the wire — that
/// journey is half of what the passthrough tests assert.
type Canned<T> = Result<T, (Code, &'static str)>;

/// Every request that reached the store.
#[derive(Default)]
struct Recorded {
    list: Vec<pb::ListProjectsRequest>,
    resolve: Vec<pb::ResolveProjectRequest>,
    /// The `x-yadgar-request-id` each `ListProjects` arrived with, read through
    /// `to_str()` as `project-db` reads it. `None` is no header at all.
    list_request_ids: Vec<Option<String>>,
    /// The same, for each `ResolveProject`.
    resolve_request_ids: Vec<Option<String>>,
}

/// The header a store call arrived with, read the way `project-db` reads it:
/// `to_str()`, so a value that is not visible ASCII arrives as `None` here
/// exactly as it would be unreadable there.
fn request_id_header<T>(request: &Request<T>) -> Option<String> {
    request
        .metadata()
        .get("x-yadgar-request-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

/// One scripted outcome per RPC. No queue: no handler in this service calls any
/// `-db` RPC more than once, and a queue would quietly permit a second call.
#[derive(Default)]
struct MockDb {
    list: Option<Canned<pb::ListProjectsResponse>>,
    resolve: Option<Canned<pb::ResolveProjectResponse>>,
    seen: Arc<Mutex<Recorded>>,
}

fn answer<T: Clone>(canned: &Option<Canned<T>>, rpc: &str) -> Result<Response<T>, Status> {
    match canned {
        Some(Ok(value)) => Ok(Response::new(value.clone())),
        Some(Err((code, message))) => Err(Status::new(*code, *message)),
        // Never scripted on purpose. Surfacing it as a status rather than a
        // panic keeps the failure inside the assertion the test already makes.
        None => Err(Status::unimplemented(format!(
            "the mock store has no scripted outcome for {rpc}"
        ))),
    }
}

/// The verbs these cases never reach answer `UNIMPLEMENTED` here, which is
/// also what `project-db` answers for two of them. A handler that grew one of
/// them would fail rather than reach a stub that pretends.
#[tonic::async_trait]
impl ProjectDbService for MockDb {
    async fn register_project(
        &self,
        _request: Request<pb::RegisterProjectRequest>,
    ) -> Result<Response<pb::RegisterProjectResponse>, Status> {
        Err(Status::unimplemented(
            "the tier must never call RegisterProject",
        ))
    }

    async fn resolve_project(
        &self,
        request: Request<pb::ResolveProjectRequest>,
    ) -> Result<Response<pb::ResolveProjectResponse>, Status> {
        let id = request_id_header(&request);
        let mut seen = self.seen.lock().unwrap();
        seen.resolve_request_ids.push(id);
        seen.resolve.push(request.into_inner());
        drop(seen);
        answer(&self.resolve, "ResolveProject")
    }

    async fn get_project(
        &self,
        _request: Request<pb::GetProjectRequest>,
    ) -> Result<Response<pb::GetProjectResponse>, Status> {
        Err(Status::unimplemented("the tier must never call GetProject"))
    }

    async fn list_projects(
        &self,
        request: Request<pb::ListProjectsRequest>,
    ) -> Result<Response<pb::ListProjectsResponse>, Status> {
        let id = request_id_header(&request);
        let mut seen = self.seen.lock().unwrap();
        seen.list_request_ids.push(id);
        seen.list.push(request.into_inner());
        drop(seen);
        answer(&self.list, "ListProjects")
    }

    async fn touch_projects(
        &self,
        _request: Request<pb::TouchProjectsRequest>,
    ) -> Result<Response<pb::TouchProjectsResponse>, Status> {
        Err(Status::unimplemented(
            "the tier must never call TouchProjects",
        ))
    }

    async fn rename_project(
        &self,
        _request: Request<pb::RenameProjectRequest>,
    ) -> Result<Response<pb::RenameProjectResponse>, Status> {
        Err(Status::unimplemented(
            "the tier must never call RenameProject",
        ))
    }

    async fn archive_project(
        &self,
        _request: Request<pb::ArchiveProjectRequest>,
    ) -> Result<Response<pb::ArchiveProjectResponse>, Status> {
        Err(Status::unimplemented(
            "the tier must never call ArchiveProject",
        ))
    }
    // `tests/sync.rs` owns the sync relay and serves its own store; no handler
    // these cases call reaches this verb.
    async fn sync_projects(
        &self,
        _request: Request<pb::SyncProjectsRequest>,
    ) -> Result<Response<pb::SyncProjectsResponse>, Status> {
        Err(Status::unimplemented(
            "no read handler may call SyncProjects",
        ))
    }
}

/// Serve the mock on loopback and hand back a `Project` wired to it.
///
/// The server task is owned by the test's runtime and dies with it, so there is
/// nothing to reap: the runtime is dropped when the `#[tokio::test]` body
/// returns.
async fn wire(mock: MockDb) -> (Project, Arc<Mutex<Recorded>>) {
    let seen = mock.seen.clone();

    // Bind FIRST, then serve the bound listener. Picking a port and reopening it
    // leaves a window in which something else takes it.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a loopback port");
    let addr = listener.local_addr().expect("the bound address");

    tokio::spawn(async move {
        let _ = tonic::transport::Server::builder()
            .add_service(ProjectDbServiceServer::new(mock))
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await;
    });

    let channel = tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
        .expect("a valid endpoint")
        .connect()
        .await
        .expect("the mock store accepted the connection");

    (Project::new(channel), seen)
}

async fn list_returning(
    response: pb::ListProjectsResponse,
) -> (pb::ProjectServiceListProjectsResponse, Arc<Mutex<Recorded>>) {
    let (svc, seen) = wire(MockDb {
        list: Some(Ok(response)),
        ..Default::default()
    })
    .await;

    let got = svc
        .list_as(
            Some(LIST_CALLER),
            Request::new(pb::ProjectServiceListProjectsRequest {
                page_size: 17,
                page_token: CALLER_TOKEN.into(),
            }),
        )
        .await
        .expect("the store answered, so the call must")
        .into_inner();
    (got, seen)
}

// ---------------------------------------------------------------------------
// ListProjects — the three obligations project.proto states on the response.
// ---------------------------------------------------------------------------

/// The mapping. `Project` carries six fields and `RegisteredPath` carries three:
/// a tier that forwarded the store's message wholesale would put `meta`,
/// `display_name` and `last_seen_at` on the wire to every caller.
#[tokio::test]
async fn a_stored_row_becomes_a_registered_path_carrying_three_fields() {
    let (got, _) = list_returning(pb::ListProjectsResponse {
        projects: vec![a_stored_project("acme/widgets", pb::ProjectStatus::Active)],
        next_page_token: String::new(),
    })
    .await;

    assert_eq!(got.paths.len(), 1);
    let path = &got.paths[0];
    assert_eq!(path.path, "acme/widgets");
    assert_eq!(path.status, pb::ProjectStatus::Active as i32);
    assert_eq!(path.aliases, vec!["acme/the-former-path".to_string()]);
}

/// `ProjectServiceListProjectsResponse`: "THE BARE RESERVED SEGMENT IS NEVER IN
/// THIS SET, and emitting it is this tier's half of a guard the store can only
/// half-close." The store refuses to MINT such a row and drops the segment from
/// its ancestor chain; it does not delete a row an earlier build accepted, so
/// the row can still arrive here.
#[tokio::test]
async fn the_bare_reserved_segment_is_dropped_from_the_set() {
    let (got, _) = list_returning(pb::ListProjectsResponse {
        projects: vec![
            a_stored_project("local", pb::ProjectStatus::Active),
            a_stored_project("acme/widgets", pb::ProjectStatus::Active),
        ],
        next_page_token: String::new(),
    })
    .await;

    let paths: Vec<&str> = got.paths.iter().map(|p| p.path.as_str()).collect();
    assert_eq!(paths, vec!["acme/widgets"]);
}

/// The comparison folds ASCII case, because `project-db`'s does: `project.path`
/// carries `COLLATE utf8mb4_general_ci`, declared by `project-db`'s
/// `create_project` migration (migration 1) and reasserted by
/// `pin_the_path_collation` (migration 4), which is still the formal
/// declaration for `project_alias.alias_path`. `path::refuse_reserved_root`
/// uses `eq_ignore_ascii_case`. A byte-exact filter here would let `LOCAL`
/// through into a set the store's own walk excludes.
#[tokio::test]
async fn the_reserved_segment_filter_folds_ascii_case() {
    let (got, _) = list_returning(pb::ListProjectsResponse {
        projects: vec![
            a_stored_project("LOCAL", pb::ProjectStatus::Active),
            a_stored_project("LoCaL", pb::ProjectStatus::Active),
            a_stored_project("locals", pb::ProjectStatus::Active),
            a_stored_project("local/home", pb::ProjectStatus::Active),
        ],
        next_page_token: String::new(),
    })
    .await;

    // `locals` shares five characters and is a different organisation;
    // `local/home` IS the private class the segment is reserved to carry, and
    // refusing it would delete that class. Only the bare segment goes.
    let paths: Vec<&str> = got.paths.iter().map(|p| p.path.as_str()).collect();
    assert_eq!(paths, vec!["locals", "local/home"]);
}

/// THE SAME GUARD, ON THE ALIASES (ledger 764). `project-db` closes both arms
/// with one line — `read.rs` drops `local` from the ancestor chain it binds
/// against live paths AND aliases — so an alias at the bare segment is as much
/// the hole `ProjectServiceListProjectsResponse` names as a live path there. A
/// caller walking this set performs a second walk, and an alias at `local`
/// would be the nearest ancestor of every private path.
///
/// No write path mints aliases today (`RenameProject` is held back), but a
/// code-only guard does not delete a row an earlier build or a hand-written
/// statement accepted, so the row can still arrive here.
///
/// **THE MUTATION THAT REDS IT:** copy `project.aliases` through unfiltered.
#[tokio::test]
async fn an_alias_at_the_bare_reserved_segment_is_dropped_from_the_set() {
    let mut row = a_stored_project("acme/kept-by-the-store", pb::ProjectStatus::Active);
    row.aliases = vec![
        "local".into(),
        "LOCAL".into(),
        "local/home".into(),
        "locals".into(),
    ];
    let (got, _) = list_returning(pb::ListProjectsResponse {
        projects: vec![row],
        next_page_token: String::new(),
    })
    .await;

    assert_eq!(got.paths.len(), 1, "the row stays; only its alias goes");
    // `local/home` is the private class the segment is reserved to carry and
    // `locals` is a different organisation. Only the bare segment, in any ASCII
    // case, is dropped — the same precision the path filter has.
    assert_eq!(
        got.paths[0].aliases,
        vec!["local/home".to_string(), "locals".to_string()]
    );
}

/// `RegisteredPath`: "THE SET IS NOT FILTERED BY STATUS, AND MUST NOT BE." A
/// caller that dropped archived rows from its own walk would resolve a candidate
/// to a different ancestor than `ResolveProject` resolves it to.
#[tokio::test]
async fn an_archived_row_survives_into_the_set() {
    let (got, seen) = list_returning(pb::ListProjectsResponse {
        projects: vec![a_stored_project(
            "acme/retired",
            pb::ProjectStatus::Archived,
        )],
        next_page_token: String::new(),
    })
    .await;

    assert_eq!(got.paths.len(), 1);
    assert_eq!(got.paths[0].status, pb::ProjectStatus::Archived as i32);
    // And the tier must not ask the store to filter either — an unset `status`
    // is what makes the set the whole registry.
    assert_eq!(seen.lock().unwrap().list[0].status, None);
}

/// "THE CALLER LOOPS UNTIL THIS COMES BACK EMPTY." A truncated set is not a
/// smaller registry; it is a wrong one. So the token is forwarded verbatim and
/// the tier never loops on the caller's behalf.
#[tokio::test]
async fn the_page_token_is_forwarded_verbatim_in_both_directions() {
    let (got, seen) = list_returning(pb::ListProjectsResponse {
        projects: vec![a_stored_project("acme/widgets", pb::ProjectStatus::Active)],
        next_page_token: STORE_TOKEN.into(),
    })
    .await;

    assert_eq!(got.next_page_token, STORE_TOKEN);
    let seen = seen.lock().unwrap();
    assert_eq!(seen.list.len(), 1, "one page, one store call");
    assert_eq!(seen.list[0].page_token, CALLER_TOKEN);
    assert_eq!(seen.list[0].page_size, 17);
}

/// Filtering the reserved row shrinks a page by one and MUST NOT disturb the
/// token: the store's token is a keyset cursor over its own last row, so a tier
/// that recomputed it from the rows it kept would skip every row between them.
#[tokio::test]
async fn filtering_the_reserved_row_does_not_disturb_the_token() {
    let (got, _) = list_returning(pb::ListProjectsResponse {
        projects: vec![
            a_stored_project("acme/widgets", pb::ProjectStatus::Active),
            a_stored_project("local", pb::ProjectStatus::Active),
        ],
        next_page_token: STORE_TOKEN.into(),
    })
    .await;

    assert_eq!(got.paths.len(), 1);
    assert_eq!(got.next_page_token, STORE_TOKEN);
}

/// The whole registry, never a slice: the sibling plan's §2 rules that the
/// loaded set is the whole of it, and `under_path` is the only thing that could
/// narrow it.
#[tokio::test]
async fn the_store_is_asked_for_the_whole_registry() {
    let (_, seen) = list_returning(pb::ListProjectsResponse::default()).await;
    assert_eq!(seen.lock().unwrap().list[0].under_path, "");
}

// ---------------------------------------------------------------------------
// ListProjects — the owner of a private row (ledger 1638, car P2).
// ---------------------------------------------------------------------------

/// The registrant `project-db` stores on a private row. Shaped like no other
/// value in a fixture row, so a relay that copied `created_by`, `id` or
/// `project_id` instead is caught rather than passing by coincidence.
const STORE_OWNER: &str = "yadgar:user:0192f3c1-the-registrant-the-store-holds";

/// A stored row whose `Meta` carries `owner`, and audit identities that are
/// NOT the owner (D12 calls them audit identity, distinct from ownership).
fn a_stored_row_owned_by(path: &str, owner: &str) -> pb::Project {
    let mut row = a_stored_project(path, pb::ProjectStatus::Active);
    let meta = row.meta.as_mut().expect("the fixture carries a Meta");
    meta.owner_user_id = owner.into();
    meta.created_by = "yadgar:user:the-auditor-not-the-owner".into();
    meta.updated_by = "yadgar:user:the-auditor-not-the-owner".into();
    row
}

/// `RegisteredPath.owner_user_id` (proto v1.20.0): the store's
/// `Meta.owner_user_id`, relayed. On one page, the private row carries its
/// owner and the org row carries `""` — the gateway reads the class from the
/// path and fails closed on a private row with no owner, so both halves matter.
///
/// **THE MUTATIONS THAT RED IT:** leave the field empty (the private half), or
/// relay `created_by` instead (both halves).
#[tokio::test]
async fn a_private_row_carries_its_owner_and_an_org_row_carries_empty() {
    let (got, _) = list_returning(pb::ListProjectsResponse {
        projects: vec![
            a_stored_row_owned_by("local/the-owner/notes", STORE_OWNER),
            a_stored_row_owned_by("acme/widgets", ""),
        ],
        next_page_token: String::new(),
    })
    .await;

    let owners: Vec<(&str, &str)> = got
        .paths
        .iter()
        .map(|p| (p.path.as_str(), p.owner_user_id.as_str()))
        .collect();
    assert_eq!(
        owners,
        vec![("local/the-owner/notes", STORE_OWNER), ("acme/widgets", "")]
    );
}

/// The owner survives on a LATER page: the request carries a token, the store
/// answers the page after it, and the private row on that page keeps its owner.
/// The tier is stateless per page, so this pins that nothing in the relay
/// depends on the page being the first.
#[tokio::test]
async fn a_private_row_on_a_later_page_keeps_its_owner() {
    let (got, seen) = list_returning(pb::ListProjectsResponse {
        projects: vec![a_stored_row_owned_by("local/the-owner/later", STORE_OWNER)],
        next_page_token: STORE_TOKEN.into(),
    })
    .await;

    assert_eq!(seen.lock().unwrap().list[0].page_token, CALLER_TOKEN);
    assert_eq!(got.paths.len(), 1);
    assert_eq!(got.paths[0].owner_user_id, STORE_OWNER);
    assert_eq!(got.next_page_token, STORE_TOKEN);
}

/// A row with no `Meta` at all reports `""`, never a panic and never a
/// substitute. The contract makes `""` on a private row mean "owned by
/// nobody", and the gateway fails closed on it.
#[tokio::test]
async fn a_row_with_no_meta_carries_an_empty_owner() {
    let mut row = a_stored_project("local/the-owner/bare", pb::ProjectStatus::Active);
    row.meta = None;
    let (got, _) = list_returning(pb::ListProjectsResponse {
        projects: vec![row],
        next_page_token: String::new(),
    })
    .await;

    assert_eq!(got.paths[0].owner_user_id, "");
}

// ---------------------------------------------------------------------------
// ListProjects — the caller check (ledger 1643).
// ---------------------------------------------------------------------------

/// Every caller but the gateway's leaf is refused `PERMISSION_DENIED`, and the
/// store is never asked. The near misses are there because the comparison is
/// EXACT. `None` is no TLS, no client certificate, or an ambiguous subject.
///
/// **THE MUTATION THAT REDS IT:** drop the check, or run it after the relay.
#[tokio::test]
async fn a_caller_other_than_the_gateway_is_refused_before_the_store() {
    let (svc, seen) = a_store_answering_both_reads().await;

    for caller in [
        Some("seeder-caller"),
        Some("project-caller"),
        Some("task-caller"),
        Some("Gateway-caller"),
        Some("gateway-caller "),
        Some("gateway"),
        None,
    ] {
        let err = svc
            .list_as(
                caller,
                Request::new(pb::ProjectServiceListProjectsRequest::default()),
            )
            .await
            .expect_err("only the gateway may load the registry");
        assert_eq!(err.code(), Code::PermissionDenied, "{caller:?}: {err:?}");
    }
    assert!(
        seen.lock().unwrap().list.is_empty(),
        "a refused caller reached the store"
    );
}

/// The trait method reads the caller off the peer chain, and a direct call has
/// none, so it is refused. The wire half — a real leaf through the real
/// listener — is in `tests/sync.rs`.
#[tokio::test]
async fn a_direct_list_call_has_no_peer_chain_and_is_refused() {
    let (svc, seen) = a_store_answering_both_reads().await;

    let err = svc
        .list_projects(Request::new(
            pb::ProjectServiceListProjectsRequest::default(),
        ))
        .await
        .expect_err("no peer chain");
    assert_eq!(err.code(), Code::PermissionDenied);
    assert!(seen.lock().unwrap().list.is_empty());
}

/// A refusal is still a call, and D67 records it: the check runs inside the
/// observed call, so the record carries the refusal rather than nothing.
#[tokio::test]
async fn a_refused_list_is_recorded_as_permission_denied() {
    let (svc, _) = a_store_answering_both_reads().await;

    let record = the_call_record(svc.list_as(
        Some("seeder-caller"),
        with_request_id(
            pb::ProjectServiceListProjectsRequest::default(),
            GATEWAY_REQUEST_ID,
        ),
    ))
    .await;

    assert!(record.contains("PERMISSION_DENIED"), "{record}");
    assert!(carries(&record, GATEWAY_REQUEST_ID), "{record}");
}

/// Pinned by LITERAL (ADR-0599): `gateway-caller` is the common name
/// `platform` writes on the gateway's client leaf (`chart/values.yaml`,
/// `gateway-client-tls`). The two repositories share no crate.
#[test]
fn the_admitted_list_caller_is_the_gateway_leaf_platform_issues() {
    assert_eq!(LIST_CALLER, "gateway-caller");
}

// ---------------------------------------------------------------------------
// ResolveProject — a pass-through on the response, a rewrite of the request.
// ---------------------------------------------------------------------------

async fn resolve_returning(
    response: pb::ResolveProjectResponse,
) -> (
    pb::ProjectServiceResolveProjectResponse,
    Arc<Mutex<Recorded>>,
) {
    let (svc, seen) = wire(MockDb {
        resolve: Some(Ok(response)),
        ..Default::default()
    })
    .await;

    let got = svc
        .resolve_project(Request::new(pb::ProjectServiceResolveProjectRequest {
            candidate_path: CALLER_CANDIDATE.into(),
        }))
        .await
        .expect("the store answered, so the call must")
        .into_inner();
    (got, seen)
}

/// Field for field. The two response messages are separate types only because
/// buf's `RPC_REQUEST_RESPONSE_UNIQUE` refuses to let two rpcs share one, so
/// anything the tier changed here would be the tier inventing an answer.
#[tokio::test]
async fn every_field_of_a_resolution_reaches_the_caller() {
    let (got, seen) = resolve_returning(pb::ResolveProjectResponse {
        resolved_path: STORE_RESOLVED.into(),
        exact: false,
        via_alias: true,
        status: pb::ProjectStatus::Archived as i32,
        source_repo: STORE_SOURCE_REPO.into(),
    })
    .await;

    assert_eq!(got.resolved_path, STORE_RESOLVED);
    assert!(!got.exact);
    assert!(got.via_alias);
    assert_eq!(got.status, pb::ProjectStatus::Archived as i32);
    assert_eq!(got.source_repo, STORE_SOURCE_REPO);
    assert_eq!(
        seen.lock().unwrap().resolve[0].candidate_path,
        CALLER_CANDIDATE
    );
}

/// `source_repo` is a bare proto3 `string`, so an unpopulated value arrives as
/// `""`, not absent, and `""` is a legitimate answer — the resolved row is
/// PRIVATE-class or predates the column. The relay must carry that `""`
/// through unchanged: not substitute `resolved_path` (the "helpful" mutation
/// [`STORE_SOURCE_REPO`]'s doc comment warns about), not refuse, not map it
/// to anything else. The gateway is the consumer and decides what an empty
/// value means.
#[tokio::test]
async fn an_absent_source_repo_crosses_the_relay_as_empty() {
    let (got, _) = resolve_returning(pb::ResolveProjectResponse {
        resolved_path: STORE_RESOLVED.into(),
        exact: true,
        via_alias: false,
        status: pb::ProjectStatus::Active as i32,
        source_repo: String::new(),
    })
    .await;

    assert_eq!(got.source_repo, "");
}

/// `exact: false` is "NEITHER AN ERROR NOR A DETAIL: it is the whole safety
/// property that makes resolving upward acceptable instead of refusing". A tier
/// that turned it into a status, or that swallowed it, would delete the
/// condition D52 made the upward resolution conditional on.
#[tokio::test]
async fn an_inexact_resolution_is_not_an_error() {
    let (got, _) = resolve_returning(pb::ResolveProjectResponse {
        resolved_path: STORE_RESOLVED.into(),
        exact: false,
        via_alias: false,
        status: pb::ProjectStatus::Active as i32,
        source_repo: String::new(),
    })
    .await;

    assert_eq!(got.resolved_path, STORE_RESOLVED);
    assert!(!got.exact, "the soft failure must survive the hop");
}

// ---------------------------------------------------------------------------
// The Scope the store demands and this tier does not hold.
// ---------------------------------------------------------------------------

/// `project-db/src/sql.rs`'s `scope_of` refuses an ABSENT `Scope` and reads no
/// field of a present one, so both store calls must carry one or every request
/// this tier serves fails with `INVALID_ARGUMENT`.
///
/// **THIS TEST PINS A PLACEHOLDER, NOT A CONTROL.** The value attests nothing.
/// It is asserted so that whichever of the plan's routes is ruled — the tier
/// mints one, the contract grows one, or the store stops demanding one — the
/// change trips a test rather than passing silently.
///
/// **AND IT CANNOT TRIP ON ROUTE (c) BY ITSELF (ledger 762).** The store here
/// is a mock, so its premise — "the store refuses an absent one" — is pinned in
/// `project-db` instead, by `an_absent_scope_is_refused` (`tests/resolve.rs`)
/// and `list_refuses_an_absent_scope` (`tests/list.rs`). The PR that flips those
/// two is route (c), and the same train MUST delete this test and
/// `service.rs::unattested_placeholder_scope`.
#[tokio::test]
async fn both_store_calls_carry_a_scope_because_the_store_refuses_an_absent_one() {
    let (svc, seen) = wire(MockDb {
        list: Some(Ok(pb::ListProjectsResponse::default())),
        resolve: Some(Ok(pb::ResolveProjectResponse::default())),
        ..Default::default()
    })
    .await;

    svc.list_as(
        Some(LIST_CALLER),
        Request::new(pb::ProjectServiceListProjectsRequest::default()),
    )
    .await
    .expect("the store answered");
    svc.resolve_project(Request::new(pb::ProjectServiceResolveProjectRequest {
        candidate_path: CALLER_CANDIDATE.into(),
    }))
    .await
    .expect("the store answered");

    let seen = seen.lock().unwrap();
    assert!(
        seen.list[0].scope.is_some(),
        "an absent scope is refused by project-db before it reads a row — pinned \
         there by list_refuses_an_absent_scope (tests/list.rs, ledger 762)"
    );
    assert!(
        seen.resolve[0].scope.is_some(),
        "an absent scope is refused by project-db before it reads a row — pinned \
         there by an_absent_scope_is_refused (tests/resolve.rs, ledger 762)"
    );
}

// ---------------------------------------------------------------------------
// passthrough: the codes, and the message that must not escape.
// ---------------------------------------------------------------------------

async fn resolve_failing_with(code: Code, detail: &'static str) -> Status {
    let (svc, _) = wire(MockDb {
        resolve: Some(Err((code, detail))),
        ..Default::default()
    })
    .await;

    svc.resolve_project(Request::new(pb::ProjectServiceResolveProjectRequest {
        candidate_path: CALLER_CANDIDATE.into(),
    }))
    .await
    .expect_err("the store failed, so the call must")
}

/// `ProjectService.ResolveProject`: "NO REGISTERED ANCESTOR IS `NOT_FOUND`,
/// which is the code the store already answers, so one condition carries one
/// code across both tiers." Collapsing it into `INTERNAL` would break that.
#[tokio::test]
async fn a_store_not_found_reaches_the_caller_as_not_found() {
    let status = resolve_failing_with(Code::NotFound, DB_DETAIL).await;
    assert_eq!(status.code(), Code::NotFound);
    assert_eq!(
        status.message(),
        "no registered project is that path or an ancestor of it"
    );
}

/// The store was unreachable, which is the one failure this service already
/// promises to report as itself: the boot dial is lazy (ADR-0532), and the whole
/// justification for that is that a request arriving before `project-db` is
/// reachable fails RECOVERABLY.
#[tokio::test]
async fn an_unreachable_store_reaches_the_caller_as_unavailable() {
    for code in [Code::Unavailable, Code::DeadlineExceeded] {
        let status = resolve_failing_with(code, DB_DETAIL).await;
        assert_eq!(status.code(), Code::Unavailable);
        assert_eq!(
            status.message(),
            "the project registry could not be reached in time — retry"
        );
    }
}

/// The store's message may name tables and columns, and a client of the public
/// API has no business seeing the storage layer's vocabulary. Asserted over
/// every code the store can answer with, so an arm added later cannot leak.
#[tokio::test]
async fn the_stores_own_words_never_reach_the_caller() {
    for code in [
        Code::NotFound,
        Code::InvalidArgument,
        Code::Unavailable,
        Code::DeadlineExceeded,
        Code::Internal,
        Code::PermissionDenied,
    ] {
        let status = resolve_failing_with(code, DB_DETAIL).await;
        assert!(
            !status.message().contains("project_0007"),
            "{code:?} leaked the store's message: {}",
            status.message()
        );
        assert!(
            !status.message().contains("mauve porcupine"),
            "{code:?} leaked the store's message: {}",
            status.message()
        );
    }
}

/// A `ListProjects` failure takes the same route. Written separately because
/// `passthrough` is reached from two handlers and a change to either call site
/// could drop it.
#[tokio::test]
async fn a_list_failure_passes_through_with_its_code_intact() {
    let (svc, _) = wire(MockDb {
        list: Some(Err((Code::InvalidArgument, DB_DETAIL))),
        ..Default::default()
    })
    .await;

    let status = svc
        .list_as(
            Some(LIST_CALLER),
            Request::new(pb::ProjectServiceListProjectsRequest::default()),
        )
        .await
        .expect_err("the store failed, so the call must");
    assert_eq!(status.code(), Code::InvalidArgument);
    assert!(!status.message().contains("project_0007"));
}

// ---------------------------------------------------------------------------
// D67's request id: read from the header, because no read request carries a
// Scope (ledger 1254).
// ---------------------------------------------------------------------------

/// The id the GATEWAY mints and stamps on every upstream call
/// (`gateway/src/upstream/request_id.rs`, ledger 1248). A UUIDv7 nothing in
/// this crate produces, so a record carrying it got it from the header.
const GATEWAY_REQUEST_ID: &str = "01a0fdd8-2b87-704e-b0b6-77e1c0ffee42";

/// Captures every `CallRecord` line `yadgar_telemetry::record::emit` writes,
/// for as long as the guard `record::set_sink` hands back lives.
#[derive(Clone, Default)]
struct RecordCapture(Arc<Mutex<Vec<String>>>);

impl yadgar_telemetry::record::Sink for RecordCapture {
    fn write_record(&self, line: &str) -> std::io::Result<()> {
        self.0.lock().expect("the capture").push(line.to_string());
        Ok(())
    }
}

/// `message` with `x-yadgar-request-id: id`, as the gateway sends it.
fn with_request_id<T>(message: T, id: &str) -> Request<T> {
    let mut req = Request::new(message);
    req.metadata_mut()
        .insert("x-yadgar-request-id", id.parse().expect("an ASCII id"));
    req
}

/// The one `CallRecord` line `run` emitted.
///
/// The sink is THREAD-scoped, and `#[tokio::test]` runs the handler on the
/// test's own thread, so the record lands here rather than on stdout.
async fn the_call_record<F: std::future::Future>(run: F) -> String {
    let records = RecordCapture::default();
    {
        let _sink = yadgar_telemetry::record::set_sink(Arc::new(records.clone()));
        let _ = run.await;
    }
    let lines = records.0.lock().expect("the capture").clone();
    assert_eq!(lines.len(), 1, "exactly one CallRecord: {lines:?}");
    lines[0].clone()
}

/// Whether a `CallRecord` line carries exactly `id` as its `request_id`.
fn carries(record: &str, id: &str) -> bool {
    record.contains(&format!(r#""request_id":"{id}""#))
}

async fn a_store_answering_both_reads() -> (Project, Arc<Mutex<Recorded>>) {
    wire(MockDb {
        list: Some(Ok(pb::ListProjectsResponse::default())),
        resolve: Some(Ok(pb::ResolveProjectResponse::default())),
        ..Default::default()
    })
    .await
}

/// **THE ID THE GATEWAY SENT IS THE ONE THIS HOP RECORDS, AND THE ONE THE STORE
/// RECEIVES.** `ListProjects` carries no `Scope`, so the header is the only
/// place the id can come from; a handler reading the `Scope` alone records the
/// placeholder's `""` and the call joins to nothing.
///
/// **THE MUTATION THAT REDS IT:** drop the header read, or the forward.
#[tokio::test]
async fn list_records_and_forwards_the_callers_request_id() {
    let (svc, seen) = a_store_answering_both_reads().await;

    let record = the_call_record(svc.list_as(
        Some(LIST_CALLER),
        with_request_id(
            pb::ProjectServiceListProjectsRequest::default(),
            GATEWAY_REQUEST_ID,
        ),
    ))
    .await;

    assert!(carries(&record, GATEWAY_REQUEST_ID), "{record}");
    assert_eq!(
        seen.lock().unwrap().list_request_ids,
        vec![Some(GATEWAY_REQUEST_ID.to_string())],
        "project-db joins on the id only if this hop forwards it"
    );
}

/// The same, for `ResolveProject` — the call the gateway makes on the request
/// path, so the one whose join an operator needs first.
#[tokio::test]
async fn resolve_records_and_forwards_the_callers_request_id() {
    let (svc, seen) = a_store_answering_both_reads().await;

    let record = the_call_record(svc.resolve_project(with_request_id(
        pb::ProjectServiceResolveProjectRequest {
            candidate_path: CALLER_CANDIDATE.into(),
        },
        GATEWAY_REQUEST_ID,
    )))
    .await;

    assert!(carries(&record, GATEWAY_REQUEST_ID), "{record}");
    assert_eq!(
        seen.lock().unwrap().resolve_request_ids,
        vec![Some(GATEWAY_REQUEST_ID.to_string())]
    );
}

/// **AN OVER-LONG ID IS CAPPED ONCE, HERE, AND THE CAPPED VALUE IS BOTH WHAT
/// THIS HOP RECORDS AND WHAT IT FORWARDS** — the bound and marker `iam` and
/// `iam-db` apply, so every hop cuts one over-long id identically and the
/// records still join. A header is bounded only by the transport; uncapped, a
/// caller that is not the gateway puts tens of kilobytes on every record.
#[tokio::test]
async fn an_over_long_request_id_is_capped_on_the_record_and_on_the_wire() {
    let (svc, seen) = a_store_answering_both_reads().await;
    let long = "z".repeat(1000);
    let cut = format!("{}...", "z".repeat(256));

    let record = the_call_record(svc.resolve_project(with_request_id(
        pb::ProjectServiceResolveProjectRequest {
            candidate_path: CALLER_CANDIDATE.into(),
        },
        &long,
    )))
    .await;

    assert!(carries(&record, &cut), "cut to 256 plus `...`: {record}");
    assert_eq!(seen.lock().unwrap().resolve_request_ids, vec![Some(cut)]);
}

/// No header, no id: the record carries `""` and the store receives NO header
/// rather than an empty one. Never a refusal — telemetry does not fail a call
/// (D25), and `ListProjects`'s boot-load caller may send no header at all.
#[tokio::test]
async fn an_absent_request_id_records_empty_and_forwards_nothing() {
    let (svc, seen) = a_store_answering_both_reads().await;

    let list = the_call_record(svc.list_as(
        Some(LIST_CALLER),
        Request::new(pb::ProjectServiceListProjectsRequest::default()),
    ))
    .await;
    let resolve = the_call_record(svc.resolve_project(Request::new(
        pb::ProjectServiceResolveProjectRequest {
            candidate_path: CALLER_CANDIDATE.into(),
        },
    )))
    .await;

    assert!(carries(&list, ""), "{list}");
    assert!(carries(&resolve, ""), "{resolve}");
    let seen = seen.lock().unwrap();
    assert_eq!(seen.list_request_ids, vec![None]);
    assert_eq!(seen.resolve_request_ids, vec![None]);
}

/// **THE PLACEHOLDER STAYS EMPTY WHEN AN ID ARRIVES.** The header reaches the
/// record and the wire as metadata; it is never copied into the `Scope` this
/// tier sends, because that `Scope` attests nothing (ADR-0511) and a value in it
/// would read as one the gateway attested (ledger 762).
#[tokio::test]
async fn the_request_id_never_enters_the_placeholder_scope() {
    let (svc, seen) = a_store_answering_both_reads().await;

    svc.list_as(
        Some(LIST_CALLER),
        with_request_id(
            pb::ProjectServiceListProjectsRequest::default(),
            GATEWAY_REQUEST_ID,
        ),
    )
    .await
    .expect("the store answered");
    svc.resolve_project(with_request_id(
        pb::ProjectServiceResolveProjectRequest {
            candidate_path: CALLER_CANDIDATE.into(),
        },
        GATEWAY_REQUEST_ID,
    ))
    .await
    .expect("the store answered");

    let seen = seen.lock().unwrap();
    assert_eq!(seen.list[0].scope.as_ref().unwrap().request_id, "");
    assert_eq!(seen.resolve[0].scope.as_ref().unwrap().request_id, "");
}
