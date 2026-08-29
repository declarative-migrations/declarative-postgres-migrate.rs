# Certifying an externally owned schema

`dpm` is ORM-agnostic: a product's `*-orm-core` repository may own the authored
SQL and generated ORM adapters while this repository owns catalog
introspection, diffing, safety classification, and convergence verification.
The schema does not need to be copied into DPM or a shared Kubernetes repository.

[`scripts/certify-external-schema.sh`](../scripts/certify-external-schema.sh)
is a bounded, read-only planning adapter for CI systems that have already
materialized an external SQL package at an immutable revision:

```sh
bash scripts/certify-external-schema.sh \
  --dpm zed_modules/.bin/dpm \
  --source-sql zed_modules/example/example-schema/registry.sql \
  --target "$READ_ONLY_DATABASE_URL" \
  --shadow "$SHADOW_DATABASE_URL" \
  --output-dir test-results/external-schema
```

The harness:

1. snapshots the live target with `dpm dump`; this is its only connection to
   the live target and performs catalog reads only;
2. plans `source SQL -> target catalog` as machine-readable JSON;
3. replays the target snapshot and migration only on throwaway shadow
   databases to prove convergence;
4. records SHA-256 identities for the source SQL, target catalog, DPM binary,
   plan, and verification log;
5. records DPM's reviewed-plan checksum and an evidence digest without storing
   a database URL or credential.

The report deliberately sets `decisionEligible` to `false`, `reviewRequired`
to `true`, and `applied` to `false`. Certification proves that the reviewed
plan converges from the captured target state. It is not permission to mutate
production.

## Zed package boundary

The calling repository, not DPM, owns dependency resolution. A consumer pins
both packages in `.zpkg.toml` / `.zpkg.lock`, runs `zed install --frozen`, and
passes ordinary materialized paths into the harness:

```toml
[dependencies]
"example/example-schema" = "=0.1.0"
"declarative-migrations/declarative-postgres-migrate" = "=0.3.2"
```

Before accepting the DPM report, the caller must prove that
`sourceSqlSha256` matches the SQL file inside the exact Zed artifact recorded
by the lockfile. It must retain the package coordinate, exact published
version, immutable artifact digest, VCS tag, and source commit beside the DPM
evidence.

DPM intentionally does not parse `.zpkg.toml`, `.zpkg.lock`, registry tokens,
or repository coordinates. Zed intentionally does not receive a database URL.
The consumer workflow is the small composition root between the two tools.

## Apply is a separate capability

After human review and environment approval, a privileged deployment job
re-materializes the same locked schema and DPM artifacts, verifies their
digests, and invokes:

```sh
zed_modules/.bin/dpm apply \
  --source-sql zed_modules/example/example-schema/registry.sql \
  --target "$WRITE_DATABASE_URL" \
  --shadow "$SHADOW_DATABASE_URL" \
  --require-plan-checksum "$REVIEWED_PLAN_CHECKSUM" \
  --yes
```

`dpm apply` re-introspects the current target and refuses writes if the source
or target has drifted from the reviewed checksum. A plan generated with
`--allow-destructive-sql` additionally requires both
`--allow-destructive-sql` and `--allow-destructive-ops` at apply time; these
are separate operator consents. Certification fails closed on destructive
drift unless SQL-generation consent was explicitly supplied to the harness.

Product web/API servers should receive only a read-only database role and the
opaque ORM package. Migration authority belongs to the deployment/admin plane,
not to request-serving processes.

## Responsibility split

The caller owns:

- Zed authentication, frozen dependency resolution, and artifact provenance;
- exact package version, digest, tag, and source commit evidence;
- target/shadow database lifecycle and least-privilege credentials;
- review, approval, retention, and the later apply capability.

The harness owns:

- regular-file, symlink, output-overwrite, and 16 MiB source bounds;
- target/shadow URL-scheme and distinctness checks;
- a stable target-catalog snapshot;
- exact `diff` and `verify` invocations;
- bounded, non-secret evidence shape.

It accepts no repository URL, branch, registry token, arbitrary shell
fragment, or caller-supplied migration command.

## ORESoftware ownership migration

Product SQL should be published from each organization's `*-orm-core`
repository as a small dependency-free `*-schema` Zed package. SeaORM entities
and the additional ORM shadow belong beside that SQL, but generated artifacts
remain projections: authored DDL is authoritative for RLS policies, triggers,
procedures, grants, and application guard SQLSTATEs.

`github.com/oresoftware/k8s-libs-and-shared-defs` should retain only genuinely
cross-product platform SQL, compatibility tombstones, and an ownership index.
It should not remain an alternate copy of product schemas.

The fixture in this repository is representative test data. It proves the
generic DPM boundary against PostgreSQL 17 without copying a product schema or
depending on Zed registry availability.
