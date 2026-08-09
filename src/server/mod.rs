//! Hardened HTTP service for declarative migration planning and application.
//!
//! The server is intentionally small and dependency-light. It accepts only
//! versioned JSON requests, closes every HTTP/1.1 connection after one request,
//! and never accepts database URLs from callers. Operators configure aliases
//! through `DPM_SERVER_DATABASES_JSON`.

mod http;

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{bail, Context, Result as AnyResult};
use serde::de::DeserializeOwned;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, Semaphore};
use tokio::task::LocalSet;

use crate::interfaces::{
    ApiError, ApplyRequest, ApplyResponse, CatalogSource, DiffRequest, DiffResponse, ErrorResponse,
    HealthResponse, MigrationSummary, ReadyResponse, VersionResponse, API_VERSION, APPLY_PATH,
    DIFF_PATH, HEALTH_PATH, OPENAPI_JSON, OPENAPI_PATH, READY_PATH, VERSION_PATH,
};
use crate::lease::{PostgresMigrationLease, ValidatedScript, DEFAULT_MIGRATION_LOCK_KEY};
use crate::model::{Catalog, DatabaseFlavor, CATALOG_FORMAT_VERSION};
use crate::{diff, emit, introspect_url, EmitOptions, IntrospectOptions};

use self::http::{read_request, write_response, Request, Response};

const DEFAULT_BIND: &str = "127.0.0.1:8080";
const DEFAULT_MAX_BODY_BYTES: usize = 1024 * 1024;
const MAX_MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
const DEFAULT_MAX_IN_FLIGHT: usize = 64;
const MAX_DATABASE_ALIASES: usize = 256;

#[derive(Clone)]
pub struct ServerConfig {
    pub bind: SocketAddr,
    pub bearer_token: Option<String>,
    pub databases: BTreeMap<String, String>,
    pub allow_apply: bool,
    pub max_body_bytes: usize,
    pub max_in_flight: usize,
}

impl ServerConfig {
    pub fn from_env() -> AnyResult<Self> {
        let bind = std::env::var("DPM_SERVER_BIND")
            .unwrap_or_else(|_| DEFAULT_BIND.to_string())
            .parse::<SocketAddr>()
            .context("DPM_SERVER_BIND must be an IP socket address such as 127.0.0.1:8080")?;
        let bearer_token = std::env::var("DPM_SERVER_TOKEN")
            .ok()
            .filter(|value| !value.trim().is_empty());
        let databases = match std::env::var("DPM_SERVER_DATABASES_JSON") {
            Ok(value) if !value.trim().is_empty() => serde_json::from_str(&value)
                .context("DPM_SERVER_DATABASES_JSON must be a JSON object of alias to URL")?,
            _ => BTreeMap::new(),
        };
        let allow_apply = env_bool("DPM_SERVER_ALLOW_APPLY", false)?;
        let max_body_bytes = env_usize("DPM_SERVER_MAX_BODY_BYTES", DEFAULT_MAX_BODY_BYTES)?;
        let max_in_flight = env_usize("DPM_SERVER_MAX_IN_FLIGHT", DEFAULT_MAX_IN_FLIGHT)?;
        Self {
            bind,
            bearer_token,
            databases,
            allow_apply,
            max_body_bytes,
            max_in_flight,
        }
        .validate()
    }

    pub fn validate(self) -> AnyResult<Self> {
        if !self.bind.ip().is_loopback() && self.bearer_token.is_none() {
            bail!("DPM_SERVER_TOKEN is required when DPM_SERVER_BIND is not loopback");
        }
        if let Some(token) = &self.bearer_token {
            if token.len() < 24 {
                bail!("DPM_SERVER_TOKEN must contain at least 24 bytes");
            }
        }
        if self.databases.len() > MAX_DATABASE_ALIASES {
            bail!("at most {MAX_DATABASE_ALIASES} database aliases are supported");
        }
        for (name, url) in &self.databases {
            validate_alias(name)?;
            let lower = url.to_ascii_lowercase();
            if !lower.starts_with("postgres://") && !lower.starts_with("postgresql://") {
                bail!("database alias {name:?} must use a postgres URL");
            }
        }
        if self.max_body_bytes == 0 || self.max_body_bytes > MAX_MAX_BODY_BYTES {
            bail!("DPM_SERVER_MAX_BODY_BYTES must be between 1 and {MAX_MAX_BODY_BYTES}");
        }
        if self.max_in_flight == 0 || self.max_in_flight > 4096 {
            bail!("DPM_SERVER_MAX_IN_FLIGHT must be between 1 and 4096");
        }
        Ok(self)
    }
}

