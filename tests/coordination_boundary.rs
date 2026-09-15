#![forbid(unsafe_code)]

use std::fs;

const SHARED_ZED_COORDINATE: &str =
    "\"oresoftware/ores-locks-and-leases\" = \"=0.1.1\"";

#[test]
fn migration_executor_declares_the_shared_coordination_package() {
    let manifest = fs::read_to_string(".zpkg.toml").expect("read .zpkg.toml");
    assert!(
        manifest.contains(SHARED_ZED_COORDINATE),
        "DPM must resolve fleet lock/lease semantics through oresoftware/ores-locks-and-leases"
    );
}

#[test]
fn migration_executor_does_not_import_a_concrete_distributed_lease_provider() {
    for path in ["Cargo.toml", "Cargo.lock", ".zpkg.toml"] {
        let text = fs::read_to_string(path).unwrap_or_default().to_ascii_lowercase();
        for forbidden in [
            "fiducia-client",
            "fiducia_client",
            "fiducia-cloud/fiducia-clients",
        ] {
            assert!(
                !text.contains(forbidden),
                "{path} contains direct provider dependency {forbidden}; depend on ores-locks-and-leases instead"
            );
        }
    }
}

#[test]
fn legacy_postgres_lock_is_explicitly_a_compatibility_lane() {
    let lease_source = fs::read_to_string("src/lease.rs").expect("read src/lease.rs");
    let cutover = fs::read_to_string("docs/coordination-cutover.md")
        .expect("read coordination cutover contract");

    assert!(lease_source.contains("DEFAULT_MIGRATION_LOCK_KEY"));
    assert!(lease_source.contains("pg_try_advisory_lock"));
    assert!(cutover.contains("dual-lock"));
    assert!(cutover.contains("Do **not** replace `DEFAULT_MIGRATION_LOCK_KEY`"));
    assert!(cutover.contains("declarative-migrations/migrations/apply:{target_id}"));
}
