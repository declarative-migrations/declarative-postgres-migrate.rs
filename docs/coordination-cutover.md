# Declarative Migrations coordination cutover

Tracking: `declarative-migrations/declmig-lib-core#20` and this repository's issue #16.

## Authority boundary

`ORESoftware/ores-locks-and-leases` owns fleet lock identities, local locks, distributed leases, PostgreSQL advisory locking, fencing-token semantics, provider composition, and the common failure vocabulary. DPM owns migration planning/application/convergence semantics and the migration-specific lock catalog. DPM must not import Fiducia or another distributed lock provider directly.

The current `PostgresMigrationLease` predates that fleet boundary. Its direct `sqlx` advisory lock is therefore a compatibility mechanism, not a second public lock abstraction.

## Safe transition

Do **not** replace `DEFAULT_MIGRATION_LOCK_KEY` with a newly derived shared advisory key in one release. An old DPM process and a new DPM process would then acquire different PostgreSQL locks and could execute concurrently.

The runtime migration must use a bounded dual-lock transition:

1. Resolve the admitted migration identity `declarative-migrations/migrations/apply:{target_id}`. `target_id` is a stable, non-secret target identity; never hash a raw DSN or credential-bearing URL into receipts.
2. Acquire the legacy PostgreSQL advisory key first so mixed old/new fleets still exclude each other.
3. Acquire the `ores-locks-and-leases` plan for the canonical identity. For multi-runner deployments this is the outer distributed/fenced lease plus its PostgreSQL layer; local-only execution may select the admitted local profile.
4. Carry the fencing token through every guarded write/receipt that can outlive the holder. Reject stale or replayed fencing tokens at the durable write boundary.
5. Re-check/renew the maintained lease immediately before any irreversible commit boundary required by the shared package contract.
6. On every error/cancellation path release in reverse order; cleanup failures outrank an otherwise successful result.
7. Retain both locks until exact test-org evidence proves the supported fleet can no longer contain a legacy-only DPM executor. Remove the legacy key in a separately reviewed compatibility change.

## Required qualification

Before promotion, exact `*-test` or disposable database fixtures must demonstrate:

- positive single-runner plan/apply/convergence;
- two-runner contention with no overlapping migration statement execution;
- old-only versus dual-lock process contention during the compatibility window;
- lease expiry/renewal and stale-fencing-token rejection;
- process death before apply, during a statement boundary, and before final convergence;
- lock/lease cleanup after success and failure;
- PostgreSQL restart/network reset classification without duplicate irreversible effects;
- separate CockroachDB qualification using only mechanisms actually supported there.

`ores-locks-and-leases` is the public coordination dependency. Any remaining direct advisory SQL in DPM must be labelled and tested as legacy compatibility until removed.
