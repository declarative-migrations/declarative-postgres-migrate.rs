# Code-first and database-first workflows with peer authorities

Status: governing migration workflow

## Common invariant

TypeSpec and JSON Schema/OpenAPI are independent top-level contract authorities. Diesel and SeaORM are independent ORM projections. A workflow may begin in any one of these places, or in an existing database, but no initiating representation becomes the automatic winner.

A candidate release reaches migration planning only after:

1. both contract sources are explicitly updated or baselined;
2. both contract lanes generate complete artifacts;
3. both SQL candidates converge to equivalent DPM catalogs;
4. both canonical type/operation manifests agree;
5. Diesel and SeaORM independently project the agreed candidate database;
6. ORM manifests and shared query/effect fixtures agree;
7. every gate returns `proceed`.

Any earlier mismatch produces `pause` and an evaluation record.

## TypeSpec-initiated code-first change

1. Change the TypeSpec source.
2. Generate TypeSpec SQL, Protobuf, gRPC, wire clients, and canonical type/operation manifest.
3. Independently update the JSON Schema/OpenAPI source to express the intended equivalent contract; do not generate it from TypeSpec as the certification input.
4. Generate JSON Schema/OpenAPI SQL, interface/types, validators, write clients, and canonical type/operation manifest.
5. Compare type/operation manifests.
6. Materialize both SQL candidates in exact-version PostgreSQL and CockroachDB shadows and compare DPM catalogs.
7. On any discrepancy, record `pause`; evaluate the source intent and generator mappings without copying TypeSpec output over the peer lane.
8. After contract parity, generate and compare Diesel/SeaORM projections and shared fixtures.
9. Only then generate and verify the DPM migration plan.

TypeSpec initiates this workflow but does not outrank JSON Schema/OpenAPI.

## JSON Schema/OpenAPI-initiated code-first change

This workflow is symmetric:

1. Change JSON Schema Draft 2020-12 and/or OpenAPI 3.1 sources.
2. Generate SQL, client interfaces/types, validators, write clients, and the canonical manifest.
3. Independently update TypeSpec to the intended equivalent contract.
4. Generate TypeSpec SQL, Protobuf, gRPC, wire clients, and manifest.
5. Run the same type, SQL-catalog, ORM, fixture, and migration gates.

JSON Schema/OpenAPI initiates this workflow but does not outrank TypeSpec.

## Database-first change

Use this for brownfield adoption, database-native features, or a reviewed SQL design.

1. Introspect the current database into a versioned DPM catalog and capture native schema-only SQL plus engine/version/extension identity.
2. Create or review the desired database catalog/SQL in a disposable environment.
3. Generate **proposals** for TypeSpec and JSON Schema/OpenAPI from the desired catalog where reverse adapters exist.
4. Human review completes information that database introspection cannot recover, including operation direction, client behavior, validation intent, write-only/read-only semantics, transport, naming, and domain distinctions.
5. Commit both top-level sources explicitly.
6. Regenerate SQL independently from each source.
7. Require both regenerated catalogs to equal the reviewed desired catalog and each other.
8. Compare type/operation manifests, then Diesel/SeaORM projections and fixtures.
9. DPM plans from the observed current catalog to the agreed desired catalog.

The database is authoritative about observed current state and may seed a proposed desired state. It does not silently overwrite either contract authority.

## Brownfield baseline

For an existing database with no peer sources:

1. freeze nonessential DDL;
2. capture DPM catalog, native DDL, engine version, extensions, grants/policies, and checksums;
3. establish a baseline release without generating destructive changes against that database;
4. author/review both TypeSpec and JSON Schema/OpenAPI sources from the baseline;
5. regenerate both SQL candidates and require convergence to the baseline catalog;
6. generate both ORMs and require manifest/fixture parity;
7. record any feature that cannot round-trip as an explicit `pause` until mapped or declared unsupported.

## ORM-first proposal

Diesel schema definitions or SeaORM entity-first synchronization may be used only to create a disposable proposal:

1. materialize the ORM proposal in a scratch database;
2. dump its DPM catalog;
3. review the catalog and security/invariant overlays;
4. update both TypeSpec and JSON Schema/OpenAPI sources explicitly;
5. regenerate and run all peer gates;
6. discard the ORM-authored migration history.

Neither ORM may synchronize shared development, staging, or production as a shortcut around DPM.

## Emergency production SQL

1. require an incident/change identifier and capture the before catalog;
2. apply the smallest separately reviewed emergency change;
3. capture the after catalog and native DDL;
4. create proposed updates for both contract authorities;
5. record `pause` for normal releases until both sources regenerate to the observed after catalog and all type/ORM gates pass;
6. calculate a DPM adoption/residual plan and retain all evidence.

An emergency database change is temporary observed truth, not a permanent silent source-of-design authority.

## Migration planning and apply

DPM is the only component allowed to plan, lease, verify, apply, and receipt production schema migrations. Contract generators and ORMs emit candidates/proposals only.

The reviewed plan binds:

- both top-level source and generator identities;
- both SQL candidates and DPM catalogs;
- both type/operation manifests;
- Protobuf/gRPC/client artifacts;
- Diesel/SeaORM manifests and fixture results;
- current/desired catalogs, execution phases, safety decision, and checksums.

Apply re-introspects after acquiring the execution lease and must stop if the observed source catalog or any reviewed digest changed.

## Evaluation outcomes

A paused discrepancy may be resolved only by one of these recorded decisions:

- correct the TypeSpec source;
- correct the JSON Schema/OpenAPI source;
- correct a generator/adapter;
- correct an ORM projection adapter;
- correct the reviewed database design;
- add a narrowly scoped, versioned equivalence rule that demonstrably preserves semantics;
- declare the feature unsupported for a specific engine/client and keep release blocked for that target.

There is no outcome named "prefer the currently dominant tool."
