# project

The project registry's **logic tier**: business rules, no store. It serves
`yadgar.project.v1.ProjectService` and reaches its data only over
`ProjectDbService`, in `project-db`.

Decisions in [`yadgarhq/docs`](https://github.com/yadgarhq/docs): D4 (the twin
split), D23 (client-side balancing), D52 and D53 (the registry and what a path
means), D70 (how the protos get here).

## It holds no store, and that absence is the design

There is no `sqlx` and no `yadgar-store` in this crate's dependency tree. A logic
service reaches its data only over the `-db` API — which is what makes the twin a
**connection concentrator** rather than merely a boundary. N replicas of this
service with embedded pools would multiply connections against an engine with
hard limits (D4).

`proto-contract-design.md` keeps a per-repo check that the binary has no store
SDK in its dependency tree, for exactly this reason.

## Two RPCs, and the other five are absent for stated reasons

`ProjectDbService` declares seven verbs. `ProjectService` declares two, and
`yadgar/project/v1/project.proto` gives the reason for each absence in its own
words. Mirroring seven verbs one for one would be a proxy, and a proxy earns
nothing over the store it wraps.

| rpc               | why it is not here                                                                                                                                                                                        |
| ----------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `ListProjects`    | **served** — the registry LOAD, so a caller resolves in memory rather than over the network on every request                                                                                              |
| `ResolveProject`  | **served** — the fallback, asked when the caller has no loaded set to walk                                                                                                                                |
| `RegisterProject` | administrative. The gateway surface meant to reach it does not exist, so it is reached by port-forwarding `project-db`                                                                                    |
| `RenameProject`   | administrative, and it answers `UNIMPLEMENTED` in `project-db` for a second reason: an alias keeps a stored `Meta.project_id` valid but not current, and the job that retags older records does not exist |
| `ArchiveProject`  | the same two reasons. The two return together with that job                                                                                                                                               |
| `GetProject`      | no caller. Inventing one to justify a verb is the shape the contract refuses                                                                                                                              |
| `TouchProjects`   | a call this tier will MAKE, never a verb it EXPOSES, and staged behind `queue`                                                                                                                            |

**Adding any of the five is a contract change first.** They are on no
`ProjectService` at any tag, so there is nothing here to implement.

## What it adds over `project-db`

`ResolveProject` is a pass-through on the RESPONSE — the two response messages
are field for field identical, and they are separate types only because buf's
`RPC_REQUEST_RESPONSE_UNIQUE` refuses to let two rpcs share one. What the tier
owns is the REQUEST it sends, and, on the list, three obligations the contract
states on the response:

- **The bare reserved segment is dropped.** `local` is reserved because the
  private class of project ids lives beneath it. A row at the bare segment
  becomes the nearest registered ancestor of every private path in the estate.
  The store refuses to MINT such a row and drops the segment from its ancestor
  chain, but a code-only guard does not delete a row an earlier build already
  accepted — so a caller walking this set performs a SECOND walk, and one
  carrying a `local` row re-opens exactly what the first closed. **The
  comparison folds ASCII case**, because the store's does: `project.path` takes
  the server default collation, measured `utf8mb4_uca1400_ai_ci`. `local/home`
  and `locals` both stay, and only the bare segment goes.
- **The set is not filtered by status.** The store's resolution walks live paths
  and aliases without consulting status and reports the status it found. A
  caller that dropped archived rows from its own walk would resolve a candidate
  to a different ancestor than `ResolveProject` resolves it to — two answers for
  one path, differing only in which route the caller took.
- **The page token is forwarded verbatim, in both directions.** The caller loops
  until it comes back empty. A partial set is not a smaller registry; it is a
  wrong one, and the value is bound into rows that no later pass can merge. The
  token is the store's keyset cursor over ITS last row, so it is never
  recomputed from the rows that survived the reserved-segment filter.

`exact: false` is neither an error nor a detail: it is the safety property that
makes resolving upward acceptable instead of refusing, and D52 chose the upward
resolution on the condition that the soft failure is made visible. It is
forwarded, never swallowed.

## The `Scope` this tier sends attests nothing, and that is open

`project-db` refuses a store call carrying no `yadgar.common.v1.Scope`, with the
message _"scope is required and is attested by the gateway"_. **That sentence is
false for this caller**, and `src/service.rs` says so where the value is made.

Neither `ProjectService` request carries a `Scope`, and neither can:
`ListProjects`'s caller is a boot load holding no token, no header and no
request, and `ResolveProject` is asked by the gateway WHILE CONSTRUCTING the
estate's only `Scope`. So there is nothing to forward. The store's check is
`Option::as_ref` plus `ok_or_else` and nothing more — a presence check, not an
authorisation one, and no handler in `project-db/src/read.rs` reads a field of
the value it demands. `Some(Scope::default())` therefore satisfies it.

