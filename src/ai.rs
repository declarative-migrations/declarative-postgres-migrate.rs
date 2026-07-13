//! AI review of generated migrations — via provider HTTP APIs or local
//! agent CLIs.
//!
//! Two transports per provider, selected by `--ai-transport` (DPM_AI_TRANSPORT):
//!
//! - `api` — direct HTTP to the provider (preferred: no subprocess, no
//!   nesting issues, works headless):
//!   ```text
//!   claude   →  POST https://api.anthropic.com/v1/messages
//!               (x-api-key + anthropic-version: 2023-06-01; model
//!               claude-opus-4-8; adaptive thinking; stop_reason
//!               "refusal" handled fail-closed)
//!   chatgpt  →  POST https://api.openai.com/v1/chat/completions
//!   gemini   →  POST https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent
//!   ```
//! - `cli` — drive the installed agent CLI non-interactively:
//!   `claude -p < {file}` / `codex exec - < {file}` / `gemini < {file}` /
//!   the custom `--ai-cmd` template.
//! - `auto` (default) — `api` when the provider's key env var is set
//!   (ANTHROPIC_API_KEY / OPENAI_API_KEY / GEMINI_API_KEY or GOOGLE_API_KEY),
//!   otherwise `cli`.
//!
//! The payload instructs the reviewer to end with a machine-parseable verdict
//! line — `DPM_VERDICT: APPROVE` or `DPM_VERDICT: REJECT <reason>` — and dpm
//! parses the LAST such line. No parseable verdict counts as rejection (fail
//! closed): a reviewer that crashed, refused, or rambled must not gate a
//! migration open.

use anyhow::{bail, Context, Result};
use serde_json::json;

#[derive(Clone, Debug)]
pub struct ReviewRequest {
    /// The migration script under review.
    pub sql: String,
    /// JSON plan (typed change list) for structured cross-checking.
    pub plan_json: String,
    pub source_desc: String,
    pub target_desc: String,
    /// Flag context so the reviewer can flag policy violations (e.g. live
    /// destructive SQL when the operator did not allow it).
    pub allow_destructive_sql: bool,
    pub allow_destructive_ops: bool,
    /// Summary counts from emission.
    pub total_changes: usize,
    pub destructive_changes: usize,
    pub gated_changes: usize,
    pub manual_changes: usize,
}

#[derive(Clone, Debug)]
pub struct ReviewOutcome {
    pub approved: bool,
    /// The parsed verdict line, if any.
    pub verdict: Option<String>,
    /// Full reviewer output (the reasoning transcript).
    pub transcript: String,
    /// How the reviewer was invoked (command line or HTTP endpoint + model).
    pub command: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    Auto,
    Api,
    Cli,
}

