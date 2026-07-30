# Security policy

## Supported versions

Security fixes are made on the latest published `0.3.x` patch release. Earlier
minor lines and unreleased Git revisions are not supported security branches.
Users should upgrade to the newest release before reporting a problem that may
already have been fixed.

| Version | Supported |
| --- | --- |
| Latest `0.3.x` patch | Yes |
| `< 0.3` | No |

## Reporting a vulnerability

Please report vulnerabilities privately through
[GitHub Security Advisories](https://github.com/declarative-migrations/declarative-postgres-migrate.rs/security/advisories/new).
Do not open a public issue until a maintainer confirms that disclosure is safe.

Include the affected `dpm` version, database engine and version, the command
path involved (`diff`, `apply`, `verify`, and so on), and the smallest safe
reproduction you can provide. Never include database passwords, connection
URLs containing credentials, production schema contents, customer data, API
keys, or other secrets. Use synthetic identifiers and a disposable database
when demonstrating migration behavior.

Migration safety reports are especially useful when they identify SQL that
could bypass destructive-operation consent, affect objects outside the selected
schemas, diverge after replay, or behave differently between PostgreSQL and
CockroachDB.

Maintainers will acknowledge a report, investigate it privately, and coordinate
the fix and disclosure with the reporter. Please allow time for a patched crate,
CLI archives, checksums, and Homebrew formula to be published consistently.
