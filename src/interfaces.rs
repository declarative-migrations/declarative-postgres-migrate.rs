//! Versioned HTTP contract shared by `dpm-server` and remote consumers.
//!
//! This module deliberately contains no database URLs, filesystem paths, or
//! credentials. Remote requests refer to operator-configured database aliases
//! so secrets remain inside the server's trust boundary.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::Catalog;

pub const API_VERSION: &str = "v1";
pub const HEALTH_PATH: &str = "/healthz";
pub const READY_PATH: &str = "/readyz";
pub const VERSION_PATH: &str = "/v1/version";
pub const DIFF_PATH: &str = "/v1/diff";
pub const APPLY_PATH: &str = "/v1/apply";
pub const OPENAPI_PATH: &str = "/openapi.json";
pub const OPENAPI_JSON: &str = include_str!("../openapi/dpm-server-v1.json");

/// A catalog supplied inline or obtained from an operator-configured alias.
/// Database URLs are intentionally not part of the public wire contract.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CatalogSource {
    Catalog { catalog: Catalog },
    Database { name: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiffRequest {
    pub source: CatalogSource,
    pub target: CatalogSource,
    #[serde(default)]
    pub allow_destructive: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MigrationSummary {
    pub change_count: usize,
    pub destructive_count: usize,
    pub gated_count: usize,
    pub manual_count: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DiffResponse {
    pub api_version: String,
    pub source: String,
    pub target: String,
    pub database_flavor: String,
    /// The core plan remains represented as JSON so this contract can be
    /// extracted into `dpm-interfaces` without coupling consumers to every
    /// internal Rust enum variant.
    pub plan: Value,
    pub sql: String,
    pub summary: MigrationSummary,
}

fn default_dry_run() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplyRequest {
    pub source: CatalogSource,
    /// Operator-configured database alias. Applying to inline catalogs is not
    /// meaningful and arbitrary URLs are never accepted over HTTP.
    pub target: String,
    /// Safe default: omitted requests only preview the migration.
    #[serde(default = "default_dry_run")]
    pub dry_run: bool,
    #[serde(default)]
    pub allow_destructive: bool,
    /// Required for a live apply. Must equal `apply:<alias>` or, for a plan
    /// containing destructive changes, `apply-destructive:<alias>`.
    #[serde(default)]
    pub confirmation: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApplyResponse {
    pub api_version: String,
    pub source: String,
    pub target: String,
    pub database_flavor: String,
    pub dry_run: bool,
    pub applied: bool,
    pub executed_statements: usize,
    pub verification_remaining_changes: usize,
    pub plan: Value,
    pub sql: String,
    pub summary: MigrationSummary,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    pub service: String,
    pub version: String,
    pub api_version: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReadyResponse {
    pub status: String,
    pub configured_database_aliases: usize,
    pub apply_enabled: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VersionResponse {
    pub service: String,
    pub version: String,
    pub api_version: String,
    pub catalog_format_version: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: ApiError,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApiError {
    pub code: String,
    pub message: String,
    pub request_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_defaults_to_dry_run() {
        let request: ApplyRequest = serde_json::from_value(serde_json::json!({
            "source": {"kind": "catalog", "catalog": Catalog::default()},
            "target": "primary"
        }))
        .unwrap();
        assert!(request.dry_run);
        assert!(!request.allow_destructive);
    }

    #[test]
    fn wire_contract_rejects_database_urls() {
        let result = serde_json::from_value::<CatalogSource>(serde_json::json!({
            "kind": "database",
            "name": "postgres://user:secret@example/db",
            "url": "postgres://user:secret@example/db"
        }));
        assert!(result.is_err());
    }

    #[test]
    fn openapi_document_is_valid_json() {
        let document: Value = serde_json::from_str(OPENAPI_JSON).unwrap();
        assert_eq!(document["info"]["version"], "1.0.0");
        assert!(document["paths"].get(DIFF_PATH).is_some());
        assert!(document["paths"].get(APPLY_PATH).is_some());
    }
}
