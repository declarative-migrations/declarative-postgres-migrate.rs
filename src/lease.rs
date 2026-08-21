#![forbid(unsafe_code)]

//! Borrow-checked PostgreSQL advisory leases for migration execution.
//!
//! [`PostgresMigrationLease`] owns the database connection that owns the
//! session advisory lock. The value is deliberately neither `Clone` nor
//! `Copy`; dropping it drops the connection and therefore releases the lock.
//! Prefer [`PostgresMigrationLease::release`] when an auditable receipt is
//! required.
//!
//! Scripts must cross [`ValidatedScript::parse`] before execution. This is a
//! structural boundary: the SQL splitter must produce at least one executable
//! statement, and the immutable statement list is fingerprinted for audit.
//!
//! A raw string cannot be applied:
//!
//! ```compile_fail
//! use dpm::lease::PostgresMigrationLease;
//!
//! async fn invalid(mut lease: PostgresMigrationLease) {
//!     let _ = lease.apply("SELECT 1").await;
//! }
//! ```
//!
//! Two in-flight executions cannot mutably borrow one lease at once:
//!
//! ```compile_fail
//! use dpm::lease::{PostgresMigrationLease, ValidatedScript};
//!
//! async fn invalid(mut lease: PostgresMigrationLease) {
//!     let script = ValidatedScript::parse("SELECT 1").unwrap();
//!     let first = lease.apply(&script);
//!     let second = lease.apply(&script);
//!     let _ = (first.await, second.await);
//! }
//! ```

use anyhow::{bail, Context, Result};
use sqlx::{Connection, PgConnection};

use crate::apply::{split_statements, truncate_sql, ApplyReport};
use crate::formal::stable_fingerprint;

/// Stable organization-wide advisory lock key for the default migration lane.
pub const DEFAULT_MIGRATION_LOCK_KEY: i64 = 0x4450_4d5f_4c4f_434b;

const MAX_LEASE_OWNER_BYTES: usize = 128;

fn normalize_owner(owner: String) -> Result<String> {
    let owner = owner.trim();
    if owner.is_empty() {
        bail!("migration lease owner must not be empty");
    }
    if owner.len() > MAX_LEASE_OWNER_BYTES {
        bail!(
            "migration lease owner exceeds {MAX_LEASE_OWNER_BYTES} UTF-8 bytes"
        );
    }
    if owner.chars().any(char::is_control) {
        bail!("migration lease owner must not contain control characters");
    }
    Ok(owner.to_owned())
}

/// PostgreSQL exposes a one-argument bigint advisory key as two unsigned
/// 32-bit values in `pg_locks`: the high half in `classid`, the low half in
/// `objid`, and `objsubid = 1`. Splitting through `u64` also preserves negative
/// `i64` keys without relying on signed shift or cast behavior in SQL.
fn lock_key_halves(key: i64) -> (i64, i64) {
    let bits = key as u64;
    let mask = u64::from(u32::MAX);
    (((bits >> 32) & mask) as i64, (bits & mask) as i64)
}

async fn ensure_session_lease_held(
    conn: &mut PgConnection,
    key: i64,
    owner: &str,
    boundary: &str,
) -> Result<()> {
    let (classid, objid) = lock_key_halves(key);
    let held: bool = sqlx::query_scalar(
        r#"
        SELECT EXISTS (
            SELECT 1
            FROM pg_catalog.pg_locks
            WHERE locktype = 'advisory'
              AND pid = pg_backend_pid()
              AND granted
              AND objsubid = 1
              AND classid::bigint = $1
              AND objid::bigint = $2
        )
        "#,
    )
    .bind(classid)
    .bind(objid)
    .fetch_one(&mut *conn)
    .await
    .with_context(|| {
        format!(
            "verifying PostgreSQL migration lease {key} for owner {owner} {boundary}"
        )
    })?;

    if !held {
        bail!(
            "PostgreSQL migration lease {key} was lost for owner {owner} {boundary}"
        );
    }
    Ok(())
}

/// Structurally validated, immutable migration script.
pub struct ValidatedScript<'sql> {
    sql: &'sql str,
    statements: Vec<String>,
    fingerprint: u64,
}

impl<'sql> ValidatedScript<'sql> {
    pub fn parse(sql: &'sql str) -> Result<Self> {
        let statements = split_statements(sql);
        if statements.is_empty() {
            bail!("migration script contains no executable statements");
        }
        const LEASE_CONTROL: [&str; 5] = [
            "PG_ADVISORY_LOCK",
            "PG_TRY_ADVISORY_LOCK",
            "PG_ADVISORY_UNLOCK",
            "PG_ADVISORY_UNLOCK_ALL",
            "DISCARD ALL",
        ];
        for statement in &statements {
            let normalized = statement.to_ascii_uppercase();
            if let Some(forbidden) = LEASE_CONTROL
                .iter()
                .find(|token| normalized.contains(**token))
            {
                bail!("migration scripts may not control their execution lease: found {forbidden}");
            }
        }
        let fingerprint = stable_fingerprint(sql.as_bytes());
        Ok(Self {
            sql,
            statements,
            fingerprint,
        })
    }

    pub fn sql(&self) -> &'sql str {
        self.sql
    }

    pub fn statement_count(&self) -> usize {
        self.statements.len()
    }

    pub fn fingerprint(&self) -> u64 {
        self.fingerprint
    }
}

