# Beyond schema migrations: a plan for data migrations

Status: **design document — nothing here is implemented.** dpm v0.x is schema-only by
deliberate scope. This is the plan for the data layer, written down so the schema layer
keeps growing the right seams.

## Why data migrations are a different problem

A schema migration has a closed-form correctness proof: introspect, diff, converge,
re-diff to empty — dpm's `verify` does exactly this, and seven independent tools can
countersign it. Data migrations have no such proof. Correctness depends on *values*,
volume makes exhaustive checking impractical, and the failure modes that hurt are the
ones a happy-path test never sees: the one row with a NULL where "there can't be
NULLs", the legacy `latin1` bytes inside a `text` column, the timestamp from a
timezone that no longer exists, the FK orphan created two schema-versions ago.

So the design center is different: **statistical sampling for cheap confidence, plus
deliberate outlier hunting for the rows most likely to break** — because a uniform
random sample is precisely the thing that *misses* outliers.

## Where it slots into dpm

```
dpm diff        →  schema plan (exists today)
dpm data plan   →  data-migration plan: per-table transform specs        [future]
dpm data rehearse → sample-based rehearsal on a shadow replica           [future]
dpm data verify →  invariant + reconciliation checks post-migration      [future]
```

Three existing seams were built for this:

1. **The JSON plan format.** `dpm diff --format json` emits a typed change list. A
   data plan is the same idea one level down: a typed list of per-table transforms
   (`copy`, `cast`, `backfill`, `split`, `merge`, `derive`) that AI reviewers and CI
   can inspect, with the same `DPM_VERDICT` review protocol.
2. **The shadow-database machinery.** `verify` already materializes throwaway
   replicas. Data rehearsal is the same move with rows in them.
3. **The cross-check philosophy.** Nothing is trusted on self-report: dpm's own
   convergence check, seven external tools, and an AI discrepancy scan countersign the
   schema layer. The data layer gets the same treatment via reconciliation queries and
   independent recount/rehash checks.

## Sampling design: the ~3% rehearsal

Rehearsing on full production data is slow, expensive, and often forbidden
(PII). Rehearsing on empty tables proves nothing. The middle path is a **statistical
sample, default ~3% of rows per table** (configurable, `--sample-rate`), pulled into
the shadow replica before applying the migration + transforms.

But a uniform 3% is not enough on its own — three strata compose the sample:

| stratum | what | why |
|---|---|---|
| uniform | ~3% Bernoulli sample per table (`TABLESAMPLE BERNOULLI`) | unbiased estimate of aggregate behavior; catches broad-population errors |
| boundary | per column: MIN/MAX rows, longest/shortest strings, oldest/newest timestamps, numeric extremes, first/last by PK | migrations break at extremes far more often than in the middle |
| outlier | rows flagged by the outlier scan below | the whole point |

Small tables (< ~10k rows, threshold configurable) are copied whole — sampling
overhead exceeds the copy cost, and small tables are disproportionately lookup/enum
tables where every row is load-bearing.

Sampling must be **referentially closed**: after drawing the sample, walk FKs and pull
every referenced parent row (transitively), else the rehearsal fails on FK violations
that say nothing about the migration. This closure is computed from the same catalog
model dpm already introspects.

## Outlier identification (the testing payload)

The sampler's job is to find the rows most likely to falsify the migration. Per
column, cheap SQL-side screens; per table, a scored "weird rows" list. Candidate
detectors, roughly in order of value-per-cost:

- **NULL-boundary rows** — NULLs in columns the *desired* schema makes NOT NULL; the
  single most common data-migration failure.
- **Cast-fragility probes** — for every `AlterColumnType` in the schema plan, run the
  USING cast as a `SELECT ... WHERE cast fails` dry probe (wrapped in a LATERAL
  `pg_input_is_valid()` on PG16+, or an exception-trapping PL/pgSQL sampler earlier);
  every failing row goes in the sample.
- **Constraint-violation candidates** — rows violating any CHECK/UNIQUE/FK the desired
  schema *adds* (`NOT VALID` + `VALIDATE` failures found before they happen).
- **Statistical outliers** — numeric columns: |z| > 4 against `pg_stats`-derived
  moments, plus MAD-based screens for heavy-tailed columns; text: length outliers,
  non-UTF8-representable bytes, control characters, RTL/zero-width codepoints; arrays/
  JSONB: cardinality and depth extremes.
- **Encoding/locale hazards** — strings that change under the target collation's
  sort/equality; keys that collide case-insensitively when a `citext`/`lower()` unique
  index is being introduced.
- **Temporal hazards** — timestamps at DST transitions, epoch 0, far-future sentinels
  (9999-12-31), and pre-Gregorian dates that round-trip differently.
- **Enum-adjacent strays** — for text→enum conversions, every distinct value not in
  the enum's label set (with counts).
- **Duplicate-key candidates** — for every UNIQUE being added, the actual duplicate
  groups (`GROUP BY ... HAVING count(*) > 1`), sampled per group.

Each detector emits `(table, pk, reason, severity)`; the rehearsal report ties every
failure back to the reason it was sampled, so a red run reads as "the cast fails on
rows sampled for cast-fragility" rather than a bare stack trace.

## Verification: reconciliation, not vibes

After rehearsal (and after a real run), `dpm data verify` executes reconciliation
checks derived from the plan:

- **Counts**: per-table row counts, per-transform in/out counts (with explicit
  accounting for intentional drops/merges).
- **Checksums**: order-independent aggregate hashes over stable column sets
  (`sum(hashtext(...))`-style) comparing pre/post for untouched columns.
- **Invariants**: user-declared SQL predicates that must hold after migration
  (`SELECT count(*) = 0 FROM ... WHERE <bad>`), stored alongside the plan.
- **Spot re-derivation**: for derived/transformed columns, recompute the transform for
  a fresh sample and compare.
- **AI review**: the plan + rehearsal report + reconciliation results feed the same
  reviewer harness (`--ai-review`), which is well-suited to "this failure list smells
  like an encoding issue, not a cast issue" pattern reads.

## Zero-downtime rollout (why pgroll isn't a cross-checker)

pgroll solves a problem dpm deliberately doesn't: serving old and new application
versions *simultaneously* mid-migration via versioned schema views and backfills. That
is an orchestration contract, not a diff contract — there is no "do these two
databases match" question to ask it, which is why it isn't in the cross-checker set.
The likely integration is the opposite direction: emit dpm's plan (schema + data
transforms) *into* pgroll's operation format for teams that need its expand/contract
rollout machinery. That belongs to this data-migration phase, not the schema differ.

## Non-goals (for this phase too)

- Logical-replication-based cutovers (Debezium/CDC territory).
- Cross-engine migrations (MySQL→PG etc.).
- Automatic PII discovery/masking — sampling into a shadow replica must respect data
  policies; the design assumes an operator-supplied masking hook per table before
  anything leaves production.

## Sequencing

1. `data plan` format + referentially-closed sampler (uniform + boundary strata).
2. Outlier detectors, starting with NULL-boundary, cast-fragility, and
   constraint-violation candidates (they fall directly out of the schema plan).
3. Rehearsal runner on shadow replicas + reconciliation checks.
4. AI review integration (payload builder exists; add the data sections).
5. pgroll emission for zero-downtime consumers.
