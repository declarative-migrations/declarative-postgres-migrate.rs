# Typed-plan borrow checking

The migration engine has three linked safety layers:

1. `plan_safety` performs alias analysis over typed `Change` values and produces an exact, deterministic plan certificate.
2. `formal` consumes the certified plan into linear `Draft -> Validated -> Applied | Aborted` typestates.
3. `lease` owns the PostgreSQL connection and session advisory lock; execution requires its unique mutable borrow.

This division keeps distinct claims distinct. A conflict-free plan is not authorization to execute, and possession of a database lease is not proof that a plan's internal resource schedule is safe.

## Borrow rules

Each change receives one or more borrows over a hierarchical resource path:

- shared/shared overlaps are compatible;
- exclusive/shared and exclusive/exclusive overlaps conflict;
- exact borrows affect one resource;
- subtree borrows cover all descendants;
- direct table, column, index-create, constraint, policy, RLS, and trigger mutations borrow the table subtree exclusively;
- schema creation and removal borrow the schema subtree exclusively;
- extension changes borrow the database subtree exclusively because extensions may install cross-schema objects;
- enum, sequence, view, and routine changes borrow their exact object;
- a dropped index conservatively borrows its schema subtree because the current `Change` shape does not retain its parent table.

Definitions and expressions may reference objects outside the object being mutated. Until those references are represented explicitly in the typed plan, views, routines, constraints, expression indexes, defaults, generated columns, policies, triggers, and related changes also receive a shared database-subtree borrow. That conservative dependency barrier prevents the checker from certifying concurrency based on guessed SQL dependencies.

The `Change` matches are exhaustive. Adding a new change variant fails compilation until maintainers assign both its direct resource borrow and its opaque-dependency policy.

## Plan certificates

`Plan::borrow_check()` produces a `PlanCertificate` containing:

- a versioned model identifier;
- a stable identity fingerprint of serialized typed changes;
- destructive and manual step indexes;
- ordered execution waves;
- each step's explicit resource borrows.

`PlanCertificate::validate()` checks certificate structure, wave numbering, step ordering, and pairwise borrow compatibility. `validate_for(&plan)` recomputes the entire certificate and rejects reuse or tampering across plans.

`reviewed_plan_checksum(plan, sql)` folds the certificate fingerprint together with the emitted SQL into a hex identity checksum. `dpm diff` and `dpm apply` print it. `dpm apply --require-plan-checksum <hex>` refuses before confirmation if the reviewed plan does not match the pinned digest. After the PostgreSQL lease is held, apply recomputes the checksum from a fresh plan and refuses writes on drift.

The scheduler only groups adjacent compatible changes. It never moves one change ahead of another, so the dependency ordering established by the diff and emitter remains authoritative. The current SQL executor is still sequential; waves are a checked contract for orchestration layers rather than a silent behavior change.

## Linear capability bridge

`Plan::certify()` consumes a raw plan and returns a non-cloneable `CertifiedPlan` with private fields. `CertifiedPlan::into_validated_migration()` consumes that capability into the execution-level `formal::Migration<CertifiedPlan, Validated>` typestate. Lease authorization then borrows the active owner guard, and applied/aborted states consume the authorized migration.

This creates an explicit chain:

```text
typed Plan
  -> exact borrow certificate
  -> linear CertifiedPlan
  -> Validated typestate
  -> borrowed lease owner
  -> Applied | Aborted
```

The fingerprint is an identity checksum, not a cryptographic signature. Cross-process trust still requires authenticated transport, immutable commit pins, and normal artifact-signing controls.

## Verification

The repository CI checks:

- compile-fail doctests for invalid typestate transitions;
- exhaustive bounded migration-state exploration from the execution formal model;
- unit tests for resource ancestry and borrow compatibility;
- randomized properties for conflict symmetry and generated-plan certificate validity;
- exact-plan certificate mismatch rejection;
- PostgreSQL advisory-lock exclusion, release, and recovery tests.

Independent repositories in `declarative-migrations-test` pin the exact product commit and repeat the formal/property suite under larger case budgets and real PostgreSQL/CockroachDB contention.
