//! A deliberately tiny HTTP/1.1 server for the local API: loopback only,
//! one request per connection, `Content-Length` bodies only (no chunked
//! encoding), and every request must carry the bearer token. Callers are
//! local tools like the Raycast extension, never browsers, so any request
//! with an `Origin` header is refused outright.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::core::SharedCore;

const MAX_HEADER_SIZE: usize = 64 * 1024;
const MAX_IN_MEMORY_BODY: usize = 1024 * 1024;
const HEADER_TIMEOUT: Duration = Duration::from_secs(30);

pub struct Request {
    pub method: String,
    pub path: String,
    pub query: HashMap<String, String>,
    /// Small bodies are kept in memory; larger ones are streamed to
    /// `body_file` instead and `body` is left empty.
    pub body: Vec<u8>,
    pub body_file: Option<PathBuf>,
}

pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}

impl Response {
    pub fn json(status: u16, value: impl Serialize) -> Self {
        Self { status, body: serde_json::to_vec(&value).unwrap_or_else(|_| b"{}".to_vec()) }
    }

    pub fn error(status: u16, message: impl Into<String>) -> Self {
        Self::json(status, serde_json::json!({ "error": message.into() }))
    }
}

struct Head {
    method: String,
    path: String,
    query: HashMap<String, String>,
    headers: HashMap<String, String>,
    content_length: Option<usize>,
}

pub async fn serve(listener: TcpListener, port: u16, token: String, core: SharedCore) {
    loop {
        let stream = match listener.accept().await {
            Ok((stream, _)) => stream,
            Err(error) => {
                // Out of file handles and the like: back off instead of spinning.
                log::warn!("Local API accept failed: {error}");
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                continue;
            }
        };
        let token = token.clone();
        let core = core.clone();
        tauri::async_runtime::spawn(async move {
            handle_connection(stream, port, &token, &core).await;
        });
    }
}

async fn handle_connection(mut stream: TcpStream, port: u16, token: &str, core: &SharedCore) {
    let mut body_file: Option<PathBuf> = None;
    let response = match read_request(&mut stream, port, token, &mut body_file).await {
        Ok(request) => super::router::handle(core, request).await,
        Err(response) => response,
    };
    let _ = write_response(&mut stream, &response).await;
    let _ = stream.shutdown().await;
    if let Some(file) = body_file {
        let _ = tokio::fs::remove_file(file).await;
    }
}

async fn read_request(
    stream: &mut TcpStream,
    port: u16,
    token: &str,
    body_file: &mut Option<PathBuf>,
) -> Result<Request, Response> {
    let mut buffer = Vec::with_capacity(4096);
    let header_end = tokio::time::timeout(HEADER_TIMEOUT, async {
        let mut chunk = [0u8; 8192];
        loop {
            if let Some(index) = find(&buffer, b"\r\n\r\n") {
                return Ok(index);
            }
            if buffer.len() > MAX_HEADER_SIZE {
                return Err(Response::error(431, "Request headers are too large."));
            }
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => return Err(Response::error(400, "Malformed request.")),
                Ok(read) => buffer.extend_from_slice(&chunk[..read]),
            }
        }
    })
    .await
    .map_err(|_| Response::error(408, "Timed out waiting for the request."))??;

    let head = parse_head(&buffer[..header_end]).ok_or_else(|| Response::error(400, "Malformed request."))?;
    // Checked as soon as the headers arrive, before any body is read, so an
    // unauthorized upload is refused without buffering it first.
    if let Some(rejection) = rejection(&head, port, token) {
        return Err(rejection);
    }
    let content_length = head.content_length.ok_or_else(|| Response::error(400, "Malformed request."))?;

    let mut rest = buffer[header_end + 4..].to_vec();
    rest.truncate(content_length);
    let mut body = Vec::new();
    if content_length > MAX_IN_MEMORY_BODY {
        let path = std::env::temp_dir().join("AktarLocalAPI").join(crate::util::new_id());
        *body_file = Some(path.clone());
        stream_to_file(stream, &path, rest, content_length)
            .await
            .map_err(|_| Response::error(500, "Could not buffer the request body."))?;
    } else {
        body = rest;
        let mut chunk = [0u8; 64 * 1024];
        while body.len() < content_length {
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => return Err(Response::error(400, "The request body ended early.")),
                Ok(read) => {
                    let wanted = (content_length - body.len()).min(read);
                    body.extend_from_slice(&chunk[..wanted]);
                }
            }
        }
    }

    Ok(Request { method: head.method, path: head.path, query: head.query, body, body_file: body_file.clone() })
}

