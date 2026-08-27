//! Execute a generated migration script against a target database.
//!
//! Scripts are split into individual statements (dollar-quote, string, and
//! comment aware) and executed one at a time over the simple protocol. This
//! matters for two reasons:
//! - `ALTER TYPE ... ADD VALUE` must run outside any transaction; sending the
//!   whole script as one batch would wrap it in an implicit transaction.
//! - Per-statement errors can point at the exact failing SQL.
//!
//! The script's own `BEGIN;` / `COMMIT;` statements provide the transaction
//! boundary for everything that belongs inside one.

use anyhow::{Context, Result};
use sqlx::{Connection, PgConnection};

fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$') || !byte.is_ascii()
}

fn single_quote_uses_backslash_escapes(bytes: &[u8], quote: usize) -> bool {
    let e_string = quote >= 1
        && matches!(bytes[quote - 1], b'e' | b'E')
        && (quote == 1 || !is_identifier_byte(bytes[quote - 2]));
    let unicode_escape = quote >= 2
        && matches!(bytes[quote - 2], b'u' | b'U')
        && bytes[quote - 1] == b'&'
        && (quote == 2 || !is_identifier_byte(bytes[quote - 3]));
    e_string || unicode_escape
}

fn dollar_quote_tag_end(bytes: &[u8], start: usize) -> Option<usize> {
    let tag_start = start.checked_add(1)?;
    let end = match bytes.get(tag_start).copied()? {
        b'$' => tag_start,
        b if b.is_ascii_alphabetic() || b == b'_' => {
            let rest_start = tag_start + 1;
            rest_start
                + bytes[rest_start..]
                    .iter()
                    .take_while(|byte| byte.is_ascii_alphanumeric() || **byte == b'_')
                    .count()
        }
        _ => return None,
    };
    (end < bytes.len() && bytes[end] == b'$').then_some(end)
}