**One site, deliberately.** `unattested_placeholder_scope` in `src/service.rs` is
a named function rather than `Default::default()` at two call sites, because the
ruling that replaces it is one edit whichever way it goes:
`plans/the-project-logic-tier.md` §6 names three routes — the tier mints one, the
contract grows one, or the store stops demanding one — and picks none.
`tests/service.rs` pins the current answer so that change trips a test.

The telemetry cost is the same under two of the three routes and so does not
choose between them: `project-db`'s `tel_scope` resolves an absent field with
`unwrap_or_default()`, and `request_id` is read only out of the `Scope` message
and never out of gRPC metadata. A minted default and a deleted check both leave
D67's `request_id` join empty for these two RPCs.

## Client-side balancing

gRPC holds **one** long-lived HTTP/2 connection. A normal Service balances at
connection time, so a client would open one connection, get one pod, and send
everything there for the life of the process — the other replicas idle while
looking healthy.

So `project-db`'s Service is **headless**: DNS returns every pod address and this
service balances across them itself. A background task re-resolves every 5s and
applies the difference to the channel's endpoint set. Two things the loop
deliberately does not do: it never acts on an empty resolution, and it never
tears down a working channel because DNS failed.

The balancing lives in the
[`yadgar-dial`](https://github.com/yadgarhq/dial) crate, pinned by tag in
`Cargo.toml`. What is here is `src/upstream.rs`: this service's decision about
which transport to reach `project-db` over.

## It does not wait for `project-db` to be ready

The boot dial is lazy (ADR-0532): the name is seeded into the balancer and
dialled until an address answers, so `connect` returns a channel and the failure
moves to the request. Blocking this service's startup on the twin would turn one
module's slow migration into a cascading outage, and under D68 a pod stuck in
startup is one the autoscaler cannot help.

**What that costs, said plainly.** The readiness probe is a `tcpSocket` on the
gRPC port, so this pod is Ready as soon as it is listening — and with
`project-db` absent it is Ready and answers `UNAVAILABLE` to every RPC it serves,
because this service is a facade over its twin and holds no state of its own.
The probe is deliberately not changed to gate on the upstream: D69's boot-failure
rule is about a capability of an engine the module OWNS, and this module owns
none, so the only thing it could gate on is an RPC asking `project-db` whether
`project-db` is up — inference by proxy, which D69's first rule refuses by name.

The discriminator that generalises is whether a restart could change the outcome.
A permanent gap — an unusable CA bundle, a missing client certificate, a host
that is not a URI authority — still fails boot. A transient absence dials lazily.

`passthrough` in `src/service.rs` is where the contract is kept: a `-db`
answering `UNAVAILABLE` or `DEADLINE_EXCEEDED` reaches the caller as
`UNAVAILABLE`, and `NOT_FOUND` reaches it as `NOT_FOUND` — one condition
carrying one code across both tiers, which is what the contract asks for. The
store's MESSAGE never reaches a caller: it may name tables and columns.

## SIGTERM, not SIGINT

Kubernetes ends a pod by sending **SIGTERM**, then waits out
`terminationGracePeriodSeconds` before SIGKILL. It never sends SIGINT. So
`yadgar_lifecycle::shutdown` listens for both, and it installs the handlers when
it is CALLED rather than when the future is first polled — a signal arriving in
that window would otherwise take SIGTERM's default disposition and kill the
process mid-request.

**The drain is bounded, because something other than a signal can start one.**
`rotate` ends the serve itself, and nothing outside the process bounds a drain
the process began: `terminationGracePeriodSeconds` never runs for a self-exit.
`yadgar_lifecycle::DRAIN_BUDGET` is 25s against this chart's 35s grace period; on
expiry the process logs an error and ends anyway. Its clock starts when shutdown
is REQUESTED, which is why the server is asked to stop through a channel rather
than handed the shutdown future. `tests/chart_grace_period.rs` holds the chart's
number to the constant this repository pins.

## A renewed certificate arrives by restart, not by reload

The certificate this service presents is read ONCE, when the listener is built,
and the client certificate it presents to `project-db` is read ONCE, inside the
dial. `tonic 0.14` cannot swap a running server's TLS configuration. cert-manager
renews before expiry and kubelet refreshes the mounted files — the chart mounts
those Secrets as DIRECTORIES rather than with `subPath` precisely so it does —
but nothing would make the process read them again (ADR-0523).

So `rotate::watch_set` hashes the files `main` opened, one digest per file, as
each is read. Three members, each earning its place for a different reason:

- **the serving certificate AND its key**, or the pair rotates half-watched;
- **the upstream CA bundle AND the client certificate and key** — the client
  certificate is the load-bearing one, because ADR-0516 records that an expired
  client leaf STOPS a hop rather than degrading it: the process works perfectly
  until a date and then cannot reach its own store, with no exit, no gauge
  movement and no log;
- **the mounted configuration document, unconditionally**, so an operator
  editing `shared.yaml` restarts the pod exactly as editing a certificate would.

When one of them changes it logs which file and the old and new leaf
fingerprint, waits out this pod's splay, drains, and returns. **A rotated
certificate is not an error, so the process exits 0.**

**A hash, never a modification time.** Kubelet rotates a mounted Secret by
renaming a new `..data` symlink over the old one, so every path resolves to a new
inode with a fresh mtime on every resync, changed or not.

**`watch_set` is a function, and that is the point rather than tidiness.** In
`task` the set was two builder calls forty lines apart in `main.rs`; no test
spawns the binary, so deleting either compiled and passed the whole suite.
`main.rs` and `tests/assembly.rs` call the SAME function here, so a deleted
member turns a test red. That test shipped in this module's first commit rather
than arriving in a later sweep.

## Local development

```bash
make proto     # refresh the vendored protos from PROTO_VERSION (D70)
cargo test     # they need no engine and no -db
```

`protoc` must be on `PATH` — types are generated, never hand-written (D16).

## Configuration

| variable                              | required? — and what the chart renders                           | what it is                                                                    |
| ------------------------------------- | ---------------------------------------------------------------- | ----------------------------------------------------------------------------- |
| `PROJECT_DB_HOST` / `PROJECT_DB_PORT` | required — from `projectDb.host` / `projectDb.port`              | the twin's headless Service                                                   |
| `LISTEN`                              | required — `0.0.0.0:50052`, matching the chart's `containerPort` | the gRPC address this service binds                                           |
| `METRICS_LISTEN`                      | required — `0.0.0.0:9090`                                        | the Prometheus endpoint (D67)                                                 |
| `RUST_LOG`                            | the binary falls back to `info` (an argued ADR-0569 exception)   | a DEFAULT, not `from_default_env`'s silence                                   |
| `LISTEN_TLS_ENABLED`                  | unset                                                            | exactly `1` to serve TLS; anything else is off                                |
| `LISTEN_TLS_CERT_FILE`                | unset                                                            | PEM certificate this service PRESENTS                                         |
| `LISTEN_TLS_KEY_FILE`                 | unset                                                            | its private key                                                               |
| `PROJECT_DB_TLS_ENABLED`              | unset                                                            | exactly `1` to dial `project-db` over TLS                                     |
| `PROJECT_DB_TLS_CA_FILE`              | unset                                                            | PEM bundle `project-db` is VERIFIED against                                   |
| `PROJECT_DB_TLS_DOMAIN`               | unset                                                            | only when the certificate names something else                                |
| `PROJECT_DB_TLS_CLIENT_CERT_FILE`     | unset                                                            | the certificate this service PRESENTS to `project-db` — mutual TLS (ADR-0516) |
| `PROJECT_DB_TLS_CLIENT_KEY_FILE`      | unset                                                            | its private key. Both or neither: half an identity is refused at boot         |

The rotation schedule is NOT an environment variable. It is read from
`yadgarhq/config`'s `shared.yaml` (`tlsRotation.pollSeconds` /
`splayMaxSeconds`), mounted at `/etc/yadgar/config/shared/shared.yaml`. There is
no compiled-in default: an absent or empty knob refuses the boot (ADR-0569).