fn env_bool(name: &str, default: bool) -> AnyResult<bool> {
    let Ok(value) = std::env::var(name) else {
        return Ok(default);
    };
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => bail!("{name} must be true or false"),
    }
}

fn env_usize(name: &str, default: usize) -> AnyResult<usize> {
    match std::env::var(name) {
        Ok(value) => value
            .parse::<usize>()
            .with_context(|| format!("{name} must be a positive integer")),
        Err(_) => Ok(default),
    }
}

fn validate_alias(name: &str) -> AnyResult<()> {
    if name.is_empty() || name.len() > 64 {
        bail!("database aliases must contain 1 to 64 characters");
    }
    if !name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        bail!("database alias {name:?} contains unsupported characters");
    }
    Ok(())
}

struct ServerState {
    config: ServerConfig,
    apply_lock: Mutex<()>,
    request_counter: AtomicU64,
}

impl ServerState {
    fn next_request_id(&self) -> String {
        let value = self.request_counter.fetch_add(1, Ordering::Relaxed);
        format!("dpm-{}-{value}", std::process::id())
    }
}

pub async fn run(config: ServerConfig) -> AnyResult<()> {
    let config = config.validate()?;
    let listener = TcpListener::bind(config.bind)
        .await
        .with_context(|| format!("binding dpm-server to {}", config.bind))?;
    let local = listener.local_addr().context("reading bound address")?;
    eprintln!(
        "dpm-server: listening on {local}; aliases={}; apply_enabled={}",
        config.databases.len(),
        config.allow_apply
    );
    serve(listener, config).await
}

pub async fn serve(listener: TcpListener, config: ServerConfig) -> AnyResult<()> {
    let tasks = LocalSet::new();
    tasks.run_until(serve_local(listener, config)).await
}

async fn serve_local(listener: TcpListener, config: ServerConfig) -> AnyResult<()> {
    let state = Rc::new(ServerState {
        config: config.validate()?,
        apply_lock: Mutex::new(()),
        request_counter: AtomicU64::new(1),
    });
    let limiter = Arc::new(Semaphore::new(state.config.max_in_flight));

    loop {
        tokio::select! {
            result = listener.accept() => {
                let (stream, _) = result.context("accepting HTTP connection")?;
                let permit = limiter
                    .clone()
                    .acquire_owned()
                    .await
                    .context("server concurrency limiter closed")?;
                let state = Rc::clone(&state);
                tokio::task::spawn_local(async move {
                    let _permit = permit;
                    if let Err(error) = handle_connection(stream, state).await {
                        eprintln!("dpm-server: connection error: {error:#}");
                    }
                });
            }
            result = tokio::signal::ctrl_c() => {
                result.context("installing shutdown signal handler")?;
                eprintln!("dpm-server: shutdown requested");
                return Ok(());
            }
        }
    }
}

async fn handle_connection(mut stream: TcpStream, state: Rc<ServerState>) -> AnyResult<()> {
    let request_id = state.next_request_id();
    let request = match read_request(&mut stream, state.config.max_body_bytes).await {
        Ok(request) => request,
        Err(error) => {
            let response = error_response(error.status, error.code, error.message, &request_id);
            write_response(&mut stream, response).await?;
            return Ok(());
        }
    };
    let method = request.method.clone();
    let path = request.path.clone();
    let response = route(request, &state, &request_id).await;
    let status = response.status;
    write_response(&mut stream, response).await?;
    eprintln!("dpm-server: {method} {path} -> {status} request_id={request_id}");
    Ok(())
}

