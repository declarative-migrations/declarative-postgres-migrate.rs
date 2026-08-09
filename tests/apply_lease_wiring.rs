//! Source-level regression guards for the `dpm apply` lease trust boundary.
//!
//! These tests intentionally assert ordering in the CLI wiring. The lease
//! implementation has behavioral tests of its own; this suite prevents a
//! future refactor from accidentally bypassing the lease or moving execution
//! ahead of confirmation and lease-protected revalidation.

const MAIN: &str = include_str!("../src/main.rs");

fn apply_body() -> &'static str {
    let (_, tail) = MAIN
        .split_once("async fn cmd_apply")
        .expect("cmd_apply must exist");
    let (body, _) = tail
        .split_once("async fn cmd_dump")
        .expect("cmd_dump must follow cmd_apply");
    body
}

fn first(body: &str, needle: &str) -> usize {
    body.find(needle)
        .unwrap_or_else(|| panic!("missing apply wiring marker: {needle}"))
}

fn last(body: &str, needle: &str) -> usize {
    body.rfind(needle)
        .unwrap_or_else(|| panic!("missing apply wiring marker: {needle}"))
}

#[test]
fn postgres_apply_confirms_then_leases_revalidates_and_executes() {
    let body = apply_body();
    let confirmation = first(body, "if !r.get_bool(\"DPM_YES\")");
    let acquisition = first(
        body,
        "Some(acquire_migration_lease(target_url).await?)",
    );
    let refresh = first(body, "let refreshed_inputs = load_sides(r, false).await?");
    let stale_plan_guard = first(body, "if refreshed_script.sql != script.sql");
    let validation = first(body, "ValidatedScript::parse(&script.sql)");
    let execution = first(body, "lease.apply(&validated).await?");

    assert!(confirmation < acquisition, "lease must follow confirmation");
    assert!(acquisition < refresh, "fresh catalogs require an owned lease");
    assert!(refresh < stale_plan_guard, "fresh plan must be compared");
    assert!(
        stale_plan_guard < validation,
        "stale reviewed plans must fail before validation"
    );
    assert!(validation < execution, "only validated SQL may execute");
}

#[test]
fn postgres_and_cockroach_execution_paths_are_explicit() {
    let body = apply_body();
    let leased_arm = first(body, "Some(lease) =>");
    let leased_execution = first(body, "lease.apply(&validated).await?");
    let fallback = first(body, "None => dpm::apply::apply_script(target_url, &script.sql).await?");

    assert!(leased_arm < leased_execution);
    assert!(leased_execution < fallback);
}

#[test]
fn normal_release_follows_convergence_and_cross_check_evidence() {
    let body = apply_body();
    let convergence = first(body, "Post-apply convergence check");
    let cross_checks = first(body, "Optional independent cross-checks");
    let final_release = last(body, "release_migration_lease(lease).await?");

    assert!(convergence < cross_checks);
    assert!(cross_checks < final_release);
}
