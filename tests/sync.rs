//! `ProjectService.SyncProjects` — the seeder's one write verb, and the caller
//! check that makes this tier more than a relay (ADR-0936, ADR-0942,
//! `plans/the-d35-seeder.md` stage 3).
//!
//! **TWO LAYERS, as `project-db`'s stage-2 suite has them.** Most cases call
//! [`Project::sync_as`] with the caller identity passed in, so the
//! authorisation and the relay are tested without a handshake. The cases that
//! say "leaf" run REAL mTLS: `project` is served through
//! [`yadgar_project::serve::builder`] — the one function `main` builds its
//! server with — under `required`, and every client leaf is minted by ONE
//! authority, platform-shaped (a common name, a dNSName SAN equal to it,
//! `clientAuth`). Every leaf verifies; only the name differs. Only a handshake
//! proves the handler reads the identity off the certificate the listener
//! verified — a handler that read it after `into_inner()` dropped the
//! extensions would pass every `sync_as` case and fail these.
//!
//! **THE STORE IS A FAKE `ProjectDbService` served over loopback**, so a
//! refusal crosses the codec and the trailers and its code and message are
//! what the wire carried. It records every `SyncProjects` it receives: a
//! refusal case asserts ZERO, because the check must run before the relay.
//!
//! FIXTURE DISCIPLINE, as in `tests/service.rs`: every value the store
//! returns, and every value the relay must carry, is one the handler could not
//! have chosen for itself — five distinct counts, a minted `request_id`, the
//! mass-archive flag set.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, DnType, ExtendedKeyUsagePurpose, IsCa,
    KeyPair, KeyUsagePurpose,
};
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint, Identity};
use tonic::{Code, Request, Response, Status};

use yadgar_project::pb::yadgar::common::v1 as common;
use yadgar_project::pb::yadgar::project::v1 as pb;
use yadgar_project::pb::yadgar::project::v1::project_db_service_server::{
    ProjectDbService, ProjectDbServiceServer,
};
use yadgar_project::pb::yadgar::project::v1::project_service_client::ProjectServiceClient;
use yadgar_project::pb::yadgar::project::v1::project_service_server::{
    ProjectService, ProjectServiceServer,
};
use yadgar_project::serve::{self, ServerLimits, ServerTls, LISTEN};

/// The chart's ADR-1006 defaults, read through the same parser the binary uses.
fn limits() -> ServerLimits {
    ServerLimits::from_lookup(LISTEN, serve::LIMITS_CHART_KEY, |key| {
        let value = match key.strip_prefix("LISTEN_")? {
            "MAX_CONCURRENT_STREAMS" => "256",
            "KEEPALIVE_INTERVAL_SECS" => "30",
            "KEEPALIVE_TIMEOUT_SECS" => "10",
            "MAX_HEADER_LIST_BYTES" => "16384",
            _ => return None,
        };
        Some(value.to_string())
    })
    .expect("the chart defaults are inside the library's ranges")
}

use yadgar_project::service::{Project, SEEDER_PRINCIPAL, SYNC_CALLER};

// ---------------------------------------------------------------------------
// Fixtures.
// ---------------------------------------------------------------------------

/// The `request_id` the SEEDER mints, once per sync (ADR-0942). `project`
/// relays it unchanged so D67's records join across both hops; a handler that
/// minted its own, or dropped it, fails.
const SEEDER_REQUEST_ID: &str = "seeder-sync-0192f3c1-minted-by-the-seeder";

/// The five counts the store answers. All distinct, so a relay that swapped
/// two fields fails rather than passing by coincidence.
fn store_counts() -> pb::SyncProjectsResponse {
    pb::SyncProjectsResponse {
        inserted: 3,
        updated: 5,
        archived: 7,
        reactivated: 11,
        unchanged: 13,
        // Team access stage 5: the grant echo counts, distinct from the five
        // row counts and from each other.
        grants_declared: 17,
        grants_inserted: 19,
        grants_deleted: 23,
        grants_unchanged: 29,
    }
}

