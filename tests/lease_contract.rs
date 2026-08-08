//! PostgreSQL-backed lease integration tests.
//!
//! Gated on DPM_TEST_DATABASE_URL so plain `cargo test` remains offline-safe.

use dpm::lease::{PostgresMigrationLease, ValidatedScript, DEFAULT_MIGRATION_LOCK_KEY};

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
