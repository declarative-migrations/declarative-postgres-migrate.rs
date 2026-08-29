//! Deterministic certificates and conflict-free typed-plan waves.

use std::error::Error;
use std::fmt;

use serde::Serialize;

use crate::diff::{Change, Plan};
use crate::formal::{stable_fingerprint, Migration, Validated, ValidationProof};

use super::resource::{first_conflict, step_conflicts_with_any, BorrowConflict, BorrowedStep};
use super::PLAN_SAFETY_MODEL_VERSION;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ExecutionWave {
    pub index: usize,
    pub steps: Vec<BorrowedStep>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PlanCertificate {
    pub model_version: u32,
    pub fingerprint: u64,
    pub change_count: usize,
    pub destructive_steps: Vec<usize>,
    pub manual_steps: Vec<usize>,
    pub waves: Vec<ExecutionWave>,
}

impl PlanCertificate {
    pub fn validate(&self) -> Result<(), CertificateError> {
        if self.model_version != PLAN_SAFETY_MODEL_VERSION {
            return Err(CertificateError::ModelVersion {
                expected: PLAN_SAFETY_MODEL_VERSION,
                actual: self.model_version,
            });
        }

        let mut next_index = 0usize;
        for (expected_wave, wave) in self.waves.iter().enumerate() {
            if wave.index != expected_wave {
                return Err(CertificateError::WaveOrder {
                    expected: expected_wave,
                    actual: wave.index,
                });
            }
            if wave.steps.is_empty() {
                return Err(CertificateError::EmptyWave { wave: wave.index });
            }
            check_parallel_steps(&wave.steps).map_err(CertificateError::BorrowConflict)?;
            for step in &wave.steps {
                if step.index != next_index {
                    return Err(CertificateError::StepOrder {
                        expected: next_index,
                        actual: step.index,
                    });
                }
                next_index += 1;
            }
        }

        if next_index != self.change_count {
            return Err(CertificateError::ChangeCount {
                expected: self.change_count,
                actual: next_index,
            });
        }
        Ok(())
    }

    /// Validate both certificate structure and exact correspondence to `plan`.
    /// Structural validation alone cannot prove that a certificate belongs to
    /// one particular plan.
    pub fn validate_for(&self, plan: &Plan) -> Result<(), CertificateError> {
        self.validate()?;
        let expected = certify_plan(plan);
        if self != &expected {
            return Err(CertificateError::PlanMismatch);
        }
        Ok(())
    }

    pub fn wave_count(&self) -> usize {
        self.waves.len()
    }

    pub fn requires_destructive_approval(&self) -> bool {
        !self.destructive_steps.is_empty()
    }

    pub fn requires_manual_approval(&self) -> bool {
        !self.manual_steps.is_empty()
    }
}

/// A typed plan paired with the exact certificate computed from it.
///
/// The fields are private and this capability is intentionally not `Clone` or
/// `Copy`. Consuming it into [`Migration`] preserves a linear path from plan
/// certification to lease authorization.
#[must_use = "certified plans must be executed, inspected, or explicitly discarded"]
#[derive(Debug)]
pub struct CertifiedPlan {
    plan: Plan,
    certificate: PlanCertificate,
}

impl CertifiedPlan {
    pub fn new(plan: Plan) -> Self {
        let certificate = certify_plan(&plan);
        Self { plan, certificate }
    }

    pub fn plan(&self) -> &Plan {
        &self.plan
    }

    pub fn certificate(&self) -> &PlanCertificate {
        &self.certificate
    }

    pub fn into_parts(self) -> (Plan, PlanCertificate) {
        (self.plan, self.certificate)
    }

    /// Consume this certificate into the validated typestate supplied by the
    /// execution-level formal model.
    pub fn into_validated_migration(self) -> Migration<Self, Validated> {
        let encoded = serde_json::to_vec(&self.certificate)
            .expect("PlanCertificate serialization is infallible");
        let checked_items = self.certificate.change_count;
        Migration::new(self).validate(move |_| ValidationProof::for_bytes(&encoded, checked_items))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CertificateError {
    ModelVersion { expected: u32, actual: u32 },
    WaveOrder { expected: usize, actual: usize },
    EmptyWave { wave: usize },
    StepOrder { expected: usize, actual: usize },
    ChangeCount { expected: usize, actual: usize },
    PlanMismatch,
    BorrowConflict(BorrowConflict),
}

impl fmt::Display for CertificateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ModelVersion { expected, actual } => write!(
                f,
                "plan safety model version mismatch: expected {expected}, got {actual}"
            ),
            Self::WaveOrder { expected, actual } => write!(
                f,
                "certificate wave order mismatch: expected {expected}, got {actual}"
            ),
            Self::EmptyWave { wave } => write!(f, "execution wave {wave} is empty"),
            Self::StepOrder { expected, actual } => write!(
                f,
                "certificate step order mismatch: expected {expected}, got {actual}"
            ),
            Self::ChangeCount { expected, actual } => write!(
                f,
                "certificate change count mismatch: expected {expected}, got {actual}"
            ),
            Self::PlanMismatch => write!(f, "certificate does not match the migration plan"),
            Self::BorrowConflict(conflict) => conflict.fmt(f),
        }
    }
}

