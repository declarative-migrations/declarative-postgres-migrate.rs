# DPM repository and package boundaries

The current repository remains the compatibility root while the public
interfaces stabilize. The new modules are deliberately shaped for extraction
without forcing existing CLI, Homebrew, npm, Cargo, Docker, Nix, or Zed
consumers to move immediately.

## Recommended organization layout

### `dpm-interfaces`

Own the versioned request/response DTOs, catalog wire-format compatibility,
JSON Schema/OpenAPI, generated client fixtures, and breaking-change tests. It
must not contain database drivers, database URLs, secrets, filesystem access,
or an apply implementation.

Current extraction source: `src/interfaces.rs` and
`openapi/dpm-server-v1.json`.

### `dpm-cli`

Own the `dpm` executable and local workflows. Keep the core library usable in
process and add an explicit remote-server mode only after the v1 API has
external conformance coverage. Existing package coordinates and command-line
behavior should continue forwarding or remain available through the current
repository during a deprecation window.

Current extraction source: `src/main.rs`, `.cli-flags.toml`, and CLI tests.

### `dpm-sync`

Own typed remote clients, retries for read-only requests, API-version checks,
idempotency support when added to apply, and language-specific generated
clients. It must not accept raw target database credentials.

Current extraction source: `src/sync.rs`.

### `dpm-server`

A separate repository is also recommended once deployment manifests and
service ownership diverge. It should depend on pinned `dpm-interfaces` and core
engine releases, own container/Kubernetes artifacts, and retain a minimal
binary entrypoint.

Current extraction source: `src/server/` and `src/bin/dpm-server.rs`.

## Split gate

Create the repositories after these conditions are green on one exact commit:

1. CLI and library compatibility tests pass unchanged.
2. The checked-in OpenAPI document matches serialized Rust DTOs.
3. A real-process server smoke test covers health, version, authentication,
   request limits, an inline-catalog diff, and apply-disabled behavior.
4. At least one external consumer fixture compiles and exercises `dpm-sync`.
5. Release automation publishes provenance attestations and both Zed binaries.
6. Ownership, versioning, and coordinated release policy are documented in the
   organization `.github` repository.

Until then, module boundaries provide the same architectural separation with a
single atomic release and no cross-repository version skew.