fn declared(path: &str, display_name: &str, source_repo: &str) -> pb::DeclaredProject {
    pb::DeclaredProject {
        path: path.into(),
        display_name: display_name.into(),
        source_repo: source_repo.into(),
        grants: Vec::new(),
    }
}

fn granted(mut d: pb::DeclaredProject, team: &str, level: pb::GrantLevel) -> pb::DeclaredProject {
    d.grants.push(pb::DeclaredGrant {
        team_id: team.into(),
        level: level as i32,
    });
    d
}

fn seeder_scope(user_id: &str) -> common::Scope {
    common::Scope {
        user_id: user_id.into(),
        request_id: SEEDER_REQUEST_ID.into(),
        ..Default::default()
    }
}

/// A seeder declaration carrying every field the relay must copy. The
/// mass-archive flag is SET, because its default is what a handler that
/// forgot it would send.
fn a_declaration(user_id: &str) -> pb::ProjectServiceSyncProjectsRequest {
    pb::ProjectServiceSyncProjectsRequest {
        scope: Some(seeder_scope(user_id)),
        // GRANTS ON A ROOT AND ON A PROJECT, and a project with none (team
        // access stage 5): a relay that dropped `DeclaredProject.grants`
        // fails the field-for-field assertion.
        roots: vec![granted(
            declared("tapir", "Tapir Org", "tapir/registry"),
            "yadgar:team:administrators",
            pb::GrantLevel::ReadWrite,
        )],
        projects: vec![
            granted(
                declared("tapir/alpha", "Alpha", "tapir/alpha"),
                "yadgar:team:readers",
                pb::GrantLevel::Read,
            ),
            declared("tapir/beta", "", "tapir/beta"),
        ],
        allow_mass_archive: true,
        // SET, for the reason the mass-archive flag is.
        allow_mass_revoke: true,
    }
}

// ---------------------------------------------------------------------------
// The fake store.
// ---------------------------------------------------------------------------

/// What the store holds: the sync requests it received, and the set a list
/// answers with. A successful sync REPLACES that set with the declaration, so
/// the replica case can see whether a sync through one `project` reaches a
/// list through another.
#[derive(Default)]
struct Held {
    syncs: Vec<pb::SyncProjectsRequest>,
    paths: Vec<String>,
}

#[derive(Clone)]
struct FakeStore {
    /// `Ok(counts)` applies the declaration; `Err((code, message))` refuses it.
    outcome: Result<pb::SyncProjectsResponse, (Code, &'static str)>,
    held: Arc<Mutex<Held>>,
}

impl FakeStore {
    fn answering(outcome: Result<pb::SyncProjectsResponse, (Code, &'static str)>) -> Self {
        Self {
            outcome,
            held: Arc::default(),
        }
    }

    fn syncs(&self) -> Vec<pb::SyncProjectsRequest> {
        self.held.lock().unwrap().syncs.clone()
    }
}

fn never(rpc: &str) -> Status {
    Status::unimplemented(format!("the sync cases never call {rpc}"))
}

#[tonic::async_trait]
impl ProjectDbService for FakeStore {
    async fn register_project(
        &self,
        _: Request<pb::RegisterProjectRequest>,
    ) -> Result<Response<pb::RegisterProjectResponse>, Status> {
        Err(never("RegisterProject"))
    }

    async fn resolve_project(
        &self,
        _: Request<pb::ResolveProjectRequest>,
    ) -> Result<Response<pb::ResolveProjectResponse>, Status> {
        Err(never("ResolveProject"))
    }

    async fn get_project(
        &self,
        _: Request<pb::GetProjectRequest>,
    ) -> Result<Response<pb::GetProjectResponse>, Status> {
        Err(never("GetProject"))
    }