async fn route(request: Request, state: &ServerState, request_id: &str) -> Response {
    let response = match (request.method.as_str(), request.path.as_str()) {
        ("GET", HEALTH_PATH) => Response::json(200, &health_response()),
        ("GET", READY_PATH) => Response::json(
            200,
            &ReadyResponse {
                status: "ready".to_string(),
                configured_database_aliases: state.config.databases.len(),
                apply_enabled: state.config.allow_apply,
            },
        ),
        ("GET", VERSION_PATH) => Response::json(200, &version_response()),
        ("GET", OPENAPI_PATH) => Response::json_bytes(200, OPENAPI_JSON.as_bytes()),
        ("POST", DIFF_PATH) => {
            if let Err(response) = authorize(&request, state, request_id) {
                response
            } else if let Err(response) = require_json(&request, request_id) {
                response
            } else {
                handle_diff(&request.body, state, request_id).await
            }
        }
        ("POST", APPLY_PATH) => {
            if let Err(response) = authorize(&request, state, request_id) {
                response
            } else if let Err(response) = require_json(&request, request_id) {
                response
            } else {
                handle_apply(&request.body, state, request_id).await
            }
        }
        (_, HEALTH_PATH | READY_PATH | VERSION_PATH | OPENAPI_PATH | DIFF_PATH | APPLY_PATH) => {
            error_response(
                405,
                "method_not_allowed",
                "method not allowed for this endpoint",
                request_id,
            )
            .with_header("Allow", allowed_methods(&request.path))
        }
        _ => error_response(404, "not_found", "endpoint not found", request_id),
    };
    response.with_header("X-Request-Id", request_id)
}

fn allowed_methods(path: &str) -> &'static str {
    match path {
        DIFF_PATH | APPLY_PATH => "POST",
        _ => "GET",
    }
}

fn health_response() -> HealthResponse {
    HealthResponse {
        status: "ok".to_string(),
        service: "dpm-server".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        api_version: API_VERSION.to_string(),
    }
}

fn version_response() -> VersionResponse {
    VersionResponse {
        service: "dpm-server".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        api_version: API_VERSION.to_string(),
        catalog_format_version: CATALOG_FORMAT_VERSION,
    }
}

fn authorize(request: &Request, state: &ServerState, request_id: &str) -> Result<(), Response> {
    let Some(expected) = &state.config.bearer_token else {
        return Ok(());
    };
    let supplied = request
        .headers
        .get("authorization")
        .and_then(|value| value.strip_prefix("Bearer "));
    if supplied.is_some_and(|value| constant_time_eq(value.as_bytes(), expected.as_bytes())) {
        return Ok(());
    }
    Err(error_response(
        401,
        "unauthorized",
        "a valid bearer token is required",
        request_id,
    )
    .with_header("WWW-Authenticate", "Bearer"))
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    let max_len = left.len().max(right.len());
    for index in 0..max_len {
        let left_byte = left.get(index).copied().unwrap_or(0);
        let right_byte = right.get(index).copied().unwrap_or(0);
        difference |= usize::from(left_byte ^ right_byte);
    }
    difference == 0
}

fn require_json(request: &Request, request_id: &str) -> Result<(), Response> {
    let valid = request
        .headers
        .get("content-type")
        .is_some_and(|value| value.to_ascii_lowercase().starts_with("application/json"));
    if valid {
        Ok(())
    } else {
        Err(error_response(
            415,
            "unsupported_media_type",
            "Content-Type must be application/json",
            request_id,
        ))
    }
}

async fn handle_diff(body: &[u8], state: &ServerState, request_id: &str) -> Response {
    let request = match parse_json::<DiffRequest>(body, request_id) {
        Ok(request) => request,
        Err(response) => return response,
    };
    match compute_migration(
        &request.source,
        &request.target,
        request.allow_destructive,
        state,
    )
    .await
    {
        Ok(computed) => Response::json(200, &computed.into_diff_response()),
        Err(failure) => failure.into_response(request_id),
    }
}

