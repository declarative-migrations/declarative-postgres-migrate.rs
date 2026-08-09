//! # declarative-postgres-migrate (dpm)
//!
//! ORM-agnostic, declarative PostgreSQL and CockroachDB schema migration. The
//! core idea: each server's compatible catalog is the neutral interchange
//! format — it doesn't matter whether a schema was authored by Prisma, Drizzle, SeaORM, ent,
//! peewee, or raw SQL. dpm introspects two states (live database, saved
//! catalog dump, or a `.sql` file materialized via a shadow database),
//! diffs the catalogs, and emits ordered, reviewable SQL that converges the
//! target onto the source.
//!
//! Library layers (each usable on its own):
//! - [`model`]: the serializable [`model::Catalog`] snapshot.
//! - [`introspect`]: live database → `Catalog` (canonical `search_path = ''`
//!   deparsing, extension-owned objects excluded).
//! - [`diff()`]: `Catalog` × `Catalog` → typed [`diff::Plan`]. Pure.
//! - [`plan_safety`]: typed-plan resource borrows and exact plan certificates.
//! - [`emit()`]: `Plan` → ordered SQL script with destructive-change gating.
//! - [`apply`]: statement-splitting executor.
//! - [`formal`]: typestate capabilities and a bounded migration model checker.
//! - [`lease`]: borrow-checked PostgreSQL advisory leases and validated scripts.
//! - [`verify`]: replay the migration on a shadow replica and prove
//!   convergence; optional external cross-checkers (migra, pgdiff, ...).
//! - [`advisor`]: non-DDL advice (foreign keys lacking supporting indexes).
//! - [`source`]: resolve a URL / `.json` dump / `.sql` file into a `Catalog`.
//! - [`flagenv`]: flags-2-env CLI contract (flags ↔ env vars).

pub mod advisor;
pub mod ai;
pub mod apply;
pub mod canonicalize;
// `DirBuilder::mode` mutates the scratch-directory builder only on Unix. Keep
// Windows strict-lint builds focused while preserving mode 0o700 on Unix.
#[cfg_attr(windows, allow(unused_mut))]
pub mod crosscheck;
pub mod diff;
pub mod emit;
pub mod flagenv;
pub mod formal;
pub mod interfaces;
pub mod introspect;
pub mod lease;
pub mod model;
pub mod plan_safety;
pub mod source;
pub mod sync;
pub mod verify;

pub use diff::{diff, Change, Plan};
pub use emit::{emit, EmitOptions, Script};
pub use introspect::{introspect_url, IntrospectOptions};
pub use model::{Catalog, DatabaseFlavor, RoutineKind, TriggerMode};
pub use plan_safety::{
    certify_plan, check_parallel_changes, BorrowConflict, BorrowMode, BorrowScope, CertifiedPlan,
    PlanCertificate, ResourceBorrow, ResourcePath,
};