async fn stream_to_file(stream: &mut TcpStream, path: &PathBuf, already: Vec<u8>, length: usize) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut file = tokio::fs::File::create(path).await?;
    file.write_all(&already).await?;
    let mut received = already.len();
    let mut chunk = vec![0u8; 256 * 1024];
    while received < length {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        let wanted = (length - received).min(read);
        file.write_all(&chunk[..wanted]).await?;
        received += wanted;
    }
    file.flush().await
}

fn rejection(head: &Head, port: u16, token: &str) -> Option<Response> {
    if head.headers.contains_key("origin") {
        return Some(Response::error(403, "Browser requests are not allowed."));
    }
    let allowed_hosts = [format!("127.0.0.1:{port}"), format!("localhost:{port}")];
    let host = head.headers.get("host").map(|host| host.to_ascii_lowercase());
    if !host.is_some_and(|host| allowed_hosts.contains(&host)) {
        return Some(Response::error(403, "Unexpected Host header."));
    }
    let authorized = head
        .headers
        .get("authorization")
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|candidate| constant_time_equals(candidate.as_bytes(), token.as_bytes()));
    if !authorized {
        return Some(Response::error(401, "Missing or invalid API token."));
    }
    if head.headers.contains_key("transfer-encoding") {
        return Some(Response::error(411, "Send a Content-Length instead of a chunked body."));
    }
    None
}

fn constant_time_equals(lhs: &[u8], rhs: &[u8]) -> bool {
    if lhs.len() != rhs.len() {
        return false;
    }
    lhs.iter().zip(rhs).fold(0u8, |difference, (a, b)| difference | (a ^ b)) == 0
}

fn parse_head(data: &[u8]) -> Option<Head> {
    let text = std::str::from_utf8(data).ok()?;
    let mut lines = text.split("\r\n");
    let request_line: Vec<&str> = lines.next()?.split(' ').collect();
    if request_line.len() != 3 {
        return None;
    }
    let method = request_line[0].to_ascii_uppercase();
    let target = url::Url::parse(&format!("http://localhost{}", request_line[1])).ok()?;
    let path = percent_encoding::percent_decode_str(target.path()).decode_utf8().ok()?.into_owned();
    let query = target.query_pairs().map(|(name, value)| (name.into_owned(), value.into_owned())).collect();

    let mut headers = HashMap::new();
    // A line that isn't a header is skipped, as the Mac app does, rather
    // than failing the whole request.
    for (name, value) in lines.filter_map(|line| line.split_once(':')) {
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
    }
    let content_length = match headers.get("content-length") {
        None => Some(0),
        Some(value) => value.parse().ok(),
    };
    Some(Head { method, path, query, headers, content_length })
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

async fn write_response(stream: &mut TcpStream, response: &Response) -> std::io::Result<()> {
    let header = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        response.status,
        reason(response.status),
        response.body.len()
    );
    stream.write_all(header.as_bytes()).await?;
    stream.write_all(&response.body).await?;
    stream.flush().await
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        409 => "Conflict",
        411 => "Length Required",
        422 => "Unprocessable Content",
        431 => "Request Header Fields Too Large",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Internal Server Error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(raw: &str) -> Head {
        parse_head(raw.as_bytes()).unwrap()
    }

    #[test]
    fn parses_requests() {
        let parsed = head("POST /v1/uploads?filename=a%20b.png&prefix= HTTP/1.1\r\nHost: 127.0.0.1:47913\r\nContent-Length: 12\r\nAuthorization: Bearer t");
        assert_eq!(parsed.method, "POST");
        assert_eq!(parsed.path, "/v1/uploads");
        assert_eq!(parsed.query["filename"], "a b.png");
        assert_eq!(parsed.query["prefix"], "");
        assert_eq!(parsed.content_length, Some(12));
    }

    #[test]
    fn rejects_browsers_foreign_hosts_and_bad_tokens() {
        let ok = head("GET /v1/status HTTP/1.1\r\nHost: localhost:47913\r\nAuthorization: Bearer secret");
        assert!(rejection(&ok, 47913, "secret").is_none());
        let origin = head("GET / HTTP/1.1\r\nHost: localhost:47913\r\nOrigin: https://evil.example\r\nAuthorization: Bearer secret");
        assert_eq!(rejection(&origin, 47913, "secret").unwrap().status, 403);
        let rebinding = head("GET / HTTP/1.1\r\nHost: evil.example:47913\r\nAuthorization: Bearer secret");
        assert_eq!(rejection(&rebinding, 47913, "secret").unwrap().status, 403);
        let wrong = head("GET / HTTP/1.1\r\nHost: localhost:47913\r\nAuthorization: Bearer nope");
        assert_eq!(rejection(&wrong, 47913, "secret").unwrap().status, 401);
    }
}