    async fn list_projects(
        &self,
        _: Request<pb::ListProjectsRequest>,
    ) -> Result<Response<pb::ListProjectsResponse>, Status> {
        let projects = self
            .held
            .lock()
            .unwrap()
            .paths
            .iter()
            .map(|path| pb::Project {
                path: path.clone(),
                status: pb::ProjectStatus::Active as i32,
                ..Default::default()
            })
            .collect();
        Ok(Response::new(pb::ListProjectsResponse {
            projects,
            next_page_token: String::new(),
        }))
    }

    async fn touch_projects(
        &self,
        _: Request<pb::TouchProjectsRequest>,
    ) -> Result<Response<pb::TouchProjectsResponse>, Status> {
        Err(never("TouchProjects"))
    }

    async fn rename_project(
        &self,
        _: Request<pb::RenameProjectRequest>,
    ) -> Result<Response<pb::RenameProjectResponse>, Status> {
        Err(never("RenameProject"))
    }

    async fn archive_project(
        &self,
        _: Request<pb::ArchiveProjectRequest>,
    ) -> Result<Response<pb::ArchiveProjectResponse>, Status> {
        Err(never("ArchiveProject"))
    }

    async fn sync_projects(
        &self,
        request: Request<pb::SyncProjectsRequest>,
    ) -> Result<Response<pb::SyncProjectsResponse>, Status> {
        let req = request.into_inner();
        let mut held = self.held.lock().unwrap();
        held.syncs.push(req.clone());
        match &self.outcome {
            Ok(counts) => {
                held.paths = req
                    .roots
                    .iter()
                    .chain(req.projects.iter())
                    .map(|d| d.path.clone())
                    .collect();
                Ok(Response::new(*counts))
            }
            Err((code, message)) => Err(Status::new(*code, *message)),
        }
    }
}

/// Serve `store` on loopback (cleartext — the store hop's TLS is not under
/// test here) and return its address.
async fn serve_store(store: FakeStore) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = tonic::transport::Server::builder()
            .add_service(ProjectDbServiceServer::new(store))
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await;
    });
    addr
}

/// A `project` replica with ITS OWN channel to the store at `addr`, the way
/// each pod dials `project-db` for itself.
async fn replica(addr: SocketAddr) -> Project {
    let channel = Endpoint::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect()
        .await
        .expect("the fake store accepted the connection");
    Project::new(channel)
}

async fn project_over(store: &FakeStore) -> Project {
    replica(serve_store(store.clone()).await).await
}

// ---------------------------------------------------------------------------
// The authority, and `project` served over required mTLS.
// ---------------------------------------------------------------------------

const SERVED_NAME: &str = "localhost";

/// One authority, its serving leaf, and a way to mint named client leaves.
struct Pki {
    issuer: CertifiedIssuer<'static, KeyPair>,
    cert_pem: String,
    key_pem: String,
}

impl Pki {
    fn new() -> Self {
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        params
            .distinguished_name
            .push(DnType::CommonName, "project sync test authority");
        let issuer = CertifiedIssuer::self_signed(params, key).unwrap();

        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(vec![SERVED_NAME.to_string()]).unwrap();
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let cert = params.signed_by(&key, &issuer).unwrap();
        Self {
            issuer,
            cert_pem: cert.pem(),
            key_pem: key.serialize_pem(),
        }
    }

    /// A client leaf as `platform` issues one: `commonName: <module>-caller`,
    /// and a dNSName SAN equal to it.
    fn client(&self, common_name: &str) -> Identity {
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(vec![common_name.to_string()]).unwrap();
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        params
            .distinguished_name
            .push(DnType::CommonName, common_name);
        let cert = params.signed_by(&key, &self.issuer).unwrap();
        Identity::from_pem(cert.pem(), key.serialize_pem())
    }
}

/// A PEM file removed on drop. The name carries the pid and a counter, so
/// parallel cases never share one.
struct TempPem(PathBuf);

impl TempPem {
    fn new(body: &str) -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "project-sync-{}-{}.pem",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&path, body).unwrap();
        Self(path)
    }
}

