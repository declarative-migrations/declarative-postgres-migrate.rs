# Certifying an externally owned schema

`dpm` is ORM-agnostic: a service or shared-library repository may own the SQL
while this repository owns the diff/apply/verify implementation. The schema does
not need to be copied here.

[`scripts/certify-external-schema.sh`](../scripts/certify-external-schema.sh)
provides a bounded adapter for CI systems that have already authenticated and
materialized an external SQL file at an immutable revision.

```sh
bash scripts/certify-external-schema.sh \
  --dpm target/release/dpm \
  --source-sql /verified/checkout/schema.sql \
  --target "$DATABASE_URL" \
  --shadow "$SHADOW_DATABASE_URL" \
  --output-dir test-results/external-schema
```

The harness performs:

1. `dpm apply --source-sql ... --target ... --shadow ... --yes`;
2. `dpm verify --source-sql ... --target ... --shadow ...`;
3. SHA-256 recording for the SQL, DPM binary, apply log, and verify log;
4. a non-decision-eligible JSON report containing no database URL or credential.

## Boundary of responsibility

The caller owns:

- repository authentication;
- exact repository and commit identity;
- the SQL file's provenance;
- target/shadow database lifecycle;
- retention and interpretation of the evidence.

The harness owns:

- regular-file and size checks for the supplied SQL;
- target/shadow URL-scheme and distinctness checks;
- exact `apply` and `verify` invocation;
- bounded, non-secret evidence shape.

It intentionally does not accept a repository URL, branch, token, arbitrary
shell fragment, or migration command. Private GitHub App/deploy-key handling
remains in the consuming organization's trusted workflow.

## ORESoftware shared-schema use

ORESoftware keeps canonical PostgreSQL SQL and generated SeaORM adapters in its
private `*k8s*` shared definitions repository. Its central k8s workflow resolves
an immutable consumer PR head and shared-schema commit through a read-only,
repository-scoped GitHub App token, then calls this harness with the resulting
SQL path. No PAT or private schema copy is required in the consumer repository
or in this repository.

The external fixture in this repository tests the same job/event schema shape
without claiming to be the private ORESoftware schema. It proves the generic
certification mechanism against PostgreSQL 17.
