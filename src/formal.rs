#![forbid(unsafe_code)]

//! Formal, ownership-oriented contracts for migration execution.
//!
//! This module provides two complementary assurance layers:
//!
//! - a typestate API that makes validation and lease authorization explicit;
//! - a small finite-state model that can be exhaustively checked over bounded
//!   traces and fuzzed with property tests.
//!
//! The typestate values are intentionally not `Clone` or `Copy`. A validated
//! migration is a linear capability: it can be authorized once and then
//! consumed into an applied or aborted terminal state.
//!
//! ```
//! use dpm::formal::{LeaseState, Migration, OwnerId, ValidationProof};
//!
//! let mut leases = LeaseState::default();
//! let guard = leases
//!     .acquire(OwnerId::new("runner-1").unwrap())
//!     .unwrap();
//! let migration = Migration::new("SELECT 1")
//!     .validate(|sql| ValidationProof::for_bytes(sql.as_bytes(), 1));
//! let applied = migration.authorize(&guard).finish();
//! assert_eq!(applied.into_inner(), "SELECT 1");
//! let receipt = guard.release();
//! assert_eq!(receipt.epoch(), 1);
//! ```
//!
//! An unvalidated migration has no `authorize` method:
//!
//! ```compile_fail
//! use dpm::formal::{LeaseState, Migration, OwnerId};
//!
//! let mut leases = LeaseState::default();
//! let guard = leases
//!     .acquire(OwnerId::new("runner-1").unwrap())
//!     .unwrap();
//! let draft = Migration::new("SELECT 1");
//! let _ = draft.authorize(&guard);
//! ```
//!
//! The lease guard holds the only mutable borrow of the lease state, so a
//! second acquisition cannot even be attempted while the first guard lives:
//!
//! ```compile_fail
//! use dpm::formal::{LeaseState, OwnerId};
//!
//! let mut leases = LeaseState::default();
//! let first = leases
//!     .acquire(OwnerId::new("runner-1").unwrap())
//!     .unwrap();
//! let second = leases.acquire(OwnerId::new("runner-2").unwrap());
//! drop((first, second));
//! ```

use std::error::Error;
use std::fmt;
use std::marker::PhantomData;

/// Draft typestate: the payload has not crossed a validation boundary.
#[derive(Debug)]
pub enum Draft {}

/// Validated typestate: a validation proof is attached to the payload.
#[derive(Debug)]
pub enum Validated {}

/// Applied terminal typestate.
#[derive(Debug)]
pub enum Applied {}

/// Aborted terminal typestate.
#[derive(Debug)]
pub enum Aborted {}

/// Evidence produced by a validation boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ValidationProof {
    checked_items: usize,
    fingerprint: u64,
}

impl ValidationProof {
    /// Build deterministic evidence from the bytes that were checked.
    pub fn for_bytes(bytes: &[u8], checked_items: usize) -> Self {
        Self {
            checked_items,
            fingerprint: stable_fingerprint(bytes),
        }
    }

    pub fn checked_items(self) -> usize {
        self.checked_items
    }

    pub fn fingerprint(self) -> u64 {
        self.fingerprint
    }
}