**Three directions, and the prefix says which.** `LISTEN_TLS_*` configures the
listener — `LISTEN` is already the variable naming the address it binds.
`PROJECT_DB_TLS_*` configures the dial, and a dial is named for the upstream it
reaches. Within the dial, `PROJECT_DB_TLS_CA_FILE` is how this service VERIFIES
`project-db` and `PROJECT_DB_TLS_CLIENT_*` is what it PRESENTS to `project-db` —
the same word, opposite directions.

Both groups are **opt-in and off**, and every enable flag is exactly the string
`1` — a permissive parse is how a setting meant to be off ends up on, and how a
revert lever stops moving. A flag that is on with a file that is missing,
unreadable or unusable **refuses the boot naming the file**. It never falls back
to cleartext.

## What this release deliberately does not do

- **It publishes no NATS events.** A registry-change event needs a broker
  account, and it hits a wall before that: `RegisterProject` is on
  `ProjectDbService` only and `project-db` has no NATS client, so a registration
  performed the way registrations are performed today is invisible to this
  process and cannot produce an event. There is no NATS dependency in this crate.
- **It has no `src/boot.rs`.** There is no engine to probe and nothing to
  migrate; D69's boot-failure rule is about a capability of an engine the module
  OWNS.
- **It ships no `chart/templates/serviceaccount.yaml`.** `iam` declares one;
  `task` and `project-db` do not, and nothing in the estate selects on a
  ServiceAccount — the NetworkPolicies that exist select on pod labels. Matching
  the twin keeps the pair symmetric.
- **It ships no `MIGRATION_NOTES.md`.** This module's operator steps are all in
  `yadgarhq/deploy` and on GitHub.
