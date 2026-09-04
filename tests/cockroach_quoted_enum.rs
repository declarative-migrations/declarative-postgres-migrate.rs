//! Live CockroachDB regression for schema-qualified enum types whose
//! identifiers require quoting.

use dpm::introspect::{introspect_url, IntrospectOptions};
use dpm::source::ShadowDb;
use dpm::verify::{verify, VerifyParams};

fn admin_url() -> Option<String> {
    match std::env::var("DPM_TEST_COCKROACH_DATABASE_URL") {
        Ok(value) if !value.is_empty() => Some(value),
        _ => {
            eprintln!(
                "skipping: DPM_TEST_COCKROACH_DATABASE_URL not set \
                 (run scripts/test-cockroach.sh)"
            );
            None
        }
    }
}

async fn fresh_db(admin: &str) -> ShadowDb {
    ShadowDb::create(admin, false)
        .await
        .expect("create CockroachDB quoted-enum test database")
}

#[tokio::test]
async fn quoted_nonpublic_enum_type_is_introspected_and_converges() {
    let Some(admin) = admin_url() else {
        return;
    };
    let source_db = fresh_db(&admin).await;
    let target_db = fresh_db(&admin).await;

    source_db
        .apply_sql(
            r#"
            CREATE SCHEMA "App-Schema";
            CREATE TYPE "App-Schema"."Order Status"
              AS ENUM ('pending', 'paid', 'fulfilled');
            CREATE TABLE "App-Schema"."Order" (
              "id" INT PRIMARY KEY,
              "status" "App-Schema"."Order Status" NOT NULL DEFAULT 'pending'
            );
            "#,
        )
        .await
        .expect("populate desired quoted-enum schema");
    target_db
        .apply_sql(
            r#"
            CREATE SCHEMA "App-Schema";
            CREATE TYPE "App-Schema"."Order Status"
              AS ENUM ('pending', 'fulfilled');
            CREATE TABLE "App-Schema"."Order" (
              "id" INT PRIMARY KEY,
              "status" "App-Schema"."Order Status" NOT NULL DEFAULT 'pending'
            );
            "#,
        )
        .await
        .expect("populate current quoted-enum schema");

    let options = IntrospectOptions::default();
    let source = introspect_url(&source_db.url, &options)
        .await
        .expect("introspect desired quoted-enum schema");
    let target = introspect_url(&target_db.url, &options)
        .await
        .expect("introspect current quoted-enum schema");

    let status_type = &target
        .tables
        .iter()
        .find(|(name, _)| name.schema == "App-Schema" && name.name == "Order")
        .expect("quoted table must be introspected")
        .1
        .column("status")
        .expect("quoted enum column must be introspected")
        .type_sql;
    assert_eq!(status_type, r#""App-Schema"."Order Status""#);

    let outcome = verify(VerifyParams {
        source: &source,
        target: &target,
        shadow_server_url: &admin,
        source_url_for_external: None,
        allow_destructive: false,
        external_check: None,
        checks: Default::default(),
        bins: Default::default(),
        keep_shadow: false,
        verbose: false,
        introspect: &options,
    })
    .await
    .expect("quoted CockroachDB enum migration must materialize");

    assert!(
        outcome.converged,
        "quoted CockroachDB enum migration must converge:\n{:?}\n{}",
        outcome.residual_sql, outcome.migration_sql
    );

    source_db.drop_db().await;
    target_db.drop_db().await;
}