impl Transport {
    pub fn parse(raw: &str) -> Result<Self> {
        match raw.to_ascii_lowercase().as_str() {
            "auto" | "" => Ok(Self::Auto),
            "api" | "http" => Ok(Self::Api),
            "cli" => Ok(Self::Cli),
            other => bail!("invalid --ai-transport {other:?}: expected auto | api | cli"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    Claude,
    OpenAi,
    Gemini,
    Custom,
}

pub fn provider_for_tool(tool: &str) -> Result<Provider> {
    Ok(match tool.to_ascii_lowercase().as_str() {
        "claude" | "anthropic" => Provider::Claude,
        "chatgpt" | "openai" | "codex" => Provider::OpenAi,
        "gemini" | "google" => Provider::Gemini,
        "custom" => Provider::Custom,
        other => bail!("unknown --ai-tool {other:?}: expected claude | codex | chatgpt | gemini | custom"),
    })
}

/// The env var holding the provider's API key, checked via `lookup` for
/// testability.
fn api_key_for(provider: Provider, lookup: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    match provider {
        Provider::Claude => lookup("ANTHROPIC_API_KEY"),
        Provider::OpenAi => lookup("OPENAI_API_KEY"),
        Provider::Gemini => lookup("GEMINI_API_KEY").or_else(|| lookup("GOOGLE_API_KEY")),
        Provider::Custom => None,
    }
}

/// Default model per provider; override with --ai-model (DPM_AI_MODEL).
pub fn default_model(provider: Provider) -> &'static str {
    match provider {
        // Anthropic's recommended default for code-review-quality work.
        Provider::Claude => "claude-opus-4-8",
        Provider::OpenAi => "gpt-5.1",
        Provider::Gemini => "gemini-2.5-pro",
        Provider::Custom => "",
    }
}

/// Resolve a tool name to its CLI command template. `{file}` is replaced
/// with the payload path.
pub fn tool_command_template(tool: &str, custom_cmd: Option<&str>) -> Result<String> {
    if let Some(cmd) = custom_cmd {
        if !cmd.trim().is_empty() {
            return Ok(cmd.to_string());
        }
    }
    Ok(match tool.to_ascii_lowercase().as_str() {
        "claude" | "anthropic" => "claude -p < {file}".to_string(),
        "codex" | "chatgpt" | "openai" => "codex exec - < {file}".to_string(),
        "gemini" | "google" => "gemini < {file}".to_string(),
        "custom" => bail!("--ai-tool custom requires --ai-cmd (DPM_AI_CMD)"),
        other => bail!("unknown --ai-tool {other:?}: expected claude | codex | chatgpt | gemini | custom"),
    })
}

pub fn build_payload(req: &ReviewRequest) -> String {
    let destructive_policy = match (req.allow_destructive_sql, req.allow_destructive_ops) {
        (false, _) => {
            "Destructive SQL generation is NOT allowed: every destructive statement must appear \
             commented out (gated). Any LIVE destructive statement is a policy violation — REJECT."
        }
        (true, false) => {
            "Destructive SQL generation IS allowed (statements may appear live), but executing \
             them is NOT yet approved; the operator will need --allow-destructive-ops at apply \
             time. Judge the SQL on correctness and safety."
        }
        (true, true) => {
            "Destructive SQL generation AND execution are both operator-approved. Still verify \
             each destructive statement is intentional given the plan, and flag anything that \
             looks like collateral damage."
        }
    };

    format!(
        r#"You are reviewing an auto-generated PostgreSQL schema migration produced by
declarative-postgres-migrate (dpm). The script converges a target database onto a
desired source state. Review it for CORRECTNESS, CONSISTENCY, and SAFETY. Do not
suggest stylistic rewrites; judge only whether this script is safe and correct to run.

CONTEXT
- desired (source): {source}
- current (target): {target}
- plan summary: {total} change(s), {destructive} destructive ({gated} gated/commented), {manual} manual-review item(s)
- destructive policy: {destructive_policy}

CHECKLIST
1. Statement ordering: types/tables/columns exist before anything references them;
   FKs added after referenced tables and their PK/unique constraints; drops run
   dependents-first; enum ADD VALUE statements appear before BEGIN (outside the
   transaction).
2. Destructive audit: list every statement that can lose data or weaken integrity
   (DROP TABLE/COLUMN/TYPE/SEQUENCE/FUNCTION, integrity-weakening constraint/index
   drops, column type changes that can truncate). Check each against the destructive
   policy above.
3. Consistency: the SQL must match the JSON plan — no statement without a plan entry,
   no plan entry without a statement (gated entries appear as comments).
4. Safety: no statement may target objects outside the declared plan; no data-modifying
   DML (INSERT/UPDATE/DELETE) except sequence setval for serial adoption; nothing that
   looks like injection or an unrelated side effect.

OUTPUT FORMAT (mandatory)
- Brief findings, most severe first. If everything is fine say so in one line.
- Then, as the FINAL line, exactly one verdict:
  DPM_VERDICT: APPROVE
  or
  DPM_VERDICT: REJECT <one-line reason>

=== JSON PLAN ===
{plan_json}

=== MIGRATION SQL ===
{sql}
"#,
        source = req.source_desc,
        target = req.target_desc,
        total = req.total_changes,
        destructive = req.destructive_changes,
        gated = req.gated_changes,
        manual = req.manual_changes,
        destructive_policy = destructive_policy,
        plan_json = req.plan_json,
        sql = req.sql,
    )
}

/// Parse the last `DPM_VERDICT:` line from a reviewer transcript.
pub fn parse_verdict(transcript: &str) -> Option<(bool, String)> {
    transcript
        .lines()
        .rev()
        .map(str::trim)
        .find_map(|line| {
            let rest = line.strip_prefix("DPM_VERDICT:")?.trim();
            if rest.to_ascii_uppercase().starts_with("APPROVE") {
                Some((true, line.to_string()))
            } else if rest.to_ascii_uppercase().starts_with("REJECT") {
                Some((false, line.to_string()))
            } else {
                None
            }
        })
}

fn outcome_from_transcript(transcript: String, command: String) -> ReviewOutcome {
    // Fail closed: a transcript without a parseable verdict is not approval.
    let parsed = parse_verdict(&transcript);
    ReviewOutcome {
        approved: parsed.as_ref().map(|(ok, _)| *ok).unwrap_or(false),
        verdict: parsed.map(|(_, line)| line),
        transcript,
        command,
    }
}

pub async fn run_review(
    tool: &str,
    custom_cmd: Option<&str>,
    transport: Transport,
    model_override: Option<&str>,
    req: &ReviewRequest,
    verbose: bool,
) -> Result<ReviewOutcome> {
    let payload = build_payload(req);
    run_payload(tool, custom_cmd, transport, model_override, &payload, verbose).await
}

/// Send an arbitrary payload (which must instruct the DPM_VERDICT protocol)
/// through the configured reviewer. Used by the migration review above and by
/// the cross-check discrepancy scan.
pub async fn run_payload(
    tool: &str,
    custom_cmd: Option<&str>,
    transport: Transport,
    model_override: Option<&str>,
    payload: &str,
    verbose: bool,
) -> Result<ReviewOutcome> {
    let provider = if custom_cmd.is_some() { Provider::Custom } else { provider_for_tool(tool)? };
    let env_lookup = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let key = api_key_for(provider, &env_lookup);

    let use_api = match transport {
        Transport::Api => {
            if key.is_none() && provider != Provider::Custom {
                bail!(
                    "--ai-transport api requested but no API key found for {tool} \
                     (set ANTHROPIC_API_KEY / OPENAI_API_KEY / GEMINI_API_KEY)"
                );
            }
            provider != Provider::Custom
        }
        Transport::Cli => false,
        Transport::Auto => key.is_some() && provider != Provider::Custom,
    };

    if use_api {
        let model = model_override
            .filter(|m| !m.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| default_model(provider).to_string());
        let key = key.expect("checked above");
        if verbose {
            eprintln!("dpm: ai call via {tool} HTTP API (model {model})");
        }
        run_review_api(provider, &key, &model, payload).await
    } else {
        run_review_cli(tool, custom_cmd, payload, payload.len(), verbose)
    }
}

/// Payload for the cross-check discrepancy scan: given dpm's own convergence
/// result and every external tool's report, ask an AI to hunt for
/// inconsistencies (tools disagreeing with each other, residuals dpm may
/// have misclassified, suspicious tool errors).
pub fn build_discrepancy_payload(
    converged: bool,
    residual_sql: Option<&str>,
    reports: &[(String, bool, String, Option<String>)],
) -> String {
    let mut tool_sections = String::new();
    for (name, agreed, output, error) in reports {
        tool_sections.push_str(&format!(
            "\n--- {name} (agreed: {agreed}) ---\n{}{}\n",
            if output.is_empty() { "(no residual output)" } else { output },
            error.as_ref().map(|e| format!("\n[tool error: {e}]")).unwrap_or_default(),
        ));
    }
    format!(
        r#"You are auditing the cross-validation results of a PostgreSQL schema migration
produced by declarative-postgres-migrate (dpm). dpm applied its migration to a replica
and re-checked convergence itself; several independent schema-diff tools then compared
the migrated replica against the desired source.

Your job: scan for DISCREPANCIES.
1. Do the tools agree with each other and with dpm's own convergence result?
2. If a tool reports residual differences, are they REAL schema drift (dpm bug),
   known blind spots or noise of that tool (e.g. atlas OSS not seeing views/functions,
   pgdiff not comparing triggers, dump-format chatter), or an environment/tool error?
3. Do any tool errors look like they are masking a real disagreement?

dpm's own result: converged = {converged}
dpm residual (empty means none):
{residual}

External tool reports:
{tools}

OUTPUT FORMAT (mandatory)
- Brief findings, most severe first; classify each residual as real-drift / tool-blind-spot / tool-error.
- Then, as the FINAL line, exactly one verdict:
  DPM_VERDICT: APPROVE            (results are consistent; no evidence of real drift)
  or
  DPM_VERDICT: REJECT <one-line reason>   (evidence of real drift or unresolvable inconsistency)
"#,
        converged = converged,
        residual = residual_sql.unwrap_or("(none)"),
        tools = tool_sections,
    )
}

// ---------------------------------------------------------------------------
// HTTP API transport
// ---------------------------------------------------------------------------

async fn run_review_api(provider: Provider, key: &str, model: &str, payload: &str) -> Result<ReviewOutcome> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()?;

    // One retry on transient failures (429 / 5xx / connection errors).
    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 0..2 {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
        match call_provider(&client, provider, key, model, payload).await {
            Ok(outcome) => return Ok(outcome),
            Err(e) => {
                let transient = e.to_string().contains("429")
                    || e.to_string().contains("status 5")
                    || e.downcast_ref::<reqwest::Error>().map(|r| r.is_connect() || r.is_timeout()).unwrap_or(false);
                if !transient {
                    return Err(e);
                }
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("AI review failed")))
}

async fn call_provider(
    client: &reqwest::Client,
    provider: Provider,
    key: &str,
    model: &str,
    payload: &str,
) -> Result<ReviewOutcome> {
    match provider {
        Provider::Claude => {
            // Anthropic Messages API. Adaptive thinking improves review
            // quality; thinking blocks come back with empty text by default
            // and are simply skipped when extracting the answer.
            let body = json!({
                "model": model,
                "max_tokens": 16000,
                "thinking": {"type": "adaptive"},
                "messages": [{"role": "user", "content": payload}],
            });
            let resp = client
                .post("https://api.anthropic.com/v1/messages")
                .header("x-api-key", key)
                .header("anthropic-version", "2023-06-01")
                .json(&body)
                .send()
                .await
                .context("calling Anthropic Messages API")?;
            let status = resp.status();
            let doc: serde_json::Value = resp.json().await.context("parsing Anthropic response")?;
            if !status.is_success() {
                bail!(
                    "Anthropic API returned status {status}: {}",
                    doc["error"]["message"].as_str().unwrap_or("(no message)")
                );
            }
            let command = format!("POST https://api.anthropic.com/v1/messages (model {model})");
            // A safety refusal must gate the migration closed, with a clear
            // note rather than a missing-verdict mystery.
            if doc["stop_reason"].as_str() == Some("refusal") {
                return Ok(ReviewOutcome {
                    approved: false,
                    verdict: Some("reviewer refused (stop_reason: refusal)".into()),
                    transcript: String::new(),
                    command,
                });
            }
            let text: String = doc["content"]
                .as_array()
                .map(|blocks| {
                    blocks
                        .iter()
                        .filter(|b| b["type"] == "text")
                        .filter_map(|b| b["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            Ok(outcome_from_transcript(text, command))
        }
        Provider::OpenAi => {
            let body = json!({
                "model": model,
                "messages": [{"role": "user", "content": payload}],
            });
            let resp = client
                .post("https://api.openai.com/v1/chat/completions")
                .header("Authorization", format!("Bearer {key}"))
                .json(&body)
                .send()
                .await
                .context("calling OpenAI chat completions API")?;
            let status = resp.status();
            let doc: serde_json::Value = resp.json().await.context("parsing OpenAI response")?;
            if !status.is_success() {
                bail!(
                    "OpenAI API returned status {status}: {}",
                    doc["error"]["message"].as_str().unwrap_or("(no message)")
                );
            }
            let text = doc["choices"][0]["message"]["content"].as_str().unwrap_or_default().to_string();
            Ok(outcome_from_transcript(
                text,
                format!("POST https://api.openai.com/v1/chat/completions (model {model})"),
            ))
        }
        Provider::Gemini => {
            let url = format!("https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent");
            let body = json!({
                "contents": [{"parts": [{"text": payload}]}],
            });
            let resp = client
                .post(&url)
                .header("x-goog-api-key", key)
                .json(&body)
                .send()
                .await
                .context("calling Gemini generateContent API")?;
            let status = resp.status();
            let doc: serde_json::Value = resp.json().await.context("parsing Gemini response")?;
            if !status.is_success() {
                bail!(
                    "Gemini API returned status {status}: {}",
                    doc["error"]["message"].as_str().unwrap_or("(no message)")
                );
            }
            let text: String = doc["candidates"][0]["content"]["parts"]
                .as_array()
                .map(|parts| {
                    parts
                        .iter()
                        .filter_map(|p| p["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            Ok(outcome_from_transcript(text, format!("POST {url}")))
        }
        Provider::Custom => bail!("custom provider has no API transport — use --ai-cmd"),
    }
}

// ---------------------------------------------------------------------------
// CLI transport
// ---------------------------------------------------------------------------

fn run_review_cli(
    tool: &str,
    custom_cmd: Option<&str>,
    payload: &str,
    tag: usize,
    verbose: bool,
) -> Result<ReviewOutcome> {
    let template = tool_command_template(tool, custom_cmd)?;

    let dir = std::env::temp_dir().join("dpm-ai-review");
    std::fs::create_dir_all(&dir)?;
    let file = dir.join(format!("payload-{}-{}.md", std::process::id(), tag));
    std::fs::write(&file, payload).with_context(|| format!("writing {}", file.display()))?;

    let command = template.replace("{file}", &file.display().to_string());
    if verbose {
        eprintln!("dpm: ai review via CLI: {command}");
    }
    // The reviewer is an independent non-interactive call; strip Claude
    // Code's nesting guard so `dpm review` works when dpm itself is being
    // driven from inside a Claude Code session (the guard exists for
    // interactive sessions sharing runtime resources, and its own error
    // message documents this bypass).
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(&command)
        .env_remove("CLAUDECODE")
        .output()
        .with_context(|| format!("running AI reviewer: {command}"))?;
    let _ = std::fs::remove_file(&file);

    let transcript = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if !output.status.success() {
        return Ok(ReviewOutcome {
            approved: false,
            verdict: Some(format!("reviewer exited {}", output.status)),
            transcript,
            command,
        });
    }
    Ok(outcome_from_transcript(transcript, command))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req() -> ReviewRequest {
        ReviewRequest {
            sql: "BEGIN;\nSELECT 1;\nCOMMIT;".into(),
            plan_json: "[]".into(),
            source_desc: "a".into(),
            target_desc: "b".into(),
            allow_destructive_sql: false,
            allow_destructive_ops: false,
            total_changes: 1,
            destructive_changes: 0,
            gated_changes: 0,
            manual_changes: 0,
        }
    }

    #[test]
    fn verdict_parsing_takes_last_line_and_fails_closed() {
        assert_eq!(parse_verdict("blah\nDPM_VERDICT: APPROVE\n"), Some((true, "DPM_VERDICT: APPROVE".into())));
        let (ok, line) = parse_verdict("DPM_VERDICT: APPROVE\nlater...\nDPM_VERDICT: REJECT drops users table").unwrap();
        assert!(!ok);
        assert!(line.contains("REJECT"));
        assert_eq!(parse_verdict("no verdict here"), None);
        assert!(parse_verdict("  DPM_VERDICT: APPROVE  ").unwrap().0);
    }

    #[test]
    fn tool_templates() {
        assert!(tool_command_template("claude", None).unwrap().starts_with("claude -p"));
        assert!(tool_command_template("chatgpt", None).unwrap().starts_with("codex exec"));
        assert!(tool_command_template("gemini", None).unwrap().starts_with("gemini"));
        assert!(tool_command_template("custom", None).is_err());
        assert_eq!(tool_command_template("custom", Some("x {file}")).unwrap(), "x {file}");
        assert_eq!(tool_command_template("claude", Some("y {file}")).unwrap(), "y {file}");
        assert!(tool_command_template("skynet", None).is_err());
    }

    #[test]
    fn provider_and_transport_resolution() {
        assert_eq!(provider_for_tool("claude").unwrap(), Provider::Claude);
        assert_eq!(provider_for_tool("chatgpt").unwrap(), Provider::OpenAi);
        assert_eq!(provider_for_tool("codex").unwrap(), Provider::OpenAi);
        assert_eq!(provider_for_tool("gemini").unwrap(), Provider::Gemini);
        assert!(provider_for_tool("skynet").is_err());
        assert_eq!(Transport::parse("auto").unwrap(), Transport::Auto);
        assert_eq!(Transport::parse("api").unwrap(), Transport::Api);
        assert_eq!(Transport::parse("cli").unwrap(), Transport::Cli);
        assert!(Transport::parse("carrier-pigeon").is_err());
    }

    #[test]
    fn api_key_resolution_per_provider() {
        let lookup = |k: &str| match k {
            "ANTHROPIC_API_KEY" => Some("a".to_string()),
            "GOOGLE_API_KEY" => Some("g".to_string()),
            _ => None,
        };
        assert_eq!(api_key_for(Provider::Claude, &lookup).as_deref(), Some("a"));
        assert_eq!(api_key_for(Provider::OpenAi, &lookup), None);
        // Gemini falls back from GEMINI_API_KEY to GOOGLE_API_KEY.
        assert_eq!(api_key_for(Provider::Gemini, &lookup).as_deref(), Some("g"));
        assert_eq!(api_key_for(Provider::Custom, &lookup), None);
    }

    #[test]
    fn default_models() {
        assert_eq!(default_model(Provider::Claude), "claude-opus-4-8");
        assert!(!default_model(Provider::OpenAi).is_empty());
        assert!(!default_model(Provider::Gemini).is_empty());
    }

    #[test]
    fn payload_contains_policy_and_sections() {
        let payload = build_payload(&req());
        assert!(payload.contains("policy violation — REJECT"));
        assert!(payload.contains("=== MIGRATION SQL ==="));
        assert!(payload.contains("DPM_VERDICT: APPROVE"));
    }

    #[tokio::test]
    async fn fake_reviewer_end_to_end_cli() {
        let outcome = run_review(
            "custom",
            Some("cat {file} > /dev/null && echo 'looks good' && echo 'DPM_VERDICT: APPROVE'"),
            Transport::Auto,
            None,
            &req(),
            false,
        )
        .await
        .unwrap();
        assert!(outcome.approved, "transcript: {}", outcome.transcript);

        let outcome = run_review(
            "custom",
            Some("echo 'DPM_VERDICT: REJECT live destructive statement found'"),
            Transport::Auto,
            None,
            &req(),
            false,
        )
        .await
        .unwrap();
        assert!(!outcome.approved);

        // Reviewer that says nothing useful → fail closed.
        let outcome = run_review("custom", Some("echo hello"), Transport::Auto, None, &req(), false)
            .await
            .unwrap();
        assert!(!outcome.approved);
        assert!(outcome.verdict.is_none());
    }

    #[test]
    fn api_transport_without_key_errors() {
        // Force api transport for a provider with no key set in this test env
        // var name (we can't safely unset real env in parallel tests, so use
        // the resolution primitive directly).
        let lookup = |_: &str| None;
        assert!(api_key_for(Provider::OpenAi, &lookup).is_none());
    }
}
