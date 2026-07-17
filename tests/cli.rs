//! End-to-end tests of the dpm BINARY (not the library): flag parsing,
//! exit-code contract, output formats, the destructive two-consent gate, and
//! the AI review verdict paths — everything a shell user or CI touches.
//!
//! Database-backed cases are gated on DPM_TEST_DATABASE_URL like the rest of
//! the integration suite; pure CLI cases always run.

use std::process::{Command, Output};

fn dpm() -> Command {
    Command::new(env!("CARGO_BIN_EXE_dpm"))
}

fn admin_url() -> Option<String> {
    match std::env::var("DPM_TEST_DATABASE_URL") {
        Ok(v) if !v.is_empty() => Some(v),
        _ => {
            eprintln!("skipping: DPM_TEST_DATABASE_URL not set (run scripts/test.sh)");
            None
        }
    }
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}
fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

fn scratch(name: &str) -> std::path::PathBuf {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("dpm-cli-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

// ---------------------------------------------------------------------------
// pure CLI (no database)
// ---------------------------------------------------------------------------

#[test]
fn help_shows_commands_and_flag_table() {
    let out = dpm().arg("help").output().unwrap();
    assert!(out.status.success());
    let text = stdout(&out);
    for needle in ["diff", "apply", "verify", "review", "bootstrap", "dump",
                   "--cross-check-all", "SOURCE_DATABASE_URL", "DPM_AI_TRANSPORT"] {
        assert!(text.contains(needle), "help missing {needle:?}");
    }
}

#[test]
fn version_prints_cargo_version() {
    for args in [
        &["version"][..],
        &["--version"][..],
        &["diff", "--version"][..],
    ] {
        let out = dpm().args(args).output().unwrap();
        assert!(out.status.success(), "args {args:?}: {}", stderr(&out));
        assert_eq!(
            stdout(&out).trim(),
            format!("dpm {}", env!("CARGO_PKG_VERSION")),
            "args {args:?}"
        );
    }
}

#[test]
fn help_shortcuts_work_at_root_and_command_level() {
    for args in [
        &["help"][..],
        &["--help"][..],
        &["-h"][..],
        &["apply", "--help"][..],
    ] {
        let out = dpm().args(args).output().unwrap();
        assert!(out.status.success(), "args {args:?}: {}", stderr(&out));
        assert!(stdout(&out).contains("USAGE"), "args {args:?}");
    }
}

#[test]
fn unknown_command_and_unknown_flag_error_cleanly() {
    let out = dpm().arg("frobnicate").output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("unknown command"));

    let out = dpm().args(["diff", "--definitely-not-a-flag"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("unknown option"));
}

#[test]
fn diff_without_source_is_a_clear_error() {
    let out = dpm()
        .arg("diff")
        .env_remove("SOURCE_DATABASE_URL")
        .env_remove("TARGET_DATABASE_URL")
        .env_remove("DATABASE_URL")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("no source"));
}

#[test]
fn conflicting_kind_flags_error() {
    let out = dpm()
        .args(["diff", "--source-sql", "a.sql", "--source-json", "b.json", "--target", "postgres://x@y/z"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("pick one"));
}

#[test]
fn explicit_side_kinds_validate_extensions() {
    let out = dpm()
        .args(["diff", "--source-sql", "desired.json", "--target", "postgres://x@y/z"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("must point at a .sql file"));

    let out = dpm()
        .args(["diff", "--source-json", "desired.sql", "--target", "postgres://x@y/z"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("must point at a .json catalog dump"));
}

#[test]
fn apply_rejects_non_live_targets_before_loading_sides() {
    let out = dpm()
        .args([
            "apply",
            "--source-sql",
            "desired.sql",
            "--target-sql",
            "current.sql",
            "--yes",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("apply needs a live --target database URL"));
}

// ---------------------------------------------------------------------------
// database-backed CLI flows
// ---------------------------------------------------------------------------

const CLI_SOURCE: &str = "CREATE TABLE widgets (id bigserial PRIMARY KEY, name text NOT NULL, price numeric(10,2) NOT NULL DEFAULT 0);\nCREATE INDEX widgets_name_idx ON widgets (name);";
const CLI_TARGET: &str = "CREATE TABLE widgets (id bigserial PRIMARY KEY, name text NOT NULL, obsolete boolean);";

#[test]
fn full_cli_lifecycle_sql_to_live() {
    let Some(admin) = admin_url() else { return };

    // Prepare a live target database via the library helpers.
    let rt = tokio::runtime::Runtime::new().unwrap();
    let target = rt.block_on(async {
        let db = dpm::source::ShadowDb::create(&admin, false).await.unwrap();
        db.apply_sql(CLI_TARGET).await.unwrap();
        db
    });
    let source_sql = scratch("desired.sql");
    std::fs::write(&source_sql, CLI_SOURCE).unwrap();

    // 1. diff (sql -> live): plan present, exit 0 without --fail-on-diff.
    let out = dpm()
        .args(["diff", "--source"])
        .arg(&source_sql)
        .args(["--target", &target.url, "--shadow", &admin])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let sql = stdout(&out);
    assert!(sql.contains("ADD COLUMN IF NOT EXISTS \"price\""), "{sql}");
    assert!(sql.contains("-- ALTER TABLE") && sql.contains("DROP COLUMN"), "destructive gated: {sql}");

    // 2. --fail-on-diff exits 2 while drift exists.
    let out = dpm()
        .args(["diff", "--fail-on-diff", "--source"])
        .arg(&source_sql)
        .args(["--target", &target.url, "--shadow", &admin])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));

    // 3. json format carries plan + summary.
    let out = dpm()
        .args(["diff", "--format", "json", "--source"])
        .arg(&source_sql)
        .args(["--target", &target.url, "--shadow", &admin])
        .output()
        .unwrap();
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert!(doc["summary"]["total"].as_u64().unwrap() >= 2);
    assert!(doc["changes"].as_array().unwrap().iter().any(|c| c["op"] == "add_column"));

    // 4. two-consent gate: sql-consent without ops-consent refuses pre-write.
    let out = dpm()
        .args(["apply", "--yes", "--allow-destructive-sql", "--source"])
        .arg(&source_sql)
        .args(["--target", &target.url, "--shadow", &admin])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("--allow-destructive-ops"));

    // 5. review with a fake AI: reject => exit 4, approve => exit 0.
    let out = dpm()
        .args(["review", "--ai-tool", "custom", "--ai-cmd", "echo 'DPM_VERDICT: REJECT nope'", "--source"])
        .arg(&source_sql)
        .args(["--target", &target.url, "--shadow", &admin])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(4), "{}", stderr(&out));
    let out = dpm()
        .args(["review", "--ai-tool", "custom", "--ai-cmd", "echo 'DPM_VERDICT: APPROVE'", "--source"])
        .arg(&source_sql)
        .args(["--target", &target.url, "--shadow", &admin])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));

    // 6. apply with both consents converges; then fail-on-diff exits 0.
    let out = dpm()
        .args(["apply", "--yes", "--allow-destructive", "--source"])
        .arg(&source_sql)
        .args(["--target", &target.url, "--shadow", &admin])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stderr(&out).contains("converged"));
    let out = dpm()
        .args(["diff", "--fail-on-diff", "--source"])
        .arg(&source_sql)
        .args(["--target", &target.url, "--shadow", &admin])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "post-apply drift: {}", stdout(&out));

    // 7. dump -> catalog.json usable as a side; json↔live now identical.
    let dump_path = scratch("target.json");
    let out = dpm()
        .args(["dump", "--source", &target.url, "-o"])
        .arg(&dump_path)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let out = dpm()
        .args(["diff", "--fail-on-diff", "--source"])
        .arg(&dump_path)
        .args(["--target", &target.url])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));

    // 8. verify end-to-end through the CLI (with a stubbed agreeing checker).
    let out = dpm()
        .args(["verify", "--source"])
        .arg(&source_sql)
        .args(["--target", &target.url, "--shadow", &admin, "--allow-destructive-sql"])
        .args(["--external-check", "true"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(stderr(&out).contains("VERIFIED"));

    rt.block_on(target.drop_db());
}

#[test]
fn env_only_diff_writes_json_and_flags_override_environment() {
    let Some(admin) = admin_url() else { return };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let target = rt.block_on(async {
        let db = dpm::source::ShadowDb::create(&admin, false).await.unwrap();
        db.apply_sql(CLI_TARGET).await.unwrap();
        db
    });
    let source_sql = scratch("env-desired.sql");
    let output_json = scratch("env-plan.json");
    std::fs::write(&source_sql, CLI_SOURCE).unwrap();

    let out = dpm()
        .arg("diff")
        .env("SOURCE_SQL_FILE", &source_sql)
        .env("TARGET_DATABASE_URL", &target.url)
        .env("SHADOW_DATABASE_URL", &admin)
        .env("DPM_FORMAT", "json")
        .env("DPM_FAIL_ON_DIFF", "true")
        .env("DPM_OUT", &output_json)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stdout(&out).is_empty(), "--out must keep stdout clean");
    assert!(stderr(&out).contains("dpm: wrote"));
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&output_json).unwrap()).unwrap();
    assert!(doc["summary"]["total"].as_u64().unwrap() >= 2);
    assert!(doc["changes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|change| change["op"] == "add_column"));

    let out = dpm()
        .args(["diff", "--format", "sql", "--source"])
        .arg(&source_sql)
        .args(["--target", &target.url, "--shadow", &admin])
        .env("DPM_FORMAT", "json")
        .env("DPM_FAIL_ON_DIFF", "false")
        .env_remove("DPM_OUT")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).starts_with("-- Generated by"));

    rt.block_on(target.drop_db());
}