/// Stable FNV-1a fingerprint used in proofs and audit receipts.
///
/// This is an identity checksum, not a cryptographic signature.
pub fn stable_fingerprint(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// A linear migration capability parameterized by its typestate.
#[must_use = "migration capabilities must be consumed into an explicit state"]
#[derive(Debug)]
pub struct Migration<T, State> {
    payload: T,
    proof: Option<ValidationProof>,
    state: PhantomData<State>,
}

impl<T, State> Migration<T, State> {
    pub fn payload(&self) -> &T {
        &self.payload
    }

    fn transition<Next>(self) -> Migration<T, Next> {
        Migration {
            payload: self.payload,
            proof: self.proof,
            state: PhantomData,
        }
    }
}

impl<T> Migration<T, Draft> {
    pub fn new(payload: T) -> Self {
        Self {
            payload,
            proof: None,
            state: PhantomData,
        }
    }

    /// Cross an infallible validation boundary.
    pub fn validate(
        self,
        validate: impl FnOnce(&T) -> ValidationProof,
    ) -> Migration<T, Validated> {
        let proof = validate(&self.payload);
        Migration {
            payload: self.payload,
            proof: Some(proof),
            state: PhantomData,
        }
    }

    /// Cross a fallible validation boundary.
    pub fn try_validate<E>(
        self,
        validate: impl FnOnce(&T) -> Result<ValidationProof, E>,
    ) -> Result<Migration<T, Validated>, E> {
        let proof = validate(&self.payload)?;
        Ok(Migration {
            payload: self.payload,
            proof: Some(proof),
            state: PhantomData,
        })
    }
}

impl<T> Migration<T, Validated> {
    pub fn proof(&self) -> ValidationProof {
        self.proof
            .expect("validated migrations always carry validation evidence")
    }

    /// Borrow an active lease and produce an authorized execution capability.
    ///
    /// The returned value cannot outlive `lease`, and `lease` cannot be
    /// released while the authorization remains live.
    pub fn authorize<'guard, 'state>(
        self,
        lease: &'guard LeaseGuard<'state>,
    ) -> AuthorizedMigration<'guard, 'state, T> {
        AuthorizedMigration {
            migration: self,
            lease,
        }
    }
}

impl<T> Migration<T, Applied> {
    pub fn into_inner(self) -> T {
        self.payload
    }
}

impl<T> Migration<T, Aborted> {
    pub fn into_inner(self) -> T {
        self.payload
    }
}

/// A validated migration tied to the lifetime of one active lease guard.
#[must_use = "authorized migrations must be finished or aborted"]
#[derive(Debug)]
pub struct AuthorizedMigration<'guard, 'state, T> {
    migration: Migration<T, Validated>,
    lease: &'guard LeaseGuard<'state>,
}

impl<T> AuthorizedMigration<'_, '_, T> {
    pub fn payload(&self) -> &T {
        self.migration.payload()
    }

    pub fn proof(&self) -> ValidationProof {
        self.migration.proof()
    }

    pub fn owner(&self) -> &OwnerId {
        self.lease.owner()
    }

    pub fn epoch(&self) -> u64 {
        self.lease.epoch()
    }

    pub fn finish(self) -> Migration<T, Applied> {
        self.migration.transition()
    }

    pub fn abort(self) -> Migration<T, Aborted> {
        self.migration.transition()
    }
}

/// Non-empty migration owner identity.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct OwnerId(String);

impl OwnerId {
    pub fn new(value: impl Into<String>) -> Result<Self, OwnerIdError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(OwnerIdError);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for OwnerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OwnerIdError;

impl fmt::Display for OwnerIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("migration owner identity must not be empty")
    }
}

impl Error for OwnerIdError {}

/// In-memory lease state used by the typestate API and deterministic tests.
///
/// The PostgreSQL-backed execution lease lives in [`crate::lease`].
#[derive(Debug, Default)]
pub struct LeaseState {
    owner: Option<OwnerId>,
    epoch: u64,
}

impl LeaseState {
    pub fn owner(&self) -> Option<&OwnerId> {
        self.owner.as_ref()
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn acquire(&mut self, owner: OwnerId) -> Result<LeaseGuard<'_>, LeaseError> {
        if let Some(current) = &self.owner {
            return Err(LeaseError::AlreadyOwned {
                owner: current.clone(),
                epoch: self.epoch,
            });
        }
        let epoch = self
            .epoch
            .checked_add(1)
            .ok_or(LeaseError::EpochOverflow)?;
        self.epoch = epoch;
        self.owner = Some(owner.clone());
        Ok(LeaseGuard {
            state: self,
            owner,
            epoch,
            released: false,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LeaseError {
    AlreadyOwned { owner: OwnerId, epoch: u64 },
    EpochOverflow,
}

impl fmt::Display for LeaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyOwned { owner, epoch } => {
                write!(f, "migration lease is owned by {owner} at epoch {epoch}")
            }
            Self::EpochOverflow => f.write_str("migration lease epoch overflowed"),
        }
    }
}

impl Error for LeaseError {}

/// Unique borrow of a [`LeaseState`].
#[must_use = "dropping a lease guard releases ownership; call release for a receipt"]
#[derive(Debug)]
pub struct LeaseGuard<'state> {
    state: &'state mut LeaseState,
    owner: OwnerId,
    epoch: u64,
    released: bool,
}

impl LeaseGuard<'_> {
    pub fn owner(&self) -> &OwnerId {
        &self.owner
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn release(mut self) -> LeaseReceipt {
        self.state.owner = None;
        self.released = true;
        LeaseReceipt {
            owner: self.owner.clone(),
            epoch: self.epoch,
        }
    }
}

