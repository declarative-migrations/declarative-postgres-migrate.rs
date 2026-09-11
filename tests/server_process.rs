#![cfg(unix)]

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const TOKEN: &str = "dpm-process-test-token-0123456789";
const OPENAPI: &[u8] = include_bytes!("../openapi/dpm-server-v1.json");

struct ServerProcess {
    child: Child,
    address: SocketAddr,
    stopped: bool,
}

impl ServerProcess {
    fn start(allow_apply: bool, max_in_flight: usize) -> Self {
        let probe = TcpListener::bind("127.0.0.1:0").expect("reserve a loopback port");
        let address = probe.local_addr().expect("read reserved address");
        drop(probe);

        let child = Command::new(env!("CARGO_BIN_EXE_dpm-server"))
            .env("DPM_SERVER_BIND", address.to_string())
            .env("DPM_SERVER_TOKEN", TOKEN)
            .env(
                "DPM_SERVER_DATABASES_JSON",
                r#"{"primary":"postgres://127.0.0.1:9/unused"}"#,
            )
            .env(
                "DPM_SERVER_ALLOW_APPLY",
                if allow_apply { "true" } else { "false" },
            )
            .env("DPM_SERVER_MAX_IN_FLIGHT", max_in_flight.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start dpm-server process");
        let mut server = Self {
            child,
            address,
            stopped: false,
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = server.child.try_wait().expect("inspect dpm-server process") {
                panic!("dpm-server exited before readiness with {status}");
            }
            if request(server.address, get_request("/healthz"))
                .is_ok_and(|response| response.status == 200)
            {
                break;
            }
            assert!(Instant::now() < deadline, "dpm-server did not become ready");
            thread::sleep(Duration::from_millis(20));
        }
        server
    }

    fn stop(&mut self) {
        let signal = Command::new("/bin/kill")
            .arg("-INT")
            .arg(self.child.id().to_string())
            .status()
            .expect("send SIGINT to dpm-server");
        assert!(signal.success(), "SIGINT delivery failed with {signal}");

        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().expect("wait for dpm-server shutdown") {
                assert!(status.success(), "dpm-server shutdown failed with {status}");
                self.stopped = true;
                return;
            }
            assert!(
                Instant::now() < deadline,
                "dpm-server did not shut down after SIGINT"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for ServerProcess {
    fn drop(&mut self) {
        if !self.stopped {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

struct HttpResponse {
    status: u16,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

fn request(address: SocketAddr, request: Vec<u8>) -> io::Result<HttpResponse> {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(2))?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    stream.write_all(&request)?;
    stream.shutdown(Shutdown::Write)?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response)?;
    parse_response(response)
}

fn parse_response(response: Vec<u8>) -> io::Result<HttpResponse> {
    let header_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing HTTP header end"))?;
    let head = std::str::from_utf8(&response[..header_end])
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid HTTP status"))?;
    let mut headers = BTreeMap::new();
    for line in lines {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid HTTP header"))?;
        headers.insert(name.to_ascii_lowercase(), value.trim().to_string());
    }
    let body = response[header_end + 4..].to_vec();
    Ok(HttpResponse {
        status,
        headers,
        body,
    })
}

fn get_request(path: &str) -> Vec<u8> {
    format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").into_bytes()
}

fn post_request(path: &str, body: &Value, authenticated: bool) -> Vec<u8> {
    let body = serde_json::to_vec(body).expect("encode request JSON");
    let authorization = if authenticated {
        format!("Authorization: Bearer {TOKEN}\r\n")
    } else {
        String::new()
    };
    let mut request = format!(
        "POST {path} HTTP/1.1\r\nHost: localhost\r\n{authorization}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    request.extend_from_slice(&body);
    request
}

fn empty_catalog() -> Value {
    json!({
        "format_version": 1,
        "server_version_num": 0,
        "database_flavor": "postgres",
        "schemas": [],
        "extensions": [],
        "enums": {},
        "sequences": {},
        "tables": {},
        "views": {},
        "functions": {},
        "triggers": {}
    })
}

fn error_code(response: &HttpResponse) -> String {
    let body: Value = serde_json::from_slice(&response.body).expect("decode error response");
    body["error"]["code"]
        .as_str()
        .expect("error response code")
        .to_string()
}

fn assert_request_id(response: &HttpResponse) {
    let header = response
        .headers
        .get("x-request-id")
        .expect("X-Request-Id response header");
    assert!(header.starts_with("dpm-"));
    if !response.status.to_string().starts_with('2') {
        let body: Value = serde_json::from_slice(&response.body).expect("decode error response");
        assert_eq!(body["error"]["request_id"], header.as_str());
    }
}

fn assert_bounded_concurrency(server: &ServerProcess) {
    let mut held = TcpStream::connect(server.address).expect("open held connection");
    held.write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\n")
        .expect("write incomplete held request");
    thread::sleep(Duration::from_millis(100));

    let mut queued = TcpStream::connect(server.address).expect("open queued connection");
    queued
        .set_read_timeout(Some(Duration::from_millis(250)))
        .expect("set bounded-concurrency timeout");
    queued
        .write_all(&get_request("/healthz"))
        .expect("write queued request");
    let mut byte = [0_u8; 1];
    let blocked = matches!(
        queued.read(&mut byte),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            )
    );
    assert!(blocked, "a request escaped the max-in-flight bound");

    drop(held);
    queued
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("extend queued response timeout");
    let mut response = Vec::new();
    queued
        .read_to_end(&mut response)
        .expect("read unblocked queued response");
    assert_eq!(
        parse_response(response)
            .expect("parse queued response")
            .status,
        200
    );
}

#[test]
fn real_process_contract_covers_safety_bounds_and_shutdown() {
    let mut disabled = ServerProcess::start(false, 1);

    let health = request(disabled.address, get_request("/healthz")).expect("health response");
    assert_eq!(health.status, 200);
    assert_eq!(
        serde_json::from_slice::<Value>(&health.body).expect("decode health")["status"],
        "ok"
    );
    assert_request_id(&health);

    let readiness = request(disabled.address, get_request("/readyz")).expect("readiness response");
    let readiness_body: Value = serde_json::from_slice(&readiness.body).expect("decode readiness");
    assert_eq!(readiness_body["configured_database_aliases"], 1);
    assert_eq!(readiness_body["apply_enabled"], false);

    let served_openapi =
        request(disabled.address, get_request("/openapi.json")).expect("OpenAPI response");
    assert_eq!(served_openapi.status, 200);
    assert_eq!(served_openapi.body, OPENAPI);

    let diff = json!({
        "source": {"kind": "catalog", "catalog": empty_catalog()},
        "target": {"kind": "catalog", "catalog": empty_catalog()}
    });
    let denied = request(disabled.address, post_request("/v1/diff", &diff, false))
        .expect("unauthenticated diff response");
    assert_eq!(denied.status, 401);
    assert_eq!(error_code(&denied), "unauthorized");
    assert_request_id(&denied);

    let empty = request(disabled.address, post_request("/v1/diff", &diff, true))
        .expect("authenticated diff response");
    assert_eq!(empty.status, 200);
    let empty_body: Value = serde_json::from_slice(&empty.body).expect("decode empty diff");
    assert_eq!(empty_body["summary"]["change_count"], 0);
    assert_eq!(empty_body["plan"]["changes"], json!([]));

    let apply = json!({
        "source": {"kind": "catalog", "catalog": empty_catalog()},
        "target": "primary",
        "dry_run": false,
        "confirmation": "apply:primary"
    });
    let apply_disabled = request(disabled.address, post_request("/v1/apply", &apply, true))
        .expect("apply-disabled response");
    assert_eq!(apply_disabled.status, 503);
    assert_eq!(error_code(&apply_disabled), "apply_disabled");

    let oversized = format!(
        "POST /v1/diff HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {TOKEN}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        1024 * 1024 + 1
    )
    .into_bytes();
    let too_large = request(disabled.address, oversized).expect("oversized-body response");
    assert_eq!(too_large.status, 413);
    assert_eq!(error_code(&too_large), "body_too_large");

    assert_bounded_concurrency(&disabled);
    disabled.stop();

    let mut enabled = ServerProcess::start(true, 4);
    let missing_confirmation = json!({
        "source": {"kind": "catalog", "catalog": empty_catalog()},
        "target": "primary",
        "dry_run": false
    });
    let confirmation_required = request(
        enabled.address,
        post_request("/v1/apply", &missing_confirmation, true),
    )
    .expect("confirmation-required response");
    assert_eq!(confirmation_required.status, 422);
    assert_eq!(error_code(&confirmation_required), "confirmation_required");

    let destructive_confirmation = json!({
        "source": {"kind": "catalog", "catalog": empty_catalog()},
        "target": "primary",
        "dry_run": false,
        "allow_destructive": true,
        "confirmation": "apply:primary"
    });
    let stronger_confirmation = request(
        enabled.address,
        post_request("/v1/apply", &destructive_confirmation, true),
    )
    .expect("destructive confirmation response");
    assert_eq!(stronger_confirmation.status, 422);
    assert_eq!(error_code(&stronger_confirmation), "confirmation_required");

    enabled.stop();
}
