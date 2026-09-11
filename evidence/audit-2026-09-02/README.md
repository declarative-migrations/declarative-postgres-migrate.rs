# 2026-09-02 SQL parity audit evidence

- `current-sql-parity.canonical.json` is the minimal machine-readable peer-authority `pause` report.
- `current-sql-parity.json` is the expanded human-review envelope with evaluation questions and release effects.

Both record that DPM is not yet receiving independently generated TypeSpec and JSON Schema/OpenAPI SQL/catalog pairs. Neither file is a successful release certificate. The decision may change to `proceed` only after exact-version materialization, catalog comparison, and clean evidence regeneration for both PostgreSQL and CockroachDB support profiles.