/// Split SQL text into executable statements. Handles:
/// - single-quoted strings (with `''` escapes)
/// - double-quoted identifiers
/// - dollar-quoted bodies (`$$ ... $$`, `$tag$ ... $tag$`)
/// - line comments (`-- ...`) and block comments (`/* ... */`, nested)
pub fn split_statements(sql: &str) -> Vec<String> {
    let bytes = sql.as_bytes();
    let mut statements = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    let n = bytes.len();

    while i < n {
        match bytes[i] {
            b'\'' => {
                let backslash_escapes = single_quote_uses_backslash_escapes(bytes, i);
                i += 1;
                while i < n {
                    if backslash_escapes && bytes[i] == b'\\' {
                        i = (i + 2).min(n);
                        continue;
                    }
                    if bytes[i] == b'\'' {
                        if i + 1 < n && bytes[i + 1] == b'\'' {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
                i = (i + 1).min(n);
            }
            b'"' => {
                i += 1;
                while i < n {
                    if bytes[i] == b'"' {
                        if i + 1 < n && bytes[i + 1] == b'"' {
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            b'-' if i + 1 < n && bytes[i + 1] == b'-' => {
                while i < n && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if i + 1 < n && bytes[i + 1] == b'*' => {
                let mut depth = 1;
                i += 2;
                while i < n && depth > 0 {
                    if i + 1 < n && bytes[i] == b'/' && bytes[i + 1] == b'*' {
                        depth += 1;
                        i += 2;
                    } else if i + 1 < n && bytes[i] == b'*' && bytes[i + 1] == b'/' {
                        depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
            }
            b'$' => {
                // Possible dollar-quote opener: $tag$ where tag is
                // [A-Za-z_][A-Za-z0-9_]* or empty.
                if let Some(j) = dollar_quote_tag_end(bytes, i) {
                    let tag = &sql[i..=j];
                    if let Some(close) = sql[j + 1..].find(tag) {
                        i = j + 1 + close + tag.len();
                    } else {
                        i = n;
                    }
                } else {
                    i += 1;
                }
            }
            b';' => {
                statements.extend(executable_statement(&sql[start..i]));
                i += 1;
                start = i;
            }
            _ => i += 1,
        }
    }
    statements.extend(executable_statement(&sql[start..]));
    statements
}

fn executable_statement(fragment: &str) -> Option<String> {
    let stmt = fragment.trim();
    (!only_comments(stmt)).then(|| stmt.to_string())
}

/// True when the fragment contains no executable tokens (only whitespace and
/// comments) and therefore should not be sent to the server.
fn only_comments(fragment: &str) -> bool {
    let mut rest = fragment.trim_start();
    loop {
        if rest.is_empty() {
            return true;
        }
        if let Some(after) = rest.strip_prefix("--") {
            rest = match after.find('\n') {
                Some(pos) => after[pos + 1..].trim_start(),
                None => "",
            };
            continue;
        }
        if let Some(after) = rest.strip_prefix("/*") {
            let bytes = after.as_bytes();
            let mut depth = 1usize;
            let mut i = 0usize;
            while i < bytes.len() && depth > 0 {
                if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
                    depth += 1;
                    i += 2;
                } else if i + 1 < bytes.len() && bytes[i] == b'*' && bytes[i + 1] == b'/' {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            if depth > 0 {
                return true;
            }
            rest = after[i..].trim_start();
            continue;
        }
        return false;
    }
}

/// Remove psql meta-command lines (`\restrict`, `\unrestrict`, `\connect`,
/// `\.`, ...) that appear in `pg_dump` output. Only lines *outside* quoted
/// regions are touched, so a backslash-leading line inside a dollar-quoted
/// function body survives. Recent pg_dump versions (2025 security releases)
/// emit `\restrict`/`\unrestrict` unconditionally, so this is required for
/// "diff two dumps" workflows.
pub fn strip_psql_meta_commands(sql: &str) -> String {
    // Mark which byte offsets are inside a quoted region using the same
    // scanner rules as split_statements.
    let bytes = sql.as_bytes();
    let n = bytes.len();
    let mut in_quote = vec![false; n];
    let mut i = 0usize;
    while i < n {
        match bytes[i] {
            b'\'' => {
                let start = i;
                let backslash_escapes = single_quote_uses_backslash_escapes(bytes, i);
                i += 1;
                while i < n {
                    if backslash_escapes && bytes[i] == b'\\' {
                        i = (i + 2).min(n);
                        continue;
                    }
                    if bytes[i] == b'\'' {
                        if i + 1 < n && bytes[i + 1] == b'\'' {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
                i = (i + 1).min(n);
                in_quote[start..i.min(n)].fill(true);
            }
            b'"' => {
                let start = i;
                i += 1;
                while i < n {
                    if bytes[i] == b'"' {
                        if i + 1 < n && bytes[i + 1] == b'"' {
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                in_quote[start..i.min(n)].fill(true);
            }
            b'$' => {
                if let Some(j) = dollar_quote_tag_end(bytes, i) {
                    let tag = &sql[i..=j];
                    let start = i;
                    if let Some(close) = sql[j + 1..].find(tag) {
                        i = j + 1 + close + tag.len();
                    } else {
                        i = n;
                    }
                    in_quote[start..i.min(n)].fill(true);
                } else {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }

    sql.split_inclusive('\n')
        .scan(0usize, |offset, line| {
            let line_start = *offset;
            *offset += line.len();
            let is_meta = line.trim_start().starts_with('\\')
                && !in_quote.get(line_start).copied().unwrap_or(false);
            Some((!is_meta).then_some(line))
        })
        .flatten()
        .collect()
}

/// True for statements that depend on roles/ownership and are skipped when
/// materializing a dump into a shadow database (dpm does not diff ownership
/// or grants; a fresh shadow db lacks the production roles).
pub fn is_role_dependent_statement(stmt: &str) -> bool {
    let upper = stmt.trim_start().to_ascii_uppercase();
    match upper.as_str() {
        s if s.starts_with("GRANT ") => true,
        s if s.starts_with("REVOKE ") => true,
        s if s.starts_with("SET SESSION AUTHORIZATION") => true,
        s if s.starts_with("SET ROLE") => true,
        s if s.starts_with("ALTER ") => s.contains(" OWNER TO "),
        _ => false,
    }
}

pub fn truncate_sql(stmt: &str) -> String {
    const MAX: usize = 500;
    match stmt.len() <= MAX {
        true => stmt.to_string(),
        false => {
            let end = (0..=MAX)
                .rev()
                .find(|&end| stmt.is_char_boundary(end))
                .unwrap_or(0);
            format!("{}… [{} bytes]", &stmt[..end], stmt.len())
        }
    }
}

#[derive(Debug)]
pub struct ApplyReport {
    pub executed: usize,
}

/// Execute a script statement-by-statement. On error the connection has
/// whatever transaction state the script left; we attempt a ROLLBACK so the
/// error surfaces cleanly.
pub async fn apply_script(url: &str, sql: &str) -> Result<ApplyReport> {
    let mut conn = PgConnection::connect(url)
        .await
        .with_context(|| format!("connecting to {}", crate::introspect::redact_url(url)))?;
    let statements = split_statements(sql);
    let mut executed = 0usize;
    for (i, stmt) in statements.iter().enumerate() {
        if let Err(err) = sqlx::raw_sql(stmt).execute(&mut conn).await {
            let _ = sqlx::raw_sql("ROLLBACK").execute(&mut conn).await;
            let _ = conn.close().await;
            return Err(anyhow::anyhow!(err)).with_context(|| {
                format!(
                    "statement {}/{} failed:\n{}",
                    i + 1,
                    statements.len(),
                    truncate_sql(stmt)
                )
            });
        }
        executed += 1;
    }
    let _ = conn.close().await;
    Ok(ApplyReport { executed })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_simple_statements() {
        let stmts = split_statements("CREATE TABLE a (id int);\nDROP TABLE b;\n");
        assert_eq!(stmts.len(), 2);
        assert_eq!(stmts[1], "DROP TABLE b");
    }

    #[test]
    fn semicolons_in_strings_and_dollar_quotes_do_not_split() {
        let sql = r#"
INSERT INTO t VALUES ('a;b');
CREATE FUNCTION f() RETURNS trigger AS $fn$
BEGIN
  PERFORM 1; PERFORM 2;
  RETURN NEW;
END;
$fn$ LANGUAGE plpgsql;
SELECT 1;
"#;
        let stmts = split_statements(sql);
        assert_eq!(stmts.len(), 3, "got: {stmts:#?}");
        assert!(stmts[1].contains("PERFORM 2;"));
    }

    #[test]
    fn escape_strings_and_quoted_identifier_escapes_do_not_split() {
        let sql = r#"SELECT E'it\'s; still one', U&'d\0061ta; still one';
CREATE TABLE "semi;""quoted" (id int);"#;
        let stmts = split_statements(sql);
        assert_eq!(stmts.len(), 2, "got: {stmts:#?}");
        assert!(stmts[0].contains("still one"));
        assert!(stmts[1].contains("semi;\"\"quoted"));
    }

    #[test]
    fn numeric_dollar_tokens_are_not_quote_tags() {
        let stmts = split_statements("SELECT $1$; SELECT 2;");
        assert_eq!(stmts.len(), 2, "got: {stmts:#?}");
    }

    #[test]
    fn comments_with_semicolons_are_ignored() {
        let sql = "-- gated: DROP TABLE x;\n/* also; not this */\nSELECT 1;\n-- trailing comment\n";
        let stmts = split_statements(sql);
        assert_eq!(stmts.len(), 1);
        assert!(stmts[0].contains("SELECT 1"));
    }

    #[test]
    fn nested_block_comments() {
        let sql = "/* outer /* inner; */ still; */ SELECT 2;";
        let stmts = split_statements(sql);
        assert_eq!(stmts.len(), 1);
        assert!(stmts[0].ends_with("SELECT 2"));
    }

    #[test]
    fn dollar_tag_mismatch_does_not_close_early() {
        let sql = "CREATE FUNCTION g() RETURNS text AS $a$ x $b$ y $a$ LANGUAGE sql;";
        let stmts = split_statements(sql);
        assert_eq!(stmts.len(), 1);
    }

    #[test]
    fn strips_psql_meta_commands_outside_quotes_only() {
        let sql = "\\restrict abc123\nSET client_encoding = 'UTF8';\n\\unrestrict abc123\nCREATE FUNCTION f() RETURNS text AS $b$\n\\not-a-meta-command\n$b$ LANGUAGE sql;\n\\.\n";
        let cleaned = strip_psql_meta_commands(sql);
        assert!(!cleaned.contains("\\restrict"));
        assert!(!cleaned.contains("\\unrestrict"));
        assert!(!cleaned.contains("\\.\n"));
        assert!(
            cleaned.contains("\\not-a-meta-command"),
            "backslash line inside dollar quotes must survive"
        );
        assert!(cleaned.contains("SET client_encoding"));
    }

    #[test]
    fn role_dependent_statements_are_recognized() {
        assert!(is_role_dependent_statement(
            "GRANT ALL ON TABLE public.t TO app_user"
        ));
        assert!(is_role_dependent_statement(
            "REVOKE ALL ON SCHEMA public FROM PUBLIC"
        ));
        assert!(is_role_dependent_statement(
            "ALTER TABLE public.users OWNER TO produser"
        ));
        assert!(is_role_dependent_statement(
            "  alter function public.f() owner to produser"
        ));
        assert!(is_role_dependent_statement("SET SESSION AUTHORIZATION 'x'"));
        assert!(!is_role_dependent_statement("CREATE TABLE t (id int)"));
        assert!(!is_role_dependent_statement(
            "ALTER TABLE t ADD COLUMN owner_to text"
        ));
    }
}

#[cfg(test)]
mod splitter_edge_tests {
    use super::*;

    #[test]
    fn empty_and_comment_only_inputs_yield_no_statements() {
        assert!(split_statements("").is_empty());
        assert!(split_statements("   \n\t\n").is_empty());
        assert!(split_statements("-- just a comment\n/* and a block */\n").is_empty());
    }

    #[test]
    fn nested_comment_only_tail_is_not_executed() {
        assert!(split_statements("/* outer /* inner */ still outer */").is_empty());
    }

    #[test]
    fn truncating_multibyte_sql_never_slices_inside_utf8() {
        let statement = format!("SELECT '{}'", "é".repeat(300));
        let rendered = truncate_sql(&statement);
        assert!(rendered.contains("bytes]"));
        assert!(rendered.len() < statement.len());
    }

    #[test]
    fn meta_strip_preserves_lines_inside_escape_strings() {
        let sql = r#"SELECT E'one\'two
\. still data
';
\.
"#;
        let cleaned = strip_psql_meta_commands(sql);
        assert!(cleaned.contains("\\. still data"));
        assert!(!cleaned.trim_end().ends_with("\\."));
    }

    #[test]
    fn trailing_statement_without_semicolon_is_kept() {
        let stmts = split_statements("SELECT 1;\nSELECT 2");
        assert_eq!(stmts, vec!["SELECT 1".to_string(), "SELECT 2".to_string()]);
    }

    #[test]
    fn unterminated_dollar_quote_does_not_panic_or_split() {
        let stmts = split_statements("CREATE FUNCTION f() AS $x$ BEGIN; never closed");
        assert_eq!(stmts.len(), 1);
    }

    #[test]
    fn quoted_identifiers_with_semicolons_do_not_split() {
        let stmts = split_statements("CREATE TABLE \"we;ird\" (id int);SELECT 1;");
        assert_eq!(stmts.len(), 2);
        assert!(stmts[0].contains("we;ird"));
    }

    #[test]
    fn meta_strip_preserves_backslash_lines_inside_strings() {
        let sql = "INSERT INTO t VALUES ('line1\n\\. not a meta terminator\nline3');\n\\.\n";
        let cleaned = strip_psql_meta_commands(sql);
        assert!(
            cleaned.contains("\\. not a meta terminator"),
            "inside string must survive"
        );
        assert!(
            !cleaned.trim_end().ends_with("\\."),
            "top-level \\. removed"
        );
    }

    #[test]
    fn role_statement_detection_is_not_overeager() {
        assert!(!is_role_dependent_statement("CREATE TABLE grants (id int)"));
        assert!(!is_role_dependent_statement(
            "COMMENT ON TABLE t IS 'GRANT nothing'"
        ));
        assert!(is_role_dependent_statement("\n  GRANT SELECT ON t TO r"));
    }
}
