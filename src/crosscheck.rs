//! Cross-checking dpm's output with independent schema-diff tools.
//!
//! migra (https://github.com/djrobstep/migra) and pgdiff
//! (https://github.com/joncrlsn/pgdiff) are second-class citizens of this
//! project: dpm's own test suite uses them to validate convergence when they
//! are installed, and end users can request the same via
//! `--cross-check-with-migra` / `--cross-check-with-pgdiff`.
//!
//! Semantics: a cross-check runs AFTER dpm's migration has been applied
//! (to the shadow replica in `verify`, or to the real target in `apply`) and
//! asks the independent tool "is there any remaining schema difference
//! between the migrated database and the source?". Agreement = the tool
//! reports no differences. This validates dpm with somebody else's diff
//! engine rather than its own.
//!
//! Neither tool is a build dependency — they are located on PATH (or via
//! DPM_MIGRA_BIN / DPM_PGDIFF_BIN) at runtime, and
//! `scripts/install-crosscheckers.sh` installs both.

use anyhow::{Context, Result};

#[derive(Clone, Debug)]
pub struct CheckReport {
    pub name: String,
    pub command: String,
    pub agreed: bool,
    /// Trimmed tool output (the residual DDL / differences it found, if any).
    pub output: String,
    /// Tool missing, crashed, or URL unparseable — reported, never fatal.
    pub error: Option<String>,
}

/// Fields of a postgres:// URL needed to drive tools that take discrete
/// connection flags (pgdiff) instead of a URL.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UrlParts {
    pub user: String,
    pub password: Option<String>,
    pub host: String,
    pub port: String,
    pub dbname: String,
    pub sslmode: String,
}

pub fn parse_postgres_url(url: &str) -> Result<UrlParts> {
    let rest = url
        .split_once("://")
        .map(|(_, r)| r)
        .with_context(|| format!("not a URL: {url:?}"))?;
    let (main, query) = match rest.split_once('?') {
        Some((m, q)) => (m, Some(q)),
        None => (rest, None),
    };
    let (creds, hostpart) = match main.rsplit_once('@') {
        Some((c, h)) => (Some(c), h),
        None => (None, main),
    };
    let (hostport, dbname) = match hostpart.split_once('/') {
        Some((hp, db)) => (hp, db),
        None => (hostpart, ""),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty() => (h, p),
        _ => (hostport, "5432"),
    };
    let (user, password) = match creds {
        Some(c) => match c.split_once(':') {
            Some((u, p)) => (u.to_string(), Some(p.to_string())),
            None => (c.to_string(), None),
        },
        None => ("postgres".to_string(), None),
    };
    let mut sslmode = "disable".to_string();
    if let Some(q) = query {
        for pair in q.split('&') {
            if let Some((k, v)) = pair.split_once('=') {
                if k == "sslmode" {
                    sslmode = v.to_string();
                }
            }
        }
    }
    Ok(UrlParts {
        user,
        password,
        host: if host.is_empty() { "localhost".into() } else { host.into() },
        port: port.to_string(),
        dbname: dbname.to_string(),
        sslmode,
    })
}

fn run_shell(command: &str, extra_env: &[(String, String)]) -> Result<(bool, String, String)> {
    let mut cmd = std::process::Command::new("sh");
    cmd.arg("-c").arg(command);
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let output = cmd.output().with_context(|| format!("running: {command}"))?;
    Ok((
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    ))
}