#[test]
fn bootstrap_emits_full_ddl_without_target() {
    let Some(admin) = admin_url() else { return };
    let source_sql = scratch("boot.sql");
    std::fs::write(&source_sql, CLI_SOURCE).unwrap();
    let out = dpm()
        .args(["bootstrap", "--source"])
        .arg(&source_sql)
        .args(["--shadow", &admin])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let sql = stdout(&out);
    assert!(sql.contains("CREATE TABLE IF NOT EXISTS \"public\".\"widgets\""));
    assert!(sql.contains("widgets_name_idx"));
}

/// verify must exit 3 when an external checker disagrees, even though dpm
/// itself converged.
#[test]
fn verify_exits_3_when_external_check_disagrees() {
    let Some(admin) = admin_url() else { return };
    let source_sql = scratch("v3.sql");
    std::fs::write(&source_sql, CLI_SOURCE).unwrap();

    // `false` exits nonzero => disagreement.
    let out = dpm()
        .args(["verify", "--source"])
        .arg(&source_sql)
        .args(["--shadow", &admin, "--allow-destructive-sql"])
        .args(["--target-sql"])
        .arg(&source_sql) // identical sides: dpm converges trivially
        .args(["--external-check", "false"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3), "stderr: {}", stderr(&out));
    assert!(stderr(&out).contains("DISAGREED"));

    // Sanity: with an agreeing checker the same invocation exits 0.
    let out = dpm()
        .args(["verify", "--source"])
        .arg(&source_sql)
        .args(["--shadow", &admin, "--allow-destructive-sql"])
        .args(["--target-sql"])
        .arg(&source_sql)
        .args(["--external-check", "true"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
}

/// apply without --yes must prompt; anything but literal "yes" aborts with
/// exit 1 and writes nothing.
#[test]
fn apply_interactive_abort_leaves_target_untouched() {
    use std::io::Write as _;
    let Some(admin) = admin_url() else { return };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let target = rt.block_on(async {
        let db = dpm::source::ShadowDb::create(&admin, false).await.unwrap();
        db.apply_sql(CLI_TARGET).await.unwrap();
        db
    });
    let source_sql = scratch("abort.sql");
    std::fs::write(&source_sql, CLI_SOURCE).unwrap();

    for answer in ["no\n", "y\n", ""] {
        let mut child = dpm()
            .args(["apply", "--source"])
            .arg(&source_sql)
            .args(["--target", &target.url, "--shadow", &admin])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.as_mut().unwrap().write_all(answer.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        assert_eq!(out.status.code(), Some(1), "answer {answer:?} must abort");
        assert!(stderr(&out).contains("aborted"), "answer {answer:?}: {}", stderr(&out));
    }

    // Nothing was applied: the price column from the source must not exist.
    let has_price = rt.block_on(async {
        let mut conn = dpm::introspect::connect(&target.url).await.unwrap();
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM information_schema.columns WHERE table_name='widgets' AND column_name='price'",
        )
        .fetch_one(&mut conn)
        .await
        .unwrap();
        n
    });
    assert_eq!(has_price, 0, "aborted apply must not modify the target");
    rt.block_on(target.drop_db());
}

/// --schemas narrows the CLI diff exactly like the library option.
#[test]
fn schemas_flag_scopes_the_cli_diff() {
    let Some(admin) = admin_url() else { return };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (a, b) = rt.block_on(async {
        let a = dpm::source::ShadowDb::create(&admin, false).await.unwrap();
        let b = dpm::source::ShadowDb::create(&admin, false).await.unwrap();
        a.apply_sql("CREATE SCHEMA keep; CREATE TABLE keep.t (id int PRIMARY KEY); CREATE TABLE public.drift (id int);").await.unwrap();
        b.apply_sql("CREATE SCHEMA keep; CREATE TABLE keep.t (id int PRIMARY KEY);").await.unwrap();
        (a, b)
    });

    // Unscoped: drift exists -> exit 2.
    let out = dpm()
        .args(["diff", "--fail-on-diff", "--source", &a.url, "--target", &b.url])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));

    // Scoped to `keep`: identical -> exit 0.
    let out = dpm()
        .args(["diff", "--fail-on-diff", "--schemas", "keep", "--source", &a.url, "--target", &b.url])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stdout(&out));

    rt.block_on(async {
        a.drop_db().await;
        b.drop_db().await;
    });
}
