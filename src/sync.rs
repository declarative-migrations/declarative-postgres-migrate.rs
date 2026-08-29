//! Remote consumer for the versioned `dpm-server` contract.
//!
//! This module is the extraction boundary for a future `dpm-sync` crate/repo.
//! It never accepts or transmits database credentials; requests use server-side
//! aliases defined by the operator.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use reqwest::{Method, StatusCode, Url};
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::interfaces::{
    ApplyRequest, ApplyResponse, DiffRequest, DiffResponse, ErrorResponse, HealthResponse,
    VersionResponse, APPLY_PATH, DIFF_PATH, HEALTH_PATH, VERSION_PATH,
};

#[derive(Clone)]
pub struct DpmClient {
    base_url: String,
    bearer_token: Option<String>,
    http: reqwest::Client,
}

impl DpmClient {
    pub fn new(base_url: impl AsRef<str>) -> Result<Self> {
        let base_url = normalize_base_url(base_url.as_ref())?;
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            .user_agent(concat!("dpm-sync/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("building dpm-server HTTP client")?;
        Ok(Self {
            base_url,
            bearer_token: None,
            http,
        })
    }

    pub fn with_bearer_token(mut self, token: impl Into<String>) -> Result<Self> {
        let token = token.into();
        if token.trim().is_empty() {
            bail!("dpm-server bearer token must not be empty");
        }
        self.bearer_token = Some(token);
        Ok(self)
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub async fn health(&self) -> Result<HealthResponse> {
        self.send::<(), HealthResponse>(Method::GET, HEALTH_PATH, None)
            .await
    }

    pub async fn version(&self) -> Result<VersionResponse> {
        self.send::<(), VersionResponse>(Method::GET, VERSION_PATH, None)
            .await
    }

    pub async fn diff(&self, request: &DiffRequest) -> Result<DiffResponse> {
        self.send(Method::POST, DIFF_PATH, Some(request)).await
    }

    pub async fn apply(&self, request: &ApplyRequest) -> Result<ApplyResponse> {
        self.send(Method::POST, APPLY_PATH, Some(request)).await
    }

    async fn send<B, T>(&self, method: Method, path: &str, body: Option<&B>) -> Result<T>
    where
        B: Serialize + ?Sized,
        T: DeserializeOwned,
    {
        let url = format!("{}{}", self.base_url, path);
        let mut request = self.http.request(method, &url);
        if let Some(token) = &self.bearer_token {
            request = request.bearer_auth(token);
        }
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request
            .send()
            .await
            .with_context(|| format!("request to dpm-server endpoint {path} failed"))?;
        let status = response.status();
        let body = response.bytes().await?;
        decode_response(status, &body, path)
    }
}

fn normalize_base_url(raw: &str) -> Result<String> {
    let mut url = Url::parse(raw).context("dpm-server base URL is invalid")?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("dpm-server base URL must use http or https");
    }
    if url.host_str().is_none() {
        bail!("dpm-server base URL must contain a host");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("dpm-server base URL must not contain credentials");
    }
    if url.query().is_some() || url.fragment().is_some() {
        bail!("dpm-server base URL must not contain a query or fragment");
    }
    let path = url.path().trim_end_matches('/').to_string();
    url.set_path(&path);
    Ok(url.to_string().trim_end_matches('/').to_string())
}

fn decode_response<T: DeserializeOwned>(status: StatusCode, body: &[u8], path: &str) -> Result<T> {
    if status.is_success() {
        return serde_json::from_slice(body)
            .with_context(|| format!("dpm-server returned invalid JSON for {path}"));
    }
    if let Ok(error) = serde_json::from_slice::<ErrorResponse>(body) {
        bail!(
            "dpm-server {} at {path}: {} (request_id={})",
            error.error.code,
            error.error.message,
            error.error.request_id
        );
    }
    bail!("dpm-server returned HTTP {status} at {path}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_is_normalized_without_credentials() {
        let client = DpmClient::new("https://example.com/api/").unwrap();
        assert_eq!(client.base_url(), "https://example.com/api");
        assert!(DpmClient::new("ftp://example.com").is_err());
        assert!(DpmClient::new("https://user:secret@example.com").is_err());
        assert!(DpmClient::new("https://example.com?token=secret").is_err());
    }

    #[test]
    fn empty_bearer_tokens_are_rejected() {
        assert!(DpmClient::new("http://127.0.0.1:8080")
            .unwrap()
            .with_bearer_token("   ")
            .is_err());
    }
}