impl Drop for TempPem {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Serve `project` through `serve::builder` under `required`, the mode every
/// ADR-0852 hop runs. Returns the port and the files the listener read.
async fn serve_project(pki: &Pki, project: Project) -> (u16, [TempPem; 3]) {
    let cert = TempPem::new(&pki.cert_pem);
    let key = TempPem::new(&pki.key_pem);
    let ca = TempPem::new(&pki.issuer.pem());
    let pairs = [
        ("LISTEN_TLS_ENABLED", "1".to_string()),
        ("LISTEN_TLS_CLIENT_AUTH", "required".to_string()),
        ("LISTEN_TLS_CERT_FILE", cert.0.display().to_string()),
        ("LISTEN_TLS_KEY_FILE", key.0.display().to_string()),
        ("LISTEN_TLS_CLIENT_CA_FILE", ca.0.display().to_string()),
    ];
    let tls = ServerTls::from_lookup(LISTEN, serve::CHART_KEY, |k| {
        pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone())
    })
    .expect("a complete mutual-TLS configuration")
    .expect("the flag is set");

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let router = serve::builder(Some(&tls), &limits())
        .unwrap()
        .add_service(ProjectServiceServer::new(project));
    tokio::spawn(async move {
        let _ = router
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await;
    });
    (port, [cert, key, ca])
}

/// A `ProjectService` client presenting `identity`.
async fn client_as(port: u16, pki: &Pki, identity: Identity) -> ProjectServiceClient<Channel> {
    let channel = Endpoint::from_shared(format!("https://127.0.0.1:{port}"))
        .unwrap()
        .connect_timeout(Duration::from_secs(10))
        .tls_config(
            ClientTlsConfig::new()
                .ca_certificate(Certificate::from_pem(pki.issuer.pem()))
                .domain_name(SERVED_NAME)
                .identity(identity),
        )
        .unwrap()
        .connect()
        .await
        .expect("the handshake completes");
    ProjectServiceClient::new(channel)
}

// ---------------------------------------------------------------------------
// Plan case 1 — the seeder's leaf reaches the store, field for field.
// ---------------------------------------------------------------------------

/// Over a real handshake: the seeder's leaf and principal reach the store, the
/// store receives the declaration FIELD FOR FIELD — the seeder's `request_id`
/// included, unchanged — and the five counts come back unchanged.
#[tokio::test]
async fn the_seeder_s_leaf_reaches_the_store_and_its_counts_come_back_unchanged() {
    let store = FakeStore::answering(Ok(store_counts()));
    let pki = Pki::new();
    let (port, _files) = serve_project(&pki, project_over(&store).await).await;

    let got = client_as(port, &pki, pki.client(SYNC_CALLER))
        .await
        .sync_projects(a_declaration(SEEDER_PRINCIPAL))
        .await
        .expect("the seeder is admitted")
        .into_inner();

    assert_eq!(
        got,
        pb::ProjectServiceSyncProjectsResponse {
            inserted: 3,
            updated: 5,
            archived: 7,
            reactivated: 11,
            unchanged: 13,
            grants_declared: 17,
            grants_inserted: 19,
            grants_deleted: 23,
            grants_unchanged: 29,
        }
    );
    let sent = a_declaration(SEEDER_PRINCIPAL);
    assert_eq!(
        store.syncs(),
        vec![pb::SyncProjectsRequest {
            scope: sent.scope,
            roots: sent.roots,
            projects: sent.projects,
            allow_mass_archive: true,
            allow_mass_revoke: true,
        }],
        "one store call, carrying the declaration and the seeder's Scope unchanged"
    );
    assert_eq!(
        store.syncs()[0].scope.as_ref().unwrap().request_id,
        SEEDER_REQUEST_ID
    );
}

// ---------------------------------------------------------------------------
// Plan cases 2 and 6 — every other verifying leaf is refused before the relay.
// ---------------------------------------------------------------------------