async fn handle_apply(body: &[u8], state: &ServerState, request_id: &str) -> Response {
    let request = match parse_json::<ApplyRequest>(body, request_id) {
        Ok(request) => request,
        Err(response) => return response,
    };
    if let Err(error) = validate_alias(&request.target) {
        return error_response(422, "invalid_database_alias", error.to_string(), request_id);
    }
    if !state.config.databases.contains_key(&request.target) {
        return error_response(
            404,
            "database_alias_not_found",
            format!("database alias {:?} is not configured", request.target),
            request_id,
        );
    }

    if request.dry_run {
        let target = CatalogSource::Database {
            name: request.target.clone(),
        };
        return match compute_migration(&request.source, &target, request.allow_destructive, state)
            .await
        {
            Ok(computed) => Response::json(200, &computed.into_apply_response(true, 0, 0)),
            Err(failure) => failure.into_response(request_id),
        };
    }

    if !state.config.allow_apply {
        return error_response(
            503,
            "apply_disabled",
            "live apply is disabled; set DPM_SERVER_ALLOW_APPLY=true on the server",
            request_id,
        );
    }
    let expected_confirmation = if request.allow_destructive {
        format!("apply-destructive:{}", request.target)
    } else {
        format!("apply:{}", request.target)
    };
    if request.confirmation.as_deref() != Some(expected_confirmation.as_str()) {
        return error_response(
            422,
            "confirmation_required",
            format!("confirmation must equal {expected_confirmation:?}"),
            request_id,
        );
    }

    let _guard = state.apply_lock.lock().await;
    let target = CatalogSource::Database {
        name: request.target.clone(),
    };
    let mut computed =
        match compute_migration(&request.source, &target, request.allow_destructive, state).await {
            Ok(computed) => computed,
            Err(failure) => return failure.into_response(request_id),
        };
    if let Err(failure) = validate_live_plan(&computed) {
        return failure.into_response(request_id);
    }
    if computed.summary.change_count == 0 {
        return Response::json(200, &computed.into_apply_response(false, 0, 0));
    }

    let Some(target_url) = state.config.databases.get(&request.target) else {
        return error_response(
            404,
            "database_alias_not_found",
            "database alias disappeared from server configuration",
            request_id,
        );
    };

    // Preserve current-main's owned execution boundary. PostgreSQL mutation
    // holds the cross-process advisory lease while re-introspecting, applying,
    // and verifying. The request task owns the non-Send SQLx state, while the
    // listener remains free to service other bounded local tasks.
    let mut lease = if computed.source_catalog.database_flavor == DatabaseFlavor::Postgres {
        let owner = format!("dpm-server:{}:{request_id}", std::process::id());
        match PostgresMigrationLease::acquire(target_url, DEFAULT_MIGRATION_LOCK_KEY, owner).await {
            Ok(lease) => Some(lease),
            Err(error) => {
                return Failure::logged(
                    503,
                    "migration_lease_unavailable",
                    "the PostgreSQL migration execution lease is unavailable",
                    error,
                )
                .into_response(request_id);
            }
        }
    } else {
        None
    };

    if lease.is_some() {
        computed =
            match compute_migration(&request.source, &target, request.allow_destructive, state)
                .await
            {
                Ok(computed) => computed,
                Err(failure) => return failure.into_response(request_id),
            };
        if let Err(failure) = validate_live_plan(&computed) {
            return failure.into_response(request_id);
        }
        if computed.summary.change_count == 0 {
            if let Err(failure) = release_migration_lease(lease, request_id).await {
                return failure.into_response(request_id);
            }
            return Response::json(200, &computed.into_apply_response(false, 0, 0));
        }
    }

    let report_result = match lease.as_mut() {
        Some(lease) => match ValidatedScript::parse(&computed.sql) {
            Ok(script) => lease.apply(&script).await,
            Err(error) => Err(error.context("validating the leased migration script")),
        },
        None => crate::apply::apply_script(target_url, &computed.sql).await,
    };
    let report = match report_result {
        Ok(report) => report,
        Err(error) => {
            eprintln!("dpm-server: apply failed request_id={request_id}: {error:#}");
            return error_response(
                500,
                "apply_failed",
                "the migration failed; inspect server logs using the request ID",
                request_id,
            );
        }
    };

    let after = match introspect_alias(&request.target, state).await {
        Ok(catalog) => catalog,
        Err(failure) => return failure.into_response(request_id),
    };
    let verification = diff(&computed.source_catalog, &after);
    let remaining = verification.changes.len();
    if remaining > 0 {
        return error_response(
            500,
            "verification_failed",
            format!("post-apply verification found {remaining} remaining changes"),
            request_id,
        );
    }

    if let Err(failure) = release_migration_lease(lease, request_id).await {
        return failure.into_response(request_id);
    }

    Response::json(
        200,
        &computed.into_apply_response(false, report.executed, remaining),
    )
}

fn validate_live_plan(computed: &ComputedMigration) -> Result<(), Failure> {
    if computed.summary.manual_count > 0 {
        return Err(Failure::safe(
            409,
            "manual_changes_present",
            "the migration contains manual changes and cannot be applied automatically",
        ));
    }
    if computed.summary.gated_count > 0 {
        return Err(Failure::safe(
            409,
            "destructive_changes_gated",
            "the migration contains destructive changes; explicitly allow and confirm them",
        ));
    }
    Ok(())
}

