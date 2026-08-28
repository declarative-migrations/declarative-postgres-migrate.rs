use std::collections::BTreeMap;
use std::io;

use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const MAX_HEADER_BYTES: usize = 32 * 1024;

#[derive(Debug)]
pub(crate) struct Request {
    pub method: String,
    pub path: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

#[derive(Debug)]
pub(crate) struct ReadError {
    pub status: u16,
    pub code: &'static str,
    pub message: String,
}

impl ReadError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: 400,
            code: "bad_request",
            message: message.into(),
        }
    }

    fn body_too_large(max_body_bytes: usize) -> Self {
        Self {
            status: 413,
            code: "body_too_large",
            message: format!("request body exceeds {max_body_bytes} bytes"),
        }
    }
}

#[derive(Debug)]
pub(crate) struct Response {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
    pub headers: Vec<(String, String)>,
}

impl Response {
    pub fn json<T: Serialize>(status: u16, value: &T) -> Self {
        let body = serde_json::to_vec(value).unwrap_or_else(|_| {
            concat!(
                r#"{"error":{"code":"serialization_error","#,
                r#""message":"response serialization failed","#,
                r#""request_id":"unknown"}}"#
            )
            .as_bytes()
            .to_vec()
        });
        Self {
            status,
            content_type: "application/json; charset=utf-8",
            body,
            headers: Vec::new(),
        }
    }

    pub fn json_bytes(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            content_type: "application/json; charset=utf-8",
            body: body.into(),
            headers: Vec::new(),
        }
    }

    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
}

#[derive(Debug)]
struct ParsedHead {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    content_length: usize,
}

pub(crate) async fn read_request(
    stream: &mut TcpStream,
    max_body_bytes: usize,
) -> Result<Request, ReadError> {
    let mut buffer = Vec::with_capacity(4096);
    let mut chunk = [0_u8; 4096];
    let header_end = loop {
        if let Some(index) = find_header_end(&buffer) {
            break index;
        }
        if buffer.len() >= MAX_HEADER_BYTES {
            return Err(ReadError {
                status: 431,
                code: "headers_too_large",
                message: format!("request headers exceed {MAX_HEADER_BYTES} bytes"),
            });
        }
        let read = stream
            .read(&mut chunk)
            .await
            .map_err(|error| ReadError::bad_request(format!("failed to read request: {error}")))?;
        if read == 0 {
            return Err(ReadError::bad_request(
                "connection closed before request headers",
            ));
        }
        buffer.extend_from_slice(&chunk[..read]);
    };

    if header_end > MAX_HEADER_BYTES {
        return Err(ReadError {
            status: 431,
            code: "headers_too_large",
            message: format!("request headers exceed {MAX_HEADER_BYTES} bytes"),
        });
    }

    let parsed = parse_head(&buffer[..header_end])?;
    if parsed.content_length > max_body_bytes {
        return Err(ReadError::body_too_large(max_body_bytes));
    }

    let body_start = header_end + 4;
    let required = body_start
        .checked_add(parsed.content_length)
        .ok_or_else(|| ReadError::body_too_large(max_body_bytes))?;
    while buffer.len() < required {
        let remaining = required - buffer.len();
        let read_size = remaining.min(chunk.len());
        let read = stream
            .read(&mut chunk[..read_size])
            .await
            .map_err(|error| ReadError::bad_request(format!("failed to read body: {error}")))?;
        if read == 0 {
            return Err(ReadError::bad_request(
                "connection closed before request body",
            ));
        }
        buffer.extend_from_slice(&chunk[..read]);
    }

    if buffer.len() != required {
        return Err(ReadError::bad_request(
            "unexpected bytes after request body; pipelining is not supported",
        ));
    }

    Ok(Request {
        method: parsed.method,
        path: parsed.path,
        headers: parsed.headers,
        body: buffer[body_start..required].to_vec(),
    })
}