/// Every OTHER leaf the authority issues is refused PERMISSION_DENIED, with the
/// right principal in the Scope, and the store records ZERO calls. The near
/// misses are there because the comparison is EXACT: a leaf spelled in another
/// case, or with a trailing space, is not the seeder's leaf.
#[tokio::test]
async fn every_other_verifying_leaf_is_refused_before_the_store() {
    let store = FakeStore::answering(Ok(store_counts()));
    let pki = Pki::new();
    let (port, _files) = serve_project(&pki, project_over(&store).await).await;

    for name in [
        "iam-caller",
        "task-caller",
        "project-caller",
        "Seeder-caller",
        "seeder-caller ",
        "seeder",
    ] {
        let err = client_as(port, &pki, pki.client(name))
            .await
            .sync_projects(a_declaration(SEEDER_PRINCIPAL))
            .await
            .expect_err(name);
        assert_eq!(err.code(), Code::PermissionDenied, "{name:?}: {err:?}");
        assert!(
            err.message().contains(&format!("{name:?}")),
            "{name:?}: the refusal names the caller it read: {}",
            err.message()
        );
    }
    assert!(
        store.syncs().is_empty(),
        "a refused caller reached the store"
    );
}

/// Plan case 6: the GATEWAY's leaf is refused. The gateway never exposes this
/// verb on any route (ADR-0936); if a route were ever added, this check still
/// refuses it.
#[tokio::test]
async fn the_gateway_s_leaf_is_refused() {
    let store = FakeStore::answering(Ok(store_counts()));
    let pki = Pki::new();
    let (port, _files) = serve_project(&pki, project_over(&store).await).await;

    let err = client_as(port, &pki, pki.client("gateway-caller"))
        .await
        .sync_projects(a_declaration(SEEDER_PRINCIPAL))
        .await
        .expect_err("the gateway is not the seeder");
    assert_eq!(err.code(), Code::PermissionDenied);
    assert!(store.syncs().is_empty());
}

/// No identity at all — a cleartext listener, or a listener that verified no
/// client certificate — is a refusal, never an admission.
#[tokio::test]
async fn a_call_with_no_caller_identity_is_refused() {
    let store = FakeStore::answering(Ok(store_counts()));
    let err = project_over(&store)
        .await
        .sync_as(None, a_declaration(SEEDER_PRINCIPAL))
        .await
        .expect_err("no identity is no seeder");
    assert_eq!(err.code(), Code::PermissionDenied);
    assert!(err.message().contains("no caller"), "{}", err.message());
    assert!(store.syncs().is_empty());
}

// ---------------------------------------------------------------------------
// Plan case 3 — the seeder's leaf with a wrong principal is refused.
// ---------------------------------------------------------------------------

/// The certificate and the principal are checked TOGETHER, and neither is
/// trusted alone (ADR-0942). The seeder's own leaf carrying any other
/// `scope.user_id` — or no Scope at all — is refused PERMISSION_DENIED before
/// the relay.
#[tokio::test]
async fn the_seeder_s_leaf_with_any_other_principal_is_refused() {
    let store = FakeStore::answering(Ok(store_counts()));
    let project = project_over(&store).await;

    for user_id in [
        "",
        "alice",
        "yadgar:system:seeder ",
        "YADGAR:SYSTEM:SEEDER",
        "yadgar:system:gateway",
    ] {
        let err = project
            .sync_as(Some(SYNC_CALLER), a_declaration(user_id))
            .await
            .expect_err(user_id);
        assert_eq!(err.code(), Code::PermissionDenied, "{user_id:?}: {err:?}");
        assert!(
            err.message().contains(SEEDER_PRINCIPAL),
            "{user_id:?}: the refusal names the principal it wants: {}",
            err.message()
        );
    }

    let mut no_scope = a_declaration(SEEDER_PRINCIPAL);
    no_scope.scope = None;
    let err = project
        .sync_as(Some(SYNC_CALLER), no_scope)
        .await
        .expect_err("no Scope is no principal");
    assert_eq!(err.code(), Code::PermissionDenied);

    assert!(store.syncs().is_empty(), "a refused call reached the store");
}