impl Drop for LeaseGuard<'_> {
    fn drop(&mut self) {
        if !self.released && self.state.owner.as_ref() == Some(&self.owner) {
            self.state.owner = None;
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeaseReceipt {
    owner: OwnerId,
    epoch: u64,
}

impl LeaseReceipt {
    pub fn owner(&self) -> &OwnerId {
        &self.owner
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }
}

/// Abstract phases in the executable migration state machine.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Phase {
    Draft,
    Validated,
    Leased,
    Applied,
    Aborted,
}

/// Actions accepted by the abstract migration state machine.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Action {
    Validate,
    Acquire(u8),
    Release(u8),
    Apply(u8),
    Abort(u8),
}

/// Compact state used by the bounded model checker.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ModelState {
    phase: Phase,
    owner: Option<u8>,
    epoch: u64,
    validated_once: bool,
}

impl ModelState {
    pub const fn initial() -> Self {
        Self {
            phase: Phase::Draft,
            owner: None,
            epoch: 0,
            validated_once: false,
        }
    }

    pub fn phase(self) -> Phase {
        self.phase
    }

    pub fn owner(self) -> Option<u8> {
        self.owner
    }

    pub fn epoch(self) -> u64 {
        self.epoch
    }

    pub fn step(self, action: Action) -> Result<Self, TransitionError> {
        let next = match (self.phase, action) {
            (Phase::Draft, Action::Validate) => Self {
                phase: Phase::Validated,
                validated_once: true,
                ..self
            },
            (Phase::Validated, Action::Acquire(owner)) => Self {
                phase: Phase::Leased,
                owner: Some(owner),
                epoch: self.epoch.checked_add(1).ok_or(TransitionError {
                    state: self,
                    action,
                    reason: "lease epoch overflow",
                })?,
                ..self
            },
            (Phase::Leased, Action::Release(owner)) if self.owner == Some(owner) => Self {
                phase: Phase::Validated,
                owner: None,
                ..self
            },
            (Phase::Leased, Action::Apply(owner)) if self.owner == Some(owner) => Self {
                phase: Phase::Applied,
                owner: None,
                ..self
            },
            (Phase::Leased, Action::Abort(owner)) if self.owner == Some(owner) => Self {
                phase: Phase::Aborted,
                owner: None,
                ..self
            },
            _ => {
                return Err(TransitionError {
                    state: self,
                    action,
                    reason: "action is not enabled in the current state",
                });
            }
        };
        next.check_invariants().map_err(|violation| TransitionError {
            state: violation.state,
            action,
            reason: violation.reason,
        })?;
        Ok(next)
    }

    pub fn check_invariants(self) -> Result<(), InvariantViolation> {
        let leased = self.phase == Phase::Leased;
        if leased != self.owner.is_some() {
            return Err(InvariantViolation {
                state: self,
                reason: "exactly one owner must exist exactly while leased",
            });
        }
        if self.phase == Phase::Draft && self.validated_once {
            return Err(InvariantViolation {
                state: self,
                reason: "draft state cannot carry validation history",
            });
        }
        if self.phase != Phase::Draft && !self.validated_once {
            return Err(InvariantViolation {
                state: self,
                reason: "non-draft state requires prior validation",
            });
        }
        if matches!(self.phase, Phase::Applied | Phase::Aborted) && self.epoch == 0 {
            return Err(InvariantViolation {
                state: self,
                reason: "terminal execution requires a prior lease epoch",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransitionError {
    pub state: ModelState,
    pub action: Action,
    pub reason: &'static str,
}

impl fmt::Display for TransitionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "transition {:?} from {:?} rejected: {}",
            self.action, self.state, self.reason
        )
    }
}

impl Error for TransitionError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvariantViolation {
    pub state: ModelState,
    pub reason: &'static str,
}

impl fmt::Display for InvariantViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "state {:?} violates invariant: {}", self.state, self.reason)
    }
}

impl Error for InvariantViolation {}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ModelCheckReport {
    pub states_checked: usize,
    pub transition_attempts: usize,
    pub accepted_transitions: usize,
    pub max_depth: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Counterexample {
    pub trace: Vec<Action>,
    pub violation: InvariantViolation,
}

impl fmt::Display for Counterexample {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "counterexample {:?}: {}",
            self.trace, self.violation
        )
    }
}

impl Error for Counterexample {}