fn parse_head(head: &[u8]) -> Result<ParsedHead, ReadError> {
    let text = std::str::from_utf8(head)
        .map_err(|_| ReadError::bad_request("request headers must be UTF-8"))?;
    let mut lines = text.split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| ReadError::bad_request("missing request line"))?;
    let mut parts = request_line.split_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| ReadError::bad_request("missing request method"))?;
    let raw_path = parts
        .next()
        .ok_or_else(|| ReadError::bad_request("missing request path"))?;
    let version = parts
        .next()
        .ok_or_else(|| ReadError::bad_request("missing HTTP version"))?;
    if parts.next().is_some() {
        return Err(ReadError::bad_request("invalid request line"));
    }
    if version != "HTTP/1.1" && version != "HTTP/1.0" {
        return Err(ReadError::bad_request("unsupported HTTP version"));
    }
    if !raw_path.starts_with('/') {
        return Err(ReadError::bad_request("request path must be absolute"));
    }
    let path = raw_path.split('?').next().unwrap_or(raw_path).to_string();

    let mut headers = BTreeMap::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| ReadError::bad_request("malformed request header"))?;
        let name = name.trim().to_ascii_lowercase();
        if name.is_empty() {
            return Err(ReadError::bad_request("empty request header name"));
        }
        let value = value.trim().to_string();
        if let Some(previous) = headers.insert(name.clone(), value.clone()) {
            if previous != value {
                return Err(ReadError::bad_request(format!(
                    "conflicting duplicate header: {name}"
                )));
            }
        }
    }

    if let Some(transfer_encoding) = headers.get("transfer-encoding") {
        if !transfer_encoding.eq_ignore_ascii_case("identity") {
            return Err(ReadError {
                status: 501,
                code: "unsupported_transfer_encoding",
                message: "chunked transfer encoding is not supported".to_string(),
            });
        }
    }

    let content_length = match headers.get("content-length") {
        Some(value) => value
            .parse::<usize>()
            .map_err(|_| ReadError::bad_request("invalid Content-Length"))?,
        None => 0,
    };

    Ok(ParsedHead {
        method: method.to_ascii_uppercase(),
        path,
        headers,
        content_length,
    })
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

pub(crate) async fn write_response(stream: &mut TcpStream, response: Response) -> io::Result<()> {
    let reason = reason_phrase(response.status);
    let mut head = format!(
        concat!(
            "HTTP/1.1 {} {}\r\n",
            "Content-Type: {}\r\n",
            "Content-Length: {}\r\n",
            "Connection: close\r\n",
            "Cache-Control: no-store\r\n",
            "X-Content-Type-Options: nosniff\r\n"
        ),
        response.status,
        reason,
        response.content_type,
        response.body.len()
    );
    for (name, value) in response.headers {
        head.push_str(&name);
        head.push_str(": ");
        head.push_str(&value);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(&response.body).await?;
    stream.shutdown().await
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
        422 => "Unprocessable Entity",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        _ => "Error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_request_head_and_strips_query() {
        let parsed =
            parse_head(b"POST /v1/diff?trace=1 HTTP/1.1\r\nHost: localhost\r\nContent-Length: 2")
                .unwrap();
        assert_eq!(parsed.method, "POST");
        assert_eq!(parsed.path, "/v1/diff");
        assert_eq!(parsed.content_length, 2);
    }

    #[test]
    fn rejects_conflicting_content_lengths() {
        let error = parse_head(b"POST /v1/diff HTTP/1.1\r\nContent-Length: 2\r\nContent-Length: 3")
            .unwrap_err();
        assert_eq!(error.status, 400);
    }

    #[test]
    fn rejects_chunked_requests() {
        let error =
            parse_head(b"POST /v1/diff HTTP/1.1\r\nTransfer-Encoding: chunked").unwrap_err();
        assert_eq!(error.status, 501);
    }

    #[test]
    fn response_reason_phrases_cover_public_statuses() {
        assert_eq!(reason_phrase(413), "Payload Too Large");
        assert_eq!(reason_phrase(503), "Service Unavailable");
    }
}