/// The same refusal over a real handshake, so the principal check is proved to
/// run on the served path and not only on the seam.
#[tokio::test]
async fn over_the_wire_the_seeder_s_leaf_with_another_principal_is_refused() {
    let store = FakeStore::answering(Ok(store_counts()));
    let pki = Pki::new();
    let (port, _files) = serve_project(&pki, project_over(&store).await).await;

    let err = client_as(port, &pki, pki.client(SYNC_CALLER))
        .await
        .sync_projects(a_declaration("yadgar:system:not-the-seeder"))
        .await
        .expect_err("a wrong principal");
    assert_eq!(err.code(), Code::PermissionDenied);
    assert!(store.syncs().is_empty());
}

// ---------------------------------------------------------------------------
// Plan case 4 — a store refusal crosses with its code and its message.
// ---------------------------------------------------------------------------

/// Unlike `ListProjects` and `ResolveProject`, whose store messages are
/// withheld, this verb's refusals are relayed with their code AND message
/// unchanged (`ProjectService.SyncProjects` in the contract): the seeder logs
/// the reason, and the reason names the declared path an operator must fix.
/// FAILED_PRECONDITION is the ADR-0939 mass-archive guard (plan Q1).
#[tokio::test]
async fn a_store_refusal_is_relayed_with_its_code_and_message() {
    for (code, message) in [
        (
            Code::InvalidArgument,
            "path \"tapir/Bad Path\" is not a valid project path",
        ),
        (
            Code::AlreadyExists,
            "\"tapir/alpha\" is a former path of project yadgar:project:017",
        ),
        (
            Code::FailedPrecondition,
            "the sync would archive 9 of 12 active org-class rows; set allow_mass_archive",
        ),
        (
            Code::FailedPrecondition,
            "this sync would delete 3 of the 4 stored team grants, which is more than half \
             (ADR-0963 (6)), and nothing was written; send allow_mass_revoke",
        ),
        (
            Code::PermissionDenied,
            "SyncProjects is callable by `project` only",
        ),
        (Code::Unavailable, "the store is draining"),
    ] {
        let store = FakeStore::answering(Err((code, message)));
        let err = project_over(&store)
            .await
            .sync_as(Some(SYNC_CALLER), a_declaration(SEEDER_PRINCIPAL))
            .await
            .expect_err(message);
        assert_eq!(err.code(), code, "{message}");
        assert_eq!(err.message(), message);
        assert_eq!(store.syncs().len(), 1, "the store was asked once");
    }
}

// ---------------------------------------------------------------------------
// Plan case 5 — two replicas, and no replica-local state.
// ---------------------------------------------------------------------------

/// Two `project` replicas, each with its own channel to one store. Replica B
/// lists FIRST, so a replica that cached on first read would be caught: after
/// a sync through replica A, the next list through B returns the new set at
/// once. `project` holds no registry cache, and this pins it.
#[tokio::test]
async fn a_sync_through_one_replica_is_listed_by_the_other_at_once() {
    let store = FakeStore::answering(Ok(store_counts()));
    store.held.lock().unwrap().paths = vec!["tapir".into(), "tapir/old".into()];
    let addr = serve_store(store.clone()).await;
    let pki = Pki::new();
    let (port_a, _files_a) = serve_project(&pki, replica(addr).await).await;
    let (port_b, _files_b) = serve_project(&pki, replica(addr).await).await;

    let mut b = client_as(port_b, &pki, pki.client("gateway-caller")).await;
    let list = |paths: pb::ProjectServiceListProjectsResponse| -> Vec<String> {
        paths.paths.into_iter().map(|p| p.path).collect()
    };
    let before = b
        .list_projects(pb::ProjectServiceListProjectsRequest::default())
        .await
        .unwrap()
        .into_inner();
    assert_eq!(list(before), ["tapir", "tapir/old"]);

    client_as(port_a, &pki, pki.client(SYNC_CALLER))
        .await
        .sync_projects(a_declaration(SEEDER_PRINCIPAL))
        .await
        .expect("the seeder is admitted through replica A");

    let after = b
        .list_projects(pb::ProjectServiceListProjectsRequest::default())
        .await
        .unwrap()
        .into_inner();
    assert_eq!(list(after), ["tapir", "tapir/alpha", "tapir/beta"]);
}

