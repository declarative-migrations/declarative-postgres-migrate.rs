# dpm-server

`dpm-server` exposes the declarative migration engine through a small,
versioned HTTP API. The CLI remains supported and unchanged; the server is an
additional deployment surface for CI systems, internal platforms, and
polyglot consumers.

## Trust boundary

Remote callers never submit a PostgreSQL URL. Operators configure an alias map
inside the server process:

```sh
export DPM_SERVER_BIND='127.0.0.1:8080'
export DPM_SERVER_TOKEN='replace-with-at-least-24-random-bytes'
export DPM_SERVER_DATABASES_JSON='{
  "development": "postgres://dpm@db.internal/app_dev",
  "production": "postgres://dpm@db.internal/app_prod"
}'

dpm-server
```

Keep `DPM_SERVER_DATABASES_JSON` in a secret manager or injected environment,
not in source control. Error payloads, access logs, API responses, and OpenAPI
do not contain configured URLs.

The default bind is `127.0.0.1:8080`. A non-loopback bind is rejected unless
`DPM_SERVER_TOKEN` is configured. Put TLS and identity-aware authorization at
the ingress or service-mesh layer when the process is reachable over a
network.

## Configuration

| Variable | Default | Purpose |
| --- | --- | --- |
| `DPM_SERVER_BIND` | `127.0.0.1:8080` | IP socket address. Hostnames are intentionally rejected. |
| `DPM_SERVER_TOKEN` | unset | Bearer token for `/v1/diff` and `/v1/apply`; mandatory off loopback. |
| `DPM_SERVER_DATABASES_JSON` | `{}` | JSON object mapping strict aliases to PostgreSQL URLs. |
| `DPM_SERVER_ALLOW_APPLY` | `false` | Enables live mutation. Preview remains available when false. |
| `DPM_SERVER_MAX_BODY_BYTES` | `1048576` | Request-body bound; hard maximum is 16 MiB. |
| `DPM_SERVER_MAX_IN_FLIGHT` | `64` | Maximum concurrently serviced connections. |

Database aliases contain 1–64 ASCII letters, digits, `.`, `_`, or `-`. At most
256 aliases are accepted. Only `postgres://` and `postgresql://` values are
accepted.

## Endpoints

- `GET /healthz` — process liveness.
- `GET /readyz` — validated configuration summary without secret values.
- `GET /v1/version` — service, API, and catalog-format versions.
- `GET /openapi.json` — checked-in OpenAPI 3.1 contract.
- `POST /v1/diff` — typed plan plus reviewable SQL.
- `POST /v1/apply` — preview by default; optional live apply and verification.

Each response has `Cache-Control: no-store`,
`X-Content-Type-Options: nosniff`, and `X-Request-Id`. The server processes one
request per HTTP/1.1 connection, rejects request pipelining, conflicting
content lengths, unsupported transfer encodings, oversized headers, and
oversized bodies.

Accepted connections run as owned local tasks. Each task moves its socket,
concurrency permit, and cloned server state across the task boundary, so the
SQLx request path may retain non-`Send` database state across awaits without
making connection handling serial. `DPM_SERVER_MAX_IN_FLIGHT` remains a hard
bound on simultaneously serviced connections. `SIGINT` stops the accept loop
and closes the local task set cleanly.

## Diff example

A source or target is either an inline catalog or an operator-defined database
alias. This example compares two aliases without exposing their URLs:

```sh
curl --fail-with-body \
  -H 'Authorization: Bearer replace-me' \
  -H 'Content-Type: application/json' \
  --data '{
    "source": {"kind": "database", "name": "development"},
    "target": {"kind": "database", "name": "production"},
    "allow_destructive": false
  }' \
  http://127.0.0.1:8080/v1/diff
```

The response contains the core plan as JSON, emitted SQL, dialect, source and
target descriptions, and counts for all, destructive, gated, and manual
changes. PostgreSQL and CockroachDB catalogs cannot be mixed.

## Apply safety model

`POST /v1/apply` defaults to `"dry_run": true`. A live apply requires all of
the following:

1. `DPM_SERVER_ALLOW_APPLY=true` on the server.
2. `"dry_run": false` in the request.
3. A target alias configured by the operator.
4. `"confirmation": "apply:<alias>"` for a non-destructive plan.
5. `"allow_destructive": true` and
   `"confirmation": "apply-destructive:<alias>"` for a destructive plan.
6. No manual-only plan steps.

Live applies are serialized inside the process. PostgreSQL applies also acquire
the same session-scoped advisory lease as the current `dpm apply` path. While
holding that lease, the server re-introspects and re-renders the plan, rejects
gated or manual work, validates the generated script structurally, executes it
through the lease-owned connection, verifies convergence, and records an audit
receipt when the lease is released. CockroachDB retains the in-process lock and
post-apply convergence check because it does not expose PostgreSQL advisory
locks.

```sh
curl --fail-with-body \
  -H 'Authorization: Bearer replace-me' \
  -H 'Content-Type: application/json' \
  --data '{
    "source": {"kind": "database", "name": "development"},
    "target": "production",
    "dry_run": false,
    "allow_destructive": false,
    "confirmation": "apply:production"
  }' \
  http://127.0.0.1:8080/v1/apply
```

Cooperating PostgreSQL CLI and server processes use one organization-wide
advisory-lock key, so they cannot apply concurrently to the same database.
Non-DPM writers can still race with a migration and are detected by the fresh
plan and convergence checks. For CockroachDB, run one active apply replica per
independently mutable alias set or enforce leader election. Read-only diff
replicas can scale separately for either engine.

## Consumer compatibility

Consumers should call `GET /v1/version` during startup and require
`api_version == "v1"`. The catalog format is separately versioned through
`catalog_format_version`. Unknown request fields are rejected so misspelled
safety options cannot silently fall back to defaults.

Rust consumers can use `dpm::sync::DpmClient`. Other languages should generate
clients from `openapi/dpm-server-v1.json` and preserve the error envelope's
`request_id` in logs and support tickets.
