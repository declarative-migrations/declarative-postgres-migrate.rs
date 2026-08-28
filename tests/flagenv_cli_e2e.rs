//! Process-level coverage for the flags-2-env command contract.
//!
//! These tests deliberately launch child tools from dpm. That is the boundary
//! where an override-map-only implementation is insufficient: command identity
//! must be published to the process environment before external checkers or AI
//! reviewers are spawned.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const SCHEMA: &str = "CREATE TABLE widgets (id bigint PRIMARY KEY, name text NOT NULL);\n";
static NEXT: AtomicU64 = AtomicU64::new(0);

fn dpm() -> Command {
    Command::new(env!("CARGO_BIN_EXE_dpm"))
}

fn admin_url() -> Option<String> {
    match std::env::var("DPM_TEST_DATABASE_URL") {
        Ok(value) if !value.is_empty() => Some(value),
        _ => {
            eprintln!("skipping: DPM_TEST_DATABASE_URL is not set");
            None
        }
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

struct Fixture {
    dir: PathBuf,
    source: PathBuf,
    target: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "dpm-flagenv-e2e-{label}-{}-{id}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create flags-2-env E2E fixture directory");
        let source = dir.join("source.sql");
        let target = dir.join("target.sql");
        fs::write(&source, SCHEMA).expect("write source SQL fixture");
        fs::write(&target, SCHEMA).expect("write target SQL fixture");
        Self {
            dir,
            source,
            target,
        }
    }

    fn source(&self) -> &Path {
        &self.source
    }

    fn target(&self) -> &Path {
        &self.target
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn verify_external_checker_receives_canonical_command_environment() {
    let Some(admin) = admin_url() else {
        return;
    };
    let fixture = Fixture::new("verify");
    let checker = r#"test "$FLAGS2ENV_COMMAND" = verify && test "$DPM_CMD_VERIFY" = true && test -z "${DPM_CMD_DIFF:-}""#;

    let output = dpm()
        .args(["verify", "--source"])
        .arg(fixture.source())
        .arg("--target-sql")
        .arg(fixture.target())
        .args([
            "--shadow",
            &admin,
            "--allow-destructive-sql",
            "--external-check",
            checker,
        ])
        // Simulate a stale parent shell. The selected command must replace the
        // generic command env and clear every non-selected marker.
        .env("FLAGS2ENV_COMMAND", "diff")
        .env("DPM_CMD_DIFF", "true")
        .output()
        .expect("run dpm verify with an external checker");

    assert!(
        output.status.success(),
        "verify command environment was not propagated:\n{}",
        stderr(&output)
    );
    assert!(stderr(&output).contains("VERIFIED"));
}

#[test]
fn review_child_receives_only_the_review_marker() {
    let Some(admin) = admin_url() else {
        return;
    };
    let fixture = Fixture::new("review");
    let reviewer = r#"test "$FLAGS2ENV_COMMAND" = review && test "$DPM_CMD_REVIEW" = true && test -z "${DPM_CMD_APPLY:-}" && printf 'DPM_VERDICT: APPROVE\n'"#;

    let output = dpm()
        .args(["review", "--source"])
        .arg(fixture.source())
        .arg("--target-sql")
        .arg(fixture.target())
        .args([
            "--shadow",
            &admin,
            "--ai-tool",
            "custom",
            "--ai-transport",
            "cli",
            "--ai-cmd",
            reviewer,
        ])
        .env("FLAGS2ENV_COMMAND", "apply")
        .env("DPM_CMD_APPLY", "true")
        .output()
        .expect("run dpm review with a custom reviewer");

    assert!(
        output.status.success(),
        "review command environment was not propagated:\n{}",
        stderr(&output)
    );
    assert!(stderr(&output).contains("DPM_VERDICT: APPROVE"));
}

#[test]
fn diff_ai_review_replaces_stale_command_identity() {
    let Some(admin) = admin_url() else {
        return;
    };
    let fixture = Fixture::new("diff");
    let reviewer = r#"test "$FLAGS2ENV_COMMAND" = diff && test "$DPM_CMD_DIFF" = true && test -z "${DPM_CMD_REVIEW:-}" && printf 'DPM_VERDICT: APPROVE\n'"#;

    let output = dpm()
        .args(["diff", "--source"])
        .arg(fixture.source())
        .arg("--target-sql")
        .arg(fixture.target())
        .args([
            "--shadow",
            &admin,
            "--ai-review",
            "--ai-tool",
            "custom",
            "--ai-transport",
            "cli",
            "--ai-cmd",
            reviewer,
        ])
        .env("FLAGS2ENV_COMMAND", "review")
        .env("DPM_CMD_REVIEW", "true")
        .output()
        .expect("run dpm diff with a custom AI reviewer");

    assert!(
        output.status.success(),
        "diff command environment was not propagated:\n{}",
        stderr(&output)
    );
    assert!(stderr(&output).contains("DPM_VERDICT: APPROVE"));
}