/// Session-scoped PostgreSQL advisory lease.
///
/// The connection and its lock have one Rust owner. The public API exposes
/// only a mutable execution borrow, so statement application is serialized by
/// the borrow checker even before PostgreSQL enforces the advisory lock.
#[must_use = "dropping the lease releases the connection; call release for an audit receipt"]
pub struct PostgresMigrationLease {
    conn: Option<PgConnection>,
    key: i64,
    owner: String,
    executed: usize,
    last_script_fingerprint: Option<u64>,
}

impl PostgresMigrationLease {
    pub async fn acquire(url: &str, key: i64, owner: impl Into<String>) -> Result<Self> {
        let owner = normalize_owner(owner.into())?;

        let mut conn = PgConnection::connect(url)
            .await
            .with_context(|| format!("connecting as migration lease owner {owner}"))?;
        let acquired: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1::bigint)")
            .bind(key)
            .fetch_one(&mut conn)
            .await
            .with_context(|| format!("acquiring PostgreSQL migration lease {key}"))?;
        if !acquired {
            let _ = conn.close().await;
            bail!("PostgreSQL migration lease {key} is already held");
        }

        if let Err(error) =
            ensure_session_lease_held(&mut conn, key, &owner, "immediately after acquisition")
                .await
        {
            let _ = conn.close().await;
            return Err(error);
        }

        Ok(Self {
            conn: Some(conn),
            key,
            owner,
            executed: 0,
            last_script_fingerprint: None,
        })
    }

    pub fn key(&self) -> i64 {
        self.key
    }

    pub fn owner(&self) -> &str {
        &self.owner
    }

    pub fn executed(&self) -> usize {
        self.executed
    }

    pub async fn apply(&mut self, script: &ValidatedScript<'_>) -> Result<ApplyReport> {
        let owner = self.owner.clone();
        let key = self.key;
        let conn = self
            .conn
            .as_mut()
            .context("migration lease has already been released")?;
        let mut executed = 0_usize;

        for (index, statement) in script.statements.iter().enumerate() {
            let before = format!(
                "before statement {}/{}",
                index + 1,
                script.statement_count()
            );
            ensure_session_lease_held(conn, key, &owner, &before).await?;

            if let Err(error) = sqlx::raw_sql(statement).execute(&mut *conn).await {
                let _ = sqlx::raw_sql("ROLLBACK").execute(&mut *conn).await;
                return Err(anyhow::anyhow!(error)).with_context(|| {
                    format!(
                        "leased statement {}/{} failed for owner {}:\n{}",
                        index + 1,
                        script.statement_count(),
                        owner,
                        truncate_sql(statement)
                    )
                });
            }
            executed += 1;

            let after = format!(
                "after statement {}/{}",
                index + 1,
                script.statement_count()
            );
            if let Err(error) = ensure_session_lease_held(conn, key, &owner, &after).await {
                let _ = sqlx::raw_sql("ROLLBACK").execute(&mut *conn).await;
                return Err(error);
            }
        }

        self.executed += executed;
        self.last_script_fingerprint = Some(script.fingerprint());
        Ok(ApplyReport { executed })
    }

    pub async fn release(mut self) -> Result<PostgresLeaseReceipt> {
        let mut conn = self
            .conn
            .take()
            .context("migration lease has already been released")?;
        let unlocked: bool = sqlx::query_scalar("SELECT pg_advisory_unlock($1::bigint)")
            .bind(self.key)
            .fetch_one(&mut conn)
            .await
            .with_context(|| format!("releasing PostgreSQL migration lease {}", self.key))?;
        conn.close()
            .await
            .context("closing PostgreSQL migration lease connection")?;
        if !unlocked {
            bail!(
                "PostgreSQL reported migration lease {} was not held",
                self.key
            );
        }
        Ok(PostgresLeaseReceipt {
            key: self.key,
            owner: self.owner,
            executed: self.executed,
            last_script_fingerprint: self.last_script_fingerprint,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PostgresLeaseReceipt {
    key: i64,
    owner: String,
    executed: usize,
    last_script_fingerprint: Option<u64>,
}

impl PostgresLeaseReceipt {
    pub fn key(&self) -> i64 {
        self.key
    }

    pub fn owner(&self) -> &str {
        &self.owner
    }

    pub fn executed(&self) -> usize {
        self.executed
    }

    pub fn last_script_fingerprint(&self) -> Option<u64> {
        self.last_script_fingerprint
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validated_script_is_non_empty_and_stable() {
        assert!(ValidatedScript::parse("-- comments only").is_err());
        assert!(ValidatedScript::parse("SELECT pg_advisory_unlock_all();").is_err());
        let script = ValidatedScript::parse("SELECT 1; SELECT 2;").unwrap();
        assert_eq!(script.statement_count(), 2);
        assert_eq!(
            script.fingerprint(),
            stable_fingerprint(script.sql().as_bytes())
        );
    }

    #[test]
    fn lease_owner_is_trimmed_bounded_and_control_free() {
        assert_eq!(normalize_owner("  migrator-a  ".to_string()).unwrap(), "migrator-a");
        assert!(normalize_owner(" \t\n ".to_string()).is_err());
        assert!(normalize_owner("migrator\nspoof".to_string()).is_err());
        assert!(normalize_owner("x".repeat(MAX_LEASE_OWNER_BYTES + 1)).is_err());
    }

    #[test]
    fn advisory_key_halves_preserve_all_i64_bits() {
        assert_eq!(
            lock_key_halves(0x1122_3344_5566_7788),
            (0x1122_3344, 0x5566_7788)
        );
        assert_eq!(
            lock_key_halves(-1),
            (i64::from(u32::MAX), i64::from(u32::MAX))
        );
    }
}
