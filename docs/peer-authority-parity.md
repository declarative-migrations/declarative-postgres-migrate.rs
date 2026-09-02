# Peer-authority contract and ORM parity gate

Status: proposed, fail-closed

## Decision

TypeSpec and JSON Schema/OpenAPI are independent top-level contract authorities.
Neither is generated from, subordinate to, or allowed to silently override the
other.

```text
TypeSpec
  ├── SQL candidate
  ├── Protobuf / gRPC
  └── wire-client types

JSON Schema + OpenAPI
  ├── SQL candidate
  ├── interfaces and client types
  └── write-client contracts

TypeSpec SQL ───────────────┐
                            ├── DPM catalog equality ── discrepancy? PAUSE
JSON Schema/OpenAPI SQL ────┘

TypeSpec generated types ──────────────┐
                                       ├── semantic manifest equality ── discrepancy? PAUSE
JSON Schema/OpenAPI generated types ───┘

SeaORM projection ──────────┐
                            ├── semantic ORM manifest equality ── discrepancy? PAUSE
Diesel projection ──────────┘
```

A successful comparison does not prove that either source is correct in
isolation. It proves that independently implemented paths agree on the reviewed
semantic surface. A failed comparison is evidence that human evaluation is
required; it is not permission to choose one side automatically.

## Required gate

A release is eligible to continue only when all three comparisons pass:

1. TypeSpec-generated SQL and JSON Schema/OpenAPI-generated SQL materialize to
   equal DPM catalogs for the exact database engine/version under certification.
2. TypeSpec and JSON Schema/OpenAPI generators emit equal canonical type
   manifests for the supported client languages and transports.
3. SeaORM and Diesel emit equal canonical ORM projection manifests for the same
   reviewed database catalog.

Missing artifacts, an unavailable generator, invalid manifest structure, a DPM
error, or any semantic difference produces `decision=pause`, exits with status
3, and preserves evidence. No automatic winner, fallback authority, merge, or
migration apply is allowed after a pause decision.

## Why comparison is semantic

Generated SQL and generated source code are not compared byte-for-byte. SQL may
use different statement order or spelling while materializing the same catalog;
language generators may differ in formatting or file layout while describing
the same public types. The gate therefore compares:

- SQL through DPM's normalized catalog model;
- generated client/interface types through
  `declmig.generated-types/v1` manifests;
- ORM projections through `declmig.orm-projection/v1` manifests.

Provenance, generator versions, input digests, and output digests remain in the
full manifests and release evidence, but the `semanticModel` object is the
comparison surface. Object key order is ignored. Array order remains
significant because enum order, tuple position, precedence, and similar list
semantics can be meaningful.

## Invocation

Consumers invoke the certifier through Bash so the repository does not depend
on executable-bit preservation by every package/export mechanism:

```sh
bash scripts/certify-peer-authorities.sh \
  --dpm target/release/dpm \
  --typespec-sql artifacts/typespec/schema.sql \
  --json-schema-sql artifacts/json-schema-openapi/schema.sql \
  --typespec-types artifacts/typespec/generated-types.json \
  --json-schema-types artifacts/json-schema-openapi/generated-types.json \
  --seaorm-projection artifacts/orm/seaorm.json \
  --diesel-projection artifacts/orm/diesel.json \
  --shadow "$SHADOW_DATABASE_URL" \
  --output-dir test-results/peer-authority
```

Exit codes:

- `0`: all comparisons passed; `decision=continue`;
- `3`: discrepancy, missing artifact, or comparison-tool error;
  `decision=pause`;
- `2`: invalid certifier environment or unsafe output location;
- `64`: invalid invocation.

The output directory contains the DPM SQL plan, canonicalized semantic models,
unified diffs, tool error text, and
`peer-authority-certification.json`. Release tooling must consume that JSON
rather than inferring success from log text.

## Generator responsibilities

Each top-level contract path must be independently implemented and pinned.
Shared helpers may define the manifest envelope, but they must not share the
mapping implementation being cross-checked; otherwise correlated bugs could
make both outputs agree incorrectly.

The TypeSpec path owns its own mapping to SQL, Protobuf/gRPC, and wire-client
artifacts. The JSON Schema/OpenAPI path owns its own mapping to SQL,
interfaces/types, and write-client artifacts. Both paths must cover optional
versus nullable fields, integer widths, decimal/money representation,
timestamps, enums, discriminated unions, defaults, uniqueness, keys,
relationships, indexes, checks, and database-specific extensions where those
concepts are declared.

SeaORM and Diesel are peer runtime projections, not migration authorities. Each
adapter independently reads the same exact catalog and emits a semantic
projection manifest. Runtime code may select the ORM appropriate for a service,
but schema certification requires agreement between both projections.

## Pause-and-evaluate protocol

When the gate pauses:

1. preserve all source, generated, normalized, and diff artifacts;
2. record exact tool versions and input/output SHA-256 digests;
3. open or update one discrepancy issue with the affected semantic paths;
4. classify the difference as source disagreement, unsupported mapping,
   generator bug, database-dialect difference, or intentional divergence;
5. resolve the source or generator explicitly;
6. regenerate both sides from clean inputs;
7. rerun all three comparisons;
8. continue only from a new all-pass certificate.

Intentional divergence is represented as a reviewed, versioned exception with a
narrow semantic path, engine/language scope, owner, rationale, and expiry. An
exception changes the expected semantic model on both sides; it must never be a
blanket rule that ignores a diff.

## Current audit result

At adoption time the organization has a JSON Schema contract and generated
Rust/TypeScript/Dart surfaces, but no checked-in TypeSpec authority or
TypeSpec-to-SQL/Protobuf/gRPC pipeline. The ORM template contains SeaORM but no
Diesel projection adapter. The first real fleet certificate must therefore be
`pause` until those missing peer paths are implemented and independently
certified. This is the intended behavior of the policy, not a reason to bypass
it.