async fn release_migration_lease(
    lease: Option<PostgresMigrationLease>,
    request_id: &str,
) -> Result<(), Failure> {
    let Some(lease) = lease else {
        return Ok(());
    };
    let receipt = lease.release().await.map_err(|error| {
        Failure::logged(
            500,
            "migration_lease_release_failed",
            "the migration completed but its execution lease could not be released cleanly",
            error,
        )
    })?;
    let fingerprint = receipt
        .last_script_fingerprint()
        .map(|value| format!("{value:016x}"))
        .unwrap_or_else(|| "none".to_string());
    eprintln!(
        "dpm-server: released migration lease {} (owner {}, statements {}, fingerprint {}, request_id={request_id})",
        receipt.key(),
        receipt.owner(),
        receipt.executed(),
        fingerprint
    );
    Ok(())
}

fn parse_json<T: DeserializeOwned>(body: &[u8], request_id: &str) -> Result<T, Response> {
    serde_json::from_slice(body).map_err(|error| {
        error_response(
            400,
            "invalid_json",
            format!(
                "invalid JSON request at line {}, column {}",
                error.line(),
                error.column()
            ),
            request_id,
        )
    })
}

struct ComputedMigration {
    source_catalog: Catalog,
    source: String,
    target: String,
    database_flavor: String,
    plan: serde_json::Value,
    sql: String,
    summary: MigrationSummary,
}

impl ComputedMigration {
    fn into_diff_response(self) -> DiffResponse {
        DiffResponse {
            api_version: API_VERSION.to_string(),
            source: self.source,
            target: self.target,
            database_flavor: self.database_flavor,
            plan: self.plan,
            sql: self.sql,
            summary: self.summary,
        }
    }

    fn into_apply_response(
        self,
        dry_run: bool,
        executed_statements: usize,
        verification_remaining_changes: usize,
    ) -> ApplyResponse {
        ApplyResponse {
            api_version: API_VERSION.to_string(),
            source: self.source,
            target: self.target,
            database_flavor: self.database_flavor,
            dry_run,
            applied: !dry_run && executed_statements > 0,
            executed_statements,
            verification_remaining_changes,
            plan: self.plan,
            sql: self.sql,
            summary: self.summary,
        }
    }
}

async fn compute_migration(
    source_input: &CatalogSource,
    target_input: &CatalogSource,
    allow_destructive: bool,
    state: &ServerState,
) -> Result<ComputedMigration, Failure> {
    let (source_catalog, source) = resolve_catalog(source_input, state).await?;
    let (target_catalog, target) = resolve_catalog(target_input, state).await?;
    if source_catalog.database_flavor != target_catalog.database_flavor {
        return Err(Failure::safe(
            409,
            "database_flavor_mismatch",
            "source and target database flavors differ",
        ));
    }
    let plan = diff(&source_catalog, &target_catalog);
    let script = emit(
        &plan,
        &EmitOptions {
            allow_destructive,
            database_flavor: source_catalog.database_flavor,
            source_desc: Some(source.clone()),
            target_desc: Some(target.clone()),
        },
    );
    let plan = serde_json::to_value(&plan)
        .map_err(|error| Failure::internal("serializing migration plan", error))?;
    let database_flavor = source_catalog.database_flavor.label().to_string();
    Ok(ComputedMigration {
        source_catalog,
        source,
        target,
        database_flavor,
        plan,
        sql: script.sql,
        summary: MigrationSummary {
            change_count: script.change_count,
            destructive_count: script.destructive_count,
            gated_count: script.gated_count,
            manual_count: script.manual_count,
        },
    })
}

async fn resolve_catalog(
    input: &CatalogSource,
    state: &ServerState,
) -> Result<(Catalog, String), Failure> {
    match input {
        CatalogSource::Catalog { catalog } => {
            validate_catalog(catalog)?;
            Ok((
                catalog.clone(),
                format!("inline catalog ({} objects)", catalog.object_count()),
            ))
        }
        CatalogSource::Database { name } => {
            let catalog = introspect_alias(name, state).await?;
            Ok((catalog, format!("database alias {name}")))
        }
    }
}

fn validate_catalog(catalog: &Catalog) -> Result<(), Failure> {
    if catalog.format_version != CATALOG_FORMAT_VERSION {
        return Err(Failure::safe(
            422,
            "unsupported_catalog_format",
            format!(
                "catalog format {} is unsupported; expected {}",
                catalog.format_version, CATALOG_FORMAT_VERSION
            ),
        ));
    }
    Ok(())
}

