# SQL catalog parity protocol

Status: proposed, fail-closed; companion to `peer-authority-parity.md`

This protocol turns the paused decision in declarative-migrations#52 into a
reviewable input contract. It does not choose a winner when the two generators
disagree and it never authorizes an apply.

## Candidate envelope

Each lane must publish one machine-readable envelope beside its SQL:

```json
{
  "format": "sql-candidate/v1",
  "authority": "typespec|json-schema-openapi",
  "engine": "postgres|cockroach",
  "engineVersion": "17.2",
  "generatorRevision": "<immutable commit or package digest>",
  "sourceDigest": "<sha256>",
  "sqlDigest": "<sha256>",
  "sqlPath": "artifacts/<authority>/schema.sql"
}
```

The envelope is evidence, not an authority. `authority`, `engine`, and
`engineVersion` must be compared before either SQL file is materialized. The
path must resolve to a regular file inside the isolated artifact directory;
URLs, symlinks, repository coordinates, and database credentials are not
candidate inputs.

## Required certification matrix

The first exit-criteria run must contain one row for every supported engine and
version, with both authorities present:

| engine | version | TypeSpec SQL | JSON Schema/OpenAPI SQL | shadow profile | decision |
| --- | --- | --- | --- | --- | --- |
| PostgreSQL | pinned version | digest + materialization result | digest + materialization result | isolated PostgreSQL | continue/pause |
| CockroachDB | pinned version | digest + materialization result | digest + materialization result | isolated CockroachDB | continue/pause |

An unsupported feature is a typed `unsupported` result for that engine/version,
not an omitted row. The row remains `pause` until the feature is either mapped
identically or recorded as a narrow, reviewed exception with an owner and
expiry.

## Normalization and comparison

Materialization is performed in throwaway shadow databases created for one row.
The catalog comparison normalizes only representation differences that do not
change the product contract:

- identifier quoting and catalog schema names;
- statement/order differences;
- equivalent default-expression formatting after parsing;
- engine-owned generated names when the referenced semantic object is equal.

The comparison must not normalize away column types, nullability, defaults,
generated expressions, identity behavior, keys, unique constraints, foreign
keys, indexes, checks, enum members/order, RLS policies, triggers, procedures,
grants, or engine-specific capabilities. Every ignored path belongs in the
report with the rule version that produced it.

## Evidence identity

The report binds the complete input closure, not just the SQL files:

```json
{
  "format": "authority-parity-report/v1",
  "decision": "continue|pause",
  "rows": [
    {
      "engine": "postgres",
      "engineVersion": "17.2",
      "typespec": { "candidateDigest": "<sha256>", "catalogDigest": "<sha256>" },
      "jsonSchemaOpenApi": { "candidateDigest": "<sha256>", "catalogDigest": "<sha256>" },
      "catalogEqual": true,
      "discrepancies": []
    }
  ],
  "normalizerRevision": "<immutable revision>",
  "evidenceDigest": "<sha256 over canonical report and inputs>"
}
```

`decision=continue` is valid only when every supported row has two successful
materializations, equal normalized catalogs, no unresolved discrepancy, and a
fresh evidence digest. Missing candidates, a failed materialization, a DPM
error, or an incomplete matrix produces `decision=pause` and preserves the
artifacts for evaluation.

## Evaluation checklist for issue #52

- [ ] Pin the supported PostgreSQL and CockroachDB engine versions.
- [ ] Define the two independent SQL-generation invocations and their source
      input manifests.
- [ ] Define the unsupported-feature result shape and reviewed-exception
      fields.
- [ ] Version the catalog normalizer and list every allowed equivalence.
- [ ] Require a distinct throwaway shadow database for each matrix row.
- [ ] Bind candidate, materialized catalog, normalizer, DPM, and report digests.
- [ ] Attach the full matrix and discrepancy paths to one release evidence
      record before changing the decision from `pause`.

Until every box is checked by a clean pinned run, the issue remains correctly
paused.
