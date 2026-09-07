//! `project` — the project registry's logic service.
//!
//! **It holds no store, and that absence is the design (D4).** There is no sqlx
//! and no `yadgar-store` in this crate's dependency tree: a logic service reaches
//! its data only over the `-db` API, which is what makes the twin a connection
//! concentrator rather than merely a boundary. N replicas of this service with
//! embedded pools would multiply connections against an engine with hard limits.

#![forbid(unsafe_code)]

pub mod rotate;
pub mod serve;
pub mod service;
pub mod upstream;

/// Generated from the vendored contract (D16, D70). The module tree mirrors the
/// protobuf package path — generated cross-package references are emitted as
/// `super::super::common::v1::Meta`, so a flattened tree fails to compile.
///
/// ONE PACKAGE CARRIES BOTH SERVICES here, unlike `task`, where the API and the
/// store live in `yadgar.taskapi.v1` and `yadgar.task.v1`. `ProjectService` and
/// `ProjectDbService` are both declared in `yadgar/project/v1/project.proto`, so
/// `project_service_server` and `project_db_service_client` are siblings in one
/// module.
pub mod pb {
    pub mod yadgar {
        pub mod common {
            pub mod v1 {
                tonic::include_proto!("yadgar.common.v1");
            }
        }
        pub mod project {
            pub mod v1 {
                // THE ONE SUPPRESSION IN THIS CRATE, AND IT IS OVER GENERATED
                // CODE THIS REPOSITORY CANNOT EDIT.
                //
                // `project.proto`'s comment above `service ProjectService` is a
                // bullet list whose continuation lines are flush with the
                // marker. prost copies a proto comment through verbatim as a
                // `///` doc comment, and `clippy::doc_lazy_continuation` — part
                // of `clippy::all`, which this crate DENIES — reads such a line
                // as a paragraph that lost its indentation. Measured: 24 errors,
                // every one of them in
                // `target/.../out/yadgar.project.v1.rs`, none in hand-written
                // code, and none in `yadgar.common.v1.rs`.
                //
                // NO OTHER REPOSITORY HAS MET IT, and that is not luck: `task`,
                // `iam` and `project-db` all pin `PROTO_VERSION` at `v1.10.2`,
                // and the comment that trips it arrived with `ProjectService` in
                // `v1.11.0`. This repository is the first to pin `v1.11.x`, so
                // it is the first to compile that comment.
                //
                // SCOPED TO THE GENERATED MODULE, never crate-wide: a
                // hand-written lazy continuation anywhere else in this crate
                // still fails the build. The fix that removes it belongs in
                // `yadgarhq/proto` — indent the continuation lines — and this
                // suppression goes with it.
                #![allow(clippy::doc_lazy_continuation)]

                tonic::include_proto!("yadgar.project.v1");
            }
        }
    }
}