impl Error for CertificateError {}

/// Certify a plan into deterministic, order-preserving, conflict-free waves.
///
/// The scheduler only groups adjacent non-conflicting changes. It never moves
/// a change ahead of another change, so dependency ordering from the diff and
/// emitter remains authoritative.
pub fn certify_plan(plan: &Plan) -> PlanCertificate {
    let mut waves = Vec::<ExecutionWave>::new();
    let mut current = Vec::<BorrowedStep>::new();

    for (index, change) in plan.changes.iter().enumerate() {
        let step = BorrowedStep::from_change(index, change);
        if !current.is_empty() && step_conflicts_with_any(&step, &current) {
            let wave_index = waves.len();
            waves.push(ExecutionWave {
                index: wave_index,
                steps: std::mem::take(&mut current),
            });
        }
        current.push(step);
    }

    if !current.is_empty() {
        waves.push(ExecutionWave {
            index: waves.len(),
            steps: current,
        });
    }

    let serialized = serde_json::to_vec(&plan.changes)
        .expect("Change serialization is infallible for the in-memory plan model");
    let certificate = PlanCertificate {
        model_version: PLAN_SAFETY_MODEL_VERSION,
        fingerprint: stable_fingerprint(&serialized),
        change_count: plan.changes.len(),
        destructive_steps: plan
            .changes
            .iter()
            .enumerate()
            .filter_map(|(index, change)| change.is_destructive().then_some(index))
            .collect(),
        manual_steps: plan
            .changes
            .iter()
            .enumerate()
            .filter_map(|(index, change)| change.is_manual().then_some(index))
            .collect(),
        waves,
    };
    debug_assert!(certificate.validate().is_ok());
    certificate
}

/// Validate a caller-proposed parallel batch without rescheduling it.
pub fn check_parallel_changes(changes: &[&Change]) -> Result<(), BorrowConflict> {
    let steps = changes
        .iter()
        .enumerate()
        .map(|(index, change)| BorrowedStep::from_change(index, change))
        .collect::<Vec<_>>();
    check_parallel_steps(&steps)
}

pub fn check_parallel_steps(steps: &[BorrowedStep]) -> Result<(), BorrowConflict> {
    for (left_index, left) in steps.iter().enumerate() {
        for right in &steps[left_index + 1..] {
            if let Some((left_borrow, right_borrow)) = first_conflict(left, right) {
                return Err(BorrowConflict {
                    left_step: left.index,
                    right_step: right.index,
                    left: left_borrow.clone(),
                    right: right_borrow.clone(),
                });
            }
        }
    }
    Ok(())
}

/// Identity checksum of a reviewed typed plan plus its emitted SQL.
///
/// This is not a cryptographic signature. Callers that need cross-process
/// trust still pin immutable commits and signed artifacts. Apply refuses
/// writes when this value drifts after the lease is held, or when
/// `--require-plan-checksum` does not match.
pub fn reviewed_plan_checksum(plan: &Plan, sql: &str) -> String {
    let certificate = certify_plan(plan);
    let mut payload = Vec::new();
    payload.extend_from_slice(&super::PLAN_SAFETY_MODEL_VERSION.to_le_bytes());
    payload.extend_from_slice(&certificate.fingerprint.to_le_bytes());
    payload.extend_from_slice(&stable_fingerprint(sql.as_bytes()).to_le_bytes());
    payload.extend_from_slice(&(sql.len() as u64).to_le_bytes());
    payload.extend_from_slice(sql.as_bytes());
    format!("{:016x}", stable_fingerprint(&payload))
}

/// Compare operator-supplied and computed plan checksums without regard to
/// surrounding whitespace or hex case.
pub fn checksums_match(expected: &str, actual: &str) -> bool {
    expected.trim().eq_ignore_ascii_case(actual.trim())
}