// ---------------------------------------------------------------------------
// The two names, pinned by literal.
// ---------------------------------------------------------------------------

/// Both names are pinned by LITERAL (ADR-0599). `seeder-caller` is the common
/// name `platform` writes on the seeder's leaf, and `yadgar:system:seeder` is
/// the principal the seeder sends (ADR-0942). The seeder's repository shares
/// no crate with this one, so its own suite pins the same literal.
#[test]
fn the_admitted_names_are_the_ones_platform_and_the_seeder_use() {
    assert_eq!(SYNC_CALLER, "seeder-caller");
    assert_eq!(SEEDER_PRINCIPAL, "yadgar:system:seeder");
}

/// `ProjectService::sync_projects` is reachable as a trait method too, not
/// only as `sync_as`: a direct call carries no peer chain, so it is refused.
#[tokio::test]
async fn a_direct_handler_call_has_no_peer_chain_and_is_refused() {
    let store = FakeStore::answering(Ok(store_counts()));
    let err = project_over(&store)
        .await
        .sync_projects(Request::new(a_declaration(SEEDER_PRINCIPAL)))
        .await
        .expect_err("no peer chain");
    assert_eq!(err.code(), Code::PermissionDenied);
    assert!(store.syncs().is_empty());
}

// ---------------------------------------------------------------------------
// ListProjects' caller check, over the wire (ledger 1643).
// ---------------------------------------------------------------------------

/// The load is the gateway's alone: every OTHER leaf the authority issues —
/// the seeder's included — is refused `PERMISSION_DENIED` through the real
/// listener, and the gateway's own leaf is admitted. This is the half the
/// `list_as` cases in `tests/service.rs` cannot see: that the handler reads the
/// name off the certificate the listener verified, before `into_inner()`.
#[tokio::test]
async fn only_the_gateway_s_leaf_may_load_the_registry() {
    let store = FakeStore::answering(Ok(store_counts()));
    store.held.lock().unwrap().paths = vec!["tapir".into()];
    let pki = Pki::new();
    let (port, _files) = serve_project(&pki, project_over(&store).await).await;

    for name in [SYNC_CALLER, "iam-caller", "task-caller", "project-caller"] {
        let err = client_as(port, &pki, pki.client(name))
            .await
            .list_projects(pb::ProjectServiceListProjectsRequest::default())
            .await
            .expect_err(name);
        assert_eq!(err.code(), Code::PermissionDenied, "{name:?}: {err:?}");
    }

    let listed = client_as(port, &pki, pki.client("gateway-caller"))
        .await
        .list_projects(pb::ProjectServiceListProjectsRequest::default())
        .await
        .expect("the gateway's leaf is admitted")
        .into_inner();
    assert_eq!(listed.paths.len(), 1);
}

// ---------------------------------------------------------------------------
// Team access stage 5 — `allow_mass_revoke` absent reaches the store false.
// ---------------------------------------------------------------------------

/// The guard flag's DEFAULT is the guard ON. A declaration that does not set
/// it reaches the store with it false, and one that sets it reaches the store
/// true (the field-for-field case above); the fake asserts both.
#[tokio::test]
async fn red_stage5_an_absent_allow_mass_revoke_reaches_the_store_false() {
    let store = FakeStore::answering(Ok(store_counts()));
    let mut req = a_declaration(SEEDER_PRINCIPAL);
    req.allow_mass_revoke = false;
    project_over(&store)
        .await
        .sync_as(Some(SYNC_CALLER), req)
        .await
        .expect("relayed");
    let syncs = store.syncs();
    assert_eq!(syncs.len(), 1);
    assert!(!syncs[0].allow_mass_revoke);
    assert!(syncs[0].allow_mass_archive, "the other flag is untouched");
}