fn binary_exists(bin: &str) -> bool {
    std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {}", shell_quote(bin)))
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// migra: `migra --unsafe <migrated_url> <source_url>` prints the DDL needed
/// to turn the first database into the second. Empty output + exit 0 means
/// "already identical" — agreement. Exit 2 with output means differences
/// remain. `--unsafe` is required so migra doesn't abort when the residual
/// would contain drops.
pub fn run_migra(bin: &str, migrated_url: &str, source_url: &str) -> CheckReport {
    let name = "migra".to_string();
    if !binary_exists(bin) {
        return CheckReport {
            name,
            command: bin.to_string(),
            agreed: false,
            output: String::new(),
            error: Some(format!(
                "{bin} not found on PATH — install with scripts/install-crosscheckers.sh \
                 (pip/pipx install migra) or set DPM_MIGRA_BIN"
            )),
        };
    }
    let command = format!(
        "{} --unsafe {} {}",
        shell_quote(bin),
        shell_quote(migrated_url),
        shell_quote(source_url)
    );
    match run_shell(&command, &[]) {
        Ok((success, stdout, stderr)) => {
            let out = stdout.trim().to_string();
            // migra exits 0 for "no diff", 2 for "diff found"; anything with
            // stderr content and no stdout is a tool error.
            if out.is_empty() && success {
                CheckReport { name, command, agreed: true, output: out, error: None }
            } else if !out.is_empty() {
                CheckReport { name, command, agreed: false, output: out, error: None }
            } else {
                CheckReport {
                    name,
                    command,
                    agreed: false,
                    output: out,
                    error: Some(format!("migra failed: {}", stderr.trim())),
                }
            }
        }
        Err(e) => CheckReport {
            name,
            command,
            agreed: false,
            output: String::new(),
            error: Some(format!("{e:#}")),
        },
    }
}

/// Schema aspects pgdiff can compare, in its recommended order. Role, grant
/// and ownership aspects are omitted — dpm does not manage them.
pub const PGDIFF_SCHEMA_TYPES: &[&str] = &[
    "SCHEMA", "SEQUENCE", "TABLE", "COLUMN", "PRIMARY_KEY", "INDEX", "VIEW", "MATVIEW",
    "FOREIGN_KEY", "FUNCTION", "TRIGGER",
];

/// pgdiff (joncrlsn/pgdiff) takes discrete connection flags and one schema
/// aspect per invocation; the driver loops over the aspects and collects any
/// non-comment output (pgdiff prints `-- comment` chatter plus real SQL for
/// differences). Agreement = no real SQL across all aspects.
pub fn run_pgdiff(bin: &str, migrated_url: &str, source_url: &str) -> CheckReport {
    let name = "pgdiff".to_string();
    if !binary_exists(bin) {
        return CheckReport {
            name,
            command: bin.to_string(),
            agreed: false,
            output: String::new(),
            error: Some(format!(
                "{bin} not found on PATH — install with scripts/install-crosscheckers.sh \
                 (go install github.com/joncrlsn/pgdiff@latest) or set DPM_PGDIFF_BIN"
            )),
        };
    }
    let (a, b) = match (parse_postgres_url(migrated_url), parse_postgres_url(source_url)) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => {
            return CheckReport {
                name,
                command: bin.to_string(),
                agreed: false,
                output: String::new(),
                error: Some(format!("{e:#}")),
            }
        }
    };

    let mut env: Vec<(String, String)> = Vec::new();
    if let Some(pw) = a.password.clone().or_else(|| b.password.clone()) {
        // pgdiff reads PGPASSWORD; differing per-side passwords are not
        // supported by this driver — use --external-check for those setups.
        env.push(("PGPASSWORD".into(), pw));
    }

    let base = format!(
        "{bin} -U1 {u1} -H1 {h1} -P1 {p1} -D1 {d1} -O1 'sslmode={s1}' \
         -U2 {u2} -H2 {h2} -P2 {p2} -D2 {d2} -O2 'sslmode={s2}'",
        bin = shell_quote(bin),
        u1 = shell_quote(&a.user),
        h1 = shell_quote(&a.host),
        p1 = a.port,
        d1 = shell_quote(&a.dbname),
        s1 = a.sslmode,
        u2 = shell_quote(&b.user),
        h2 = shell_quote(&b.host),
        p2 = b.port,
        d2 = shell_quote(&b.dbname),
        s2 = b.sslmode,
    );

    let mut all_sql = String::new();
    let mut errors = Vec::new();
    for aspect in PGDIFF_SCHEMA_TYPES {
        let command = format!("{base} {aspect}");
        match run_shell(&command, &env) {
            Ok((success, stdout, stderr)) => {
                let real: Vec<&str> = stdout
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty() && !l.starts_with("--"))
                    .collect();
                if !real.is_empty() {
                    all_sql.push_str(&format!("-- [{aspect}]\n{}\n", real.join("\n")));
                }
                if !success && real.is_empty() {
                    errors.push(format!("[{aspect}] {}", stderr.trim()));
                }
            }
            Err(e) => errors.push(format!("[{aspect}] {e:#}")),
        }
    }

    CheckReport {
        name,
        command: format!("{base} <{} aspects>", PGDIFF_SCHEMA_TYPES.len()),
        agreed: all_sql.is_empty() && errors.is_empty(),
        output: all_sql.trim().to_string(),
        error: if errors.is_empty() { None } else { Some(errors.join("; ")) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_parsing_covers_common_shapes() {
        let p = parse_postgres_url("postgres://alice:s3cr3t@db.example.com:6432/appdb?sslmode=require").unwrap();
        assert_eq!(p.user, "alice");
        assert_eq!(p.password.as_deref(), Some("s3cr3t"));
        assert_eq!(p.host, "db.example.com");
        assert_eq!(p.port, "6432");
        assert_eq!(p.dbname, "appdb");
        assert_eq!(p.sslmode, "require");

        let p = parse_postgres_url("postgres://postgres@127.0.0.1:54329/postgres").unwrap();
        assert_eq!(p.password, None);
        assert_eq!(p.port, "54329");
        assert_eq!(p.sslmode, "disable");

        let p = parse_postgres_url("postgresql://u@h/db").unwrap();
        assert_eq!(p.port, "5432");
        assert_eq!(p.dbname, "db");
    }

    #[test]
    fn missing_binary_reports_error_not_panic() {
        let report = run_migra("definitely-not-installed-xyz", "postgres://a@h/x", "postgres://a@h/y");
        assert!(!report.agreed);
        assert!(report.error.as_deref().unwrap_or("").contains("not found"));
        let report = run_pgdiff("definitely-not-installed-xyz", "postgres://a@h/x", "postgres://a@h/y");
        assert!(!report.agreed);
        assert!(report.error.is_some());
    }

    #[test]
    fn stub_migra_agreement_and_disagreement() {
        // Stub "migra" via a shell script on a private PATH dir.
        let dir = std::env::temp_dir().join(format!("dpm-stub-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let agree = dir.join("migra-agree");
        let disagree = dir.join("migra-disagree");
        std::fs::write(&agree, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::write(&disagree, "#!/bin/sh\necho 'alter table t add column x integer;'\nexit 2\n").unwrap();
        for f in [&agree, &disagree] {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(f, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let r = run_migra(agree.to_str().unwrap(), "postgres://a@h/x", "postgres://a@h/y");
        assert!(r.agreed, "{r:?}");
        let r = run_migra(disagree.to_str().unwrap(), "postgres://a@h/x", "postgres://a@h/y");
        assert!(!r.agreed);
        assert!(r.output.contains("alter table"));
    }
}