async fn introspect_alias(name: &str, state: &ServerState) -> Result<Catalog, Failure> {
    validate_alias(name)
        .map_err(|error| Failure::safe(422, "invalid_database_alias", error.to_string()))?;
    let Some(url) = state.config.databases.get(name) else {
        return Err(Failure::safe(
            404,
            "database_alias_not_found",
            format!("database alias {name:?} is not configured"),
        ));
    };
    introspect_url(url, &IntrospectOptions::default())
        .await
        .map_err(|error| {
            Failure::logged(
                503,
                "database_unavailable",
                format!("database alias {name:?} could not be introspected"),
                error,
            )
        })
}

#[derive(Debug)]
struct Failure {
    status: u16,
    code: &'static str,
    message: String,
    log: Option<String>,
}

impl Failure {
    fn safe(status: u16, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            log: None,
        }
    }

    fn logged(
        status: u16,
        code: &'static str,
        message: impl Into<String>,
        error: impl std::fmt::Display,
    ) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            log: Some(error.to_string()),
        }
    }

    fn internal(context: &'static str, error: impl std::fmt::Display) -> Self {
        Self::logged(
            500,
            "internal_error",
            "an internal server error occurred",
            format!("{context}: {error}"),
        )
    }

    fn into_response(self, request_id: &str) -> Response {
        if let Some(log) = self.log {
            eprintln!("dpm-server: request_id={request_id} {}: {log}", self.code);
        }
        error_response(self.status, self.code, self.message, request_id)
    }
}

fn error_response(
    status: u16,
    code: impl Into<String>,
    message: impl Into<String>,
    request_id: &str,
) -> Response {
    Response::json(
        status,
        &ErrorResponse {
            error: ApiError {
                code: code.into(),
                message: message.into(),
                request_id: request_id.to_string(),
            },
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> ServerConfig {
        ServerConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            bearer_token: None,
            databases: BTreeMap::new(),
            allow_apply: false,
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            max_in_flight: 4,
        }
    }

    fn test_state(config: ServerConfig) -> ServerState {
        ServerState {
            config,
            apply_lock: Mutex::new(()),
            request_counter: AtomicU64::new(1),
        }
    }

    #[test]
    fn external_bind_requires_authentication() {
        let mut config = test_config();
        config.bind = "0.0.0.0:8080".parse().unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn aliases_are_strict_and_urls_are_postgres_only() {
        assert!(validate_alias("primary-us_1.prod").is_ok());
        assert!(validate_alias("../../secret").is_err());
        let mut config = test_config();
        config
            .databases
            .insert("primary".to_string(), "https://example.com".to_string());
        assert!(config.validate().is_err());
    }

    #[test]
    fn token_compare_handles_different_lengths() {
        assert!(constant_time_eq(b"same", b"same"));
        assert!(!constant_time_eq(b"same", b"different"));
        assert!(!constant_time_eq(b"short", b"shorter"));
    }

    #[tokio::test]
    async fn inline_empty_catalogs_produce_an_empty_plan() {
        let state = test_state(test_config());
        let request = DiffRequest {
            source: CatalogSource::Catalog {
                catalog: Catalog::empty_with_schemas(Vec::<String>::new()),
            },
            target: CatalogSource::Catalog {
                catalog: Catalog::empty_with_schemas(Vec::<String>::new()),
            },
            allow_destructive: false,
        };
        let computed = compute_migration(
            &request.source,
            &request.target,
            request.allow_destructive,
            &state,
        )
        .await
        .unwrap();
        assert_eq!(computed.summary.change_count, 0);
        assert_eq!(computed.plan["changes"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn live_apply_is_disabled_before_database_access() {
        let mut config = test_config();
        config.databases.insert(
            "primary".to_string(),
            "postgres://localhost/unused".to_string(),
        );
        let state = test_state(config);
        let request = ApplyRequest {
            source: CatalogSource::Catalog {
                catalog: Catalog::empty_with_schemas(Vec::<String>::new()),
            },
            target: "primary".to_string(),
            dry_run: false,
            allow_destructive: false,
            confirmation: Some("apply:primary".to_string()),
        };
        let response = handle_apply(
            &serde_json::to_vec(&request).unwrap(),
            &state,
            "test-request",
        )
        .await;
        assert_eq!(response.status, 503);
    }
}
