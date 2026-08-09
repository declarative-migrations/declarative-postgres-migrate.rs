#![forbid(unsafe_code)]

//! Typed-plan alias analysis for declarative schema migrations.
//!
//! This module complements [`crate::formal`] and [`crate::lease`]:
//!
//! - `plan_safety` proves that every proposed execution wave has pairwise
//!   compatible shared/exclusive borrows over hierarchical schema resources;
//! - `formal` consumes the resulting certified plan into a linear validated
//!   migration capability;
//! - `lease` owns the PostgreSQL session and advisory lock used for execution.
//!
//! A raw [`Plan`] cannot enter the validated typestate directly through this
//! API. It must first be consumed into [`CertifiedPlan`]:
//!
//! ```
//! use dpm::diff::Plan;
//!
//! let migration = Plan { changes: vec![] }
//!     .certify()
//!     .into_validated_migration();
//! assert_eq!(migration.proof().checked_items(), 0);
//! ```
//!
//! ```compile_fail
//! use dpm::diff::Plan;
//!
//! let plan = Plan { changes: vec![] };
//! let _ = plan.into_validated_migration();
//! ```

mod certificate;
mod resource;

pub use certificate::{
    certify_plan, check_parallel_changes, check_parallel_steps, CertificateError, CertifiedPlan,
    ExecutionWave, PlanCertificate,
};
pub use resource::{
    BorrowConflict, BorrowMode, BorrowScope, BorrowedStep, ResourceBorrow, ResourcePath,
};

use crate::diff::Plan;

/// Increment when resource or certificate semantics change incompatibly.
pub const PLAN_SAFETY_MODEL_VERSION: u32 = 1;

impl Plan {
    /// Build a deterministic plan-level borrow certificate without consuming
    /// the typed plan.
    pub fn borrow_check(&self) -> PlanCertificate {
        certify_plan(self)
    }

    /// Consume this plan into the linear capability that bridges plan-level
    /// certification into the formal migration typestate.
    pub fn certify(self) -> CertifiedPlan {
        CertifiedPlan::new(self)
    }
}
