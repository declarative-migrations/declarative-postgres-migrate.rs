# Formal methods and Rust ownership contracts

The migration engine uses a layered assurance model instead of treating a successful SQL parse as sufficient evidence.

## Safety properties

The first executable slice establishes these invariants:

1. A migration cannot be authorized before validation.
2. An authorized migration cannot outlive the lease that authorized it.
3. One in-memory lease state has at most one owner because `LeaseGuard` holds its unique mutable borrow.
4. One PostgreSQL advisory-lock session has one Rust owner because `PostgresMigrationLease` owns its `PgConnection` and is not cloneable.
5. Applying SQL requires `&mut PostgresMigrationLease`, so one lease cannot have two concurrent execution borrows.
6. Applying SQL requires `ValidatedScript`, not an untyped string.
7. Applied and aborted states are terminal in the abstract state machine.
8. Every successful release produces an owner, epoch or lock key, execution count, and script fingerprint receipt.

## Verification layers

### Compile-time typestate and borrow checking

`formal::Migration<T, Draft>` must be consumed through `validate` or `try_validate` before `authorize` exists. Authorization borrows a live `LeaseGuard`; releasing the guard while authorization remains live is rejected by the compiler. Compile-fail doctests keep these negative contracts executable in CI.

`lease::PostgresMigrationLease` owns the PostgreSQL connection that owns the session advisory lock. The lock therefore follows Rust ownership. Explicit `release` is preferred because it returns an audit receipt; dropping the value still drops the connection and releases the session lock.

### Bounded model checking

`formal::bounded_model_check` enumerates every enabled transition up to a configured depth for a finite owner set. Every visited state checks:

- lease ownership exists exactly in the leased phase;
- every non-draft state has crossed validation;
- applied or aborted states have crossed at least one lease epoch;
- wrong-owner release, apply, and abort transitions are rejected;
- terminal states expose no outgoing transition.

This is a finite-state proof over the configured bound, not a proof of PostgreSQL itself. The model is intentionally small enough to run on every pull request.

### Property tests

`proptest` feeds arbitrary action streams through the transition system. Invalid actions are rejected; every accepted prefix must preserve the invariants. This complements exhaustive short traces with longer randomized traces.

### PostgreSQL integration

`tests/lease_contract.rs` runs against PostgreSQL when `DPM_TEST_DATABASE_URL` is set. It proves that a second connection cannot acquire the same advisory key, that a validated script executes through the unique lease, and that another owner can acquire the key after explicit release.

## Test-organization policy

The `declarative-migrations-test` organization runs the formal/property suite separately from the product repository. Its workflows checkout an exact upstream ref and run:

- library and doctest contracts;
- the bounded model checker and property tests;
- PostgreSQL advisory-lock collision and recovery tests;
- Clippy with warnings denied.

The test repositories should pin the exact upstream commit after the product pull request is green so scheduled runs remain reproducible.

## Next formalization targets

The next useful models are rollback atomicity under injected statement failure, migration-plan dependency ordering, CockroachDB retry semantics, and schema-convergence equivalence across PostgreSQL versions. Those should reuse the same pattern: a small pure Rust transition system, executable invariants, compile-time capabilities where possible, and database-backed witness tests in the `*-test` organization.