/// Exhaustively check every enabled transition up to `depth` for the supplied
/// finite set of owner identities.
pub fn bounded_model_check(
    depth: usize,
    owners: &[u8],
) -> Result<ModelCheckReport, Counterexample> {
    let mut alphabet = Vec::with_capacity(1 + owners.len() * 4);
    alphabet.push(Action::Validate);
    for owner in owners {
        alphabet.extend([
            Action::Acquire(*owner),
            Action::Release(*owner),
            Action::Apply(*owner),
            Action::Abort(*owner),
        ]);
    }

    let mut report = ModelCheckReport {
        max_depth: depth,
        ..ModelCheckReport::default()
    };
    let mut trace = Vec::with_capacity(depth);
    visit_model(
        ModelState::initial(),
        depth,
        &alphabet,
        &mut trace,
        &mut report,
    )?;
    Ok(report)
}

fn visit_model(
    state: ModelState,
    remaining: usize,
    alphabet: &[Action],
    trace: &mut Vec<Action>,
    report: &mut ModelCheckReport,
) -> Result<(), Counterexample> {
    report.states_checked += 1;
    state.check_invariants().map_err(|violation| Counterexample {
        trace: trace.clone(),
        violation,
    })?;
    if remaining == 0 {
        return Ok(());
    }

    for action in alphabet {
        report.transition_attempts += 1;
        if let Ok(next) = state.step(*action) {
            report.accepted_transitions += 1;
            trace.push(*action);
            visit_model(next, remaining - 1, alphabet, trace, report)?;
            trace.pop();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn typestate_is_tied_to_the_active_lease() {
        let mut leases = LeaseState::default();
        let guard = leases.acquire(OwnerId::new("agent-a").unwrap()).unwrap();
        let migration = Migration::new(vec![1_u8, 2, 3])
            .validate(|bytes| ValidationProof::for_bytes(bytes, bytes.len()));
        let authorized = migration.authorize(&guard);
        assert_eq!(authorized.owner().as_str(), "agent-a");
        assert_eq!(authorized.epoch(), 1);
        assert_eq!(authorized.proof().checked_items(), 3);
        let applied = authorized.finish();
        assert_eq!(applied.into_inner(), vec![1, 2, 3]);
        let receipt = guard.release();
        assert_eq!(receipt.owner().as_str(), "agent-a");
        assert_eq!(receipt.epoch(), 1);
        assert!(leases.owner().is_none());
    }

    #[test]
    fn dropping_a_guard_releases_the_lease() {
        let mut leases = LeaseState::default();
        {
            let guard = leases.acquire(OwnerId::new("agent-a").unwrap()).unwrap();
            assert_eq!(guard.epoch(), 1);
        }
        assert!(leases.owner().is_none());
        let guard = leases.acquire(OwnerId::new("agent-b").unwrap()).unwrap();
        assert_eq!(guard.epoch(), 2);
    }

    #[test]
    fn model_rejects_apply_without_validation_and_lease() {
        let state = ModelState::initial();
        assert!(state.step(Action::Apply(1)).is_err());
        let validated = state.step(Action::Validate).unwrap();
        assert!(validated.step(Action::Apply(1)).is_err());
    }

    #[test]
    fn model_rejects_the_wrong_owner() {
        let leased = ModelState::initial()
            .step(Action::Validate)
            .unwrap()
            .step(Action::Acquire(1))
            .unwrap();
        assert!(leased.step(Action::Apply(2)).is_err());
        assert!(leased.step(Action::Release(2)).is_err());
    }

    #[test]
    fn bounded_model_has_no_counterexample() {
        let report = bounded_model_check(10, &[1, 2]).unwrap();
        assert!(report.states_checked > 10);
        assert!(report.accepted_transitions > 0);
    }

    proptest! {
        #[test]
        fn arbitrary_action_streams_preserve_invariants(codes in prop::collection::vec(0_u8..=8, 0..256)) {
            let mut state = ModelState::initial();
            for code in codes {
                let action = match code {
                    0 => Action::Validate,
                    1 => Action::Acquire(1),
                    2 => Action::Acquire(2),
                    3 => Action::Release(1),
                    4 => Action::Release(2),
                    5 => Action::Apply(1),
                    6 => Action::Apply(2),
                    7 => Action::Abort(1),
                    _ => Action::Abort(2),
                };
                if let Ok(next) = state.step(action) {
                    state = next;
                }
                prop_assert!(state.check_invariants().is_ok());
            }
        }
    }
}
