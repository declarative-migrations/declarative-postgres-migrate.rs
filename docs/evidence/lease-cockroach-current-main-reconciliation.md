# Lease and CockroachDB hardening reconciliation evidence

## Status

Candidate implementation on pull request #53. Production migration execution
and release promotion remain blocked until every permanent exact-head workflow
passes on the reconciled branch and independent review is complete.

## Source provenance

The implementation preserves the unique intent from two stale, stacked branches
without merging their obsolete trees over current `main`:

- PostgreSQL lease invariant: pull request #39, source head
  `732a74d17ccf7fe5024dc803d35543e556df1f8b`;
- CockroachDB non-public enum qualification: pull request #47, source head
  `d2d1074bd9e8f10ddae98389aed328acf647bfa9`.

Both branches were based before the current peer-authority, typed-plan,
extension-schema, formal-contract, and packaging changes. Pull request #53 was
therefore created from `main@3322ceaff12773a7ad74f7e087bc1abc7c944f8d`.

## Semantic reconciliation

An exact-head, assertion-guarded workflow applied and reread only these changes:

- `src/lease.rs`: normalize the audit owner; reconstruct all advisory-key bits;
  prove the same session owns the exact lock after acquisition and before and
  after every statement;
- `tests/lease_contract.rs`: dynamically assemble and call advisory unlock,
  then prove the subsequent statement is not executed;
- `src/introspect.rs`: render enum column types from their actual type namespace
  and name rather than depending on search path;
- `tests/cockroach.rs`: materialize and verify a non-public `app.order_status`
  enum evolution through the full CockroachDB convergence path.

The workflow formatted the reconciled tree, ran offline unit/integration
compilation, strict Clippy, release build, and conflict-marker checks, then
removed itself and committed the four reviewed source/test files as
`995f03c1f874fbebf3b8d4f4c568309cf434a3d2`.

## Required current-head evidence

The original source-branch and external test-organization runs remain useful
historical provenance, but they do not certify this current-main reconciliation.
Promotion requires fresh results for:

- Rust formatting and broad CI;
- typed migration-plan safety;
- Rust formal contracts;
- PostgreSQL 16 and 17 live convergence;
- CockroachDB live convergence, including the non-public enum regression;
- Nix/package contract and external-schema certification;
- independent black-box test-organization evidence pinned to the final merged
  source commit where required by the release process.

PostgreSQL advisory-lock ownership is not represented as a CockroachDB
distributed fencing guarantee. The two engines remain separate evidence lanes.

## Safety boundary

This reconciliation does not apply a production migration, change a live
catalog, publish a package, create a release, alter provider or DNS state, or
use a credential supplied through chat.
