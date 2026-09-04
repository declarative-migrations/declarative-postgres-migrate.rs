//! PostgreSQL-backed lease integration tests.
//!
//! Gated on DPM_TEST_DATABASE_URL so plain `cargo test` remains offline-safe.

use dpm::lease::{PostgresMigrationLease, ValidatedScript, DEFAULT_MIGRATION_LOCK_KEY};
use sqlx::{Connection, PgConnection};

fn database_url() -> Option<String> {
    match std::env::var("DPM_TEST_DATABASE_URL") {
        Ok(value) if !value.is_empty() => Some(value),
        _ => {
            eprintln!("skipping: DPM_TEST_DATABASE_URL not set");
            None
        }
    }
}

fn test_lock_key() -> i64 {
    DEFAULT_MIGRATION_LOCK_KEY ^ i64::from(std::process::id())
}

fn negative_test_lock_key() -> i64 {
    (test_lock_key() & i64::MAX) | i64::MIN
}

#[tokio::test]
async fn advisory_lease_has_one_owner_and_can_be_reacquired() {
    let Some(url) = database_url() else {
        return;
    };
    let key = test_lock_key();
    let script = ValidatedScript::parse(
        "CREATE TEMP TABLE dpm_lease_probe (id bigint);\n\
         INSERT INTO dpm_lease_probe VALUES (1);\n\
         DROP TABLE dpm_lease_probe;",
    )
    .unwrap();

    let mut first = PostgresMigrationLease::acquire(&url, key, "owner-a")
        .await
        .unwrap();
    let collision = PostgresMigrationLease::acquire(&url, key, "owner-b").await;
    assert!(collision.is_err(), "a second owner acquired the same lease");

    let report = first.apply(&script).await.unwrap();
    assert_eq!(report.executed, 3);
    let receipt = first.release().await.unwrap();
    assert_eq!(receipt.owner(), "owner-a");
    assert_eq!(receipt.executed(), 3);
    assert_eq!(
        receipt.last_script_fingerprint(),
        Some(script.fingerprint())
    );

    let second = PostgresMigrationLease::acquire(&url, key, "owner-b")
        .await
        .unwrap();
    let receipt = second.release().await.unwrap();
    assert_eq!(receipt.owner(), "owner-b");
}

#[tokio::test]
async fn negative_advisory_key_preserves_ownership_and_normalizes_owner() {
    let Some(url) = database_url() else {
        return;
    };
    let key = negative_test_lock_key();
    assert!(key.is_negative());

    let first = PostgresMigrationLease::acquire(&url, key, "  negative-owner  ")
        .await
        .expect("negative bigint advisory key must be observable in pg_locks");
    let collision = PostgresMigrationLease::acquire(&url, key, "collision-owner").await;
    assert!(
        collision.is_err(),
        "a second session acquired the same negative advisory key"
    );

    let receipt = first.release().await.unwrap();
    assert_eq!(receipt.owner(), "negative-owner");
    assert_eq!(receipt.executed(), 0);
    assert_eq!(receipt.last_script_fingerprint(), None);

    let second = PostgresMigrationLease::acquire(&url, key, "reacquired-negative-owner")
        .await
        .expect("released negative advisory key must be reacquirable");
    let receipt = second.release().await.unwrap();
    assert_eq!(receipt.owner(), "reacquired-negative-owner");
}

#[tokio::test]
async fn script_cannot_continue_after_dynamically_releasing_lease() {
    let Some(url) = database_url() else {
        return;
    };
    let key = test_lock_key() ^ 0x5a5a;
    let table = format!("dpm_lease_guard_{}", std::process::id());
    let qualified_table = format!("public.{table}");

    let mut cleanup = PgConnection::connect(&url).await.unwrap();
    sqlx::raw_sql(&format!("DROP TABLE IF EXISTS {qualified_table}"))
        .execute(&mut cleanup)
        .await
        .unwrap();
    cleanup.close().await.unwrap();

    // The lease-control name is assembled at runtime, proving that the
    // statement-boundary invariant is authoritative even when a textual guard
    // cannot recognize dynamic SQL.
    let sql = format!(
        r#"
        DO $dpm$
        BEGIN
          EXECUTE 'SELECT ' || 'pg_' || 'advisory_unlock({key})';
        END
        $dpm$;
        CREATE TABLE {qualified_table} (id bigint);
        "#
    );
    assert!(!sql.to_ascii_uppercase().contains("PG_ADVISORY_UNLOCK"));
    let script = ValidatedScript::parse(&sql).unwrap();

    let mut lease = PostgresMigrationLease::acquire(&url, key, "dynamic-unlock-probe")
        .await
        .unwrap();
    let error = lease.apply(&script).await.unwrap_err();
    assert!(
        format!("{error:#}").contains("was lost"),
        "unexpected error: {error:#}"
    );
    drop(lease);

    let mut verifier = PgConnection::connect(&url).await.unwrap();
    let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
        .bind(&qualified_table)
        .fetch_one(&mut verifier)
        .await
        .unwrap();
    assert!(
        !exists,
        "a statement after lease loss was executed: {qualified_table}"
    );
    sqlx::raw_sql(&format!("DROP TABLE IF EXISTS {qualified_table}"))
        .execute(&mut verifier)
        .await
        .unwrap();
    verifier.close().await.unwrap();
}
