//! Tests for blocking Notion API client and Notion response adapters.
//!
//! # Purpose
//!
//! This integration-test module exercises two concerns of the `notion`
//! module in the `notion_sql` crate:
//!
//! 1. The mapping of Notion HTTP error responses into the crate's own
//!    [`NotionApiError`] type, and the human-readable text that type
//!    renders. These tests guard the *operator-facing* error experience:
//!    error code labels, remediation guidance (token/sharing hints), the
//!    request-id passthrough, and the deliberate suppression of raw,
//!    unparseable response bodies.
//! 2. The recovery of a [`NotionApiError`] from inside an [`anyhow::Error`]
//!    chain via [`find_notion_api_error`], which callers rely on to decide
//!    whether a failure originated from Notion.
//!
//! # Testing approach
//!
//! The bulk of the file is scaffolding for a hand-rolled, single-threaded
//! HTTP mock server ([`MockServer`]) built directly on [`TcpListener`]
//! rather than a heavier mock-HTTP crate. The server is queued with a fixed
//! sequence of [`MockResponse`]s, records every inbound [`MockRequest`], and
//! lets a test inspect exactly what the [`NotionClient`] sent on the wire
//! (method, path, headers, body). This is the machinery that would back
//! end-to-end client tests (retry behaviour, header injection, etc.).
//!
//! Note: at the time of writing the mock-server scaffolding and several
//! helpers carry `#[allow(dead_code)]` because the currently *enabled* tests
//! (the `#[test]` functions near the bottom) only call the pure
//! error-formatting paths and do not yet drive a live [`NotionClient`]
//! against the mock server. The scaffolding is kept in place so those
//! client-level tests can be added without rebuilding it.
//!
//! # Key items defined here
//!
//! - [`MockRequest`] / [`MockResponse`]: wire-level request capture and
//!   canned response description.
//! - [`MockServer`]: the lifecycle (start, serve N responses, finish) of the
//!   loopback test server.
//! - [`read_mock_request`] / [`write_mock_response`]: the minimal HTTP/1.1
//!   parse and serialize routines used by the server thread.
//! - [`test_client`]: constructs a [`NotionClient`] wired to a mock base URL
//!   with retry sleeps stubbed out so tests never actually block.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use notion_sql::notion::{find_notion_api_error, NotionApiError, NotionClient};
use reqwest::Method;
use serde_json::Value;

/// Notion API version string sent in the `Notion-Version` header.
///
/// Pinned to the date-stamped revision the crate targets; kept here so the
/// (currently dormant) client-level tests can assert the client forwards the
/// correct version. Marked `dead_code` while no enabled test references it.
#[allow(dead_code)]
const NOTION_VERSION: &str = "2022-06-28";

/// A request captured by the [`MockServer`] exactly as it arrived on the wire.
///
/// Used by tests to assert what the [`NotionClient`] actually sent: the HTTP
/// method, the request-target path, the headers, and the raw body. Header
/// names are stored lower-cased (see [`read_mock_request`]) so lookups are
/// case-insensitive without per-call normalization.
#[allow(dead_code)]
#[derive(Debug, Clone)]
struct MockRequest {
    /// HTTP method token from the request line (e.g. `GET`, `POST`).
    method: String,
    /// Request-target path from the request line (e.g. `/v1/search`).
    path: String,
    /// Request headers, keyed by lower-cased name to value.
    headers: HashMap<String, String>,
    /// Decoded UTF-8 request body (empty when no `Content-Length` was sent).
    body: String,
}

/// A canned HTTP response the [`MockServer`] will serve for one request.
///
/// Responses are supplied to [`MockServer::start`] as an ordered queue; the
/// server pops one per incoming connection, so the test controls precisely
/// what each successive request observes (useful for, e.g., a 429 followed by
/// a 200 to exercise retry logic).
#[allow(dead_code)]
struct MockResponse {
    /// HTTP status code to return.
    status: u16,
    /// Extra response headers beyond the always-sent content/connection ones.
    headers: Vec<(&'static str, &'static str)>,
    /// Raw response body (already serialized; see [`MockResponse::json`]).
    body: String,
}

impl MockResponse {
    /// Builds a JSON response from a status code and a [`serde_json::Value`].
    ///
    /// `status` is the HTTP status code; `body` is serialized to its string
    /// form and stored verbatim. Starts with no extra headers; chain
    /// [`MockResponse::with_header`] to add any. Returns the constructed
    /// response.
    #[allow(dead_code)]
    fn json(status: u16, body: Value) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body.to_string(),
        }
    }

    /// Appends one extra response header and returns `self` for chaining.
    ///
    /// `name`/`value` are `'static` because tests use string literals; this
    /// keeps the builder allocation-free. Use this to inject headers the
    /// client cares about, such as `Retry-After` on a 429. Returns the
    /// updated response.
    #[allow(dead_code)]
    fn with_header(mut self, name: &'static str, value: &'static str) -> Self {
        self.headers.push((name, value));
        self
    }
}

/// A minimal single-threaded loopback HTTP server for client tests.
///
/// On [`MockServer::start`] it binds an ephemeral loopback port and spawns a
/// background thread that serves exactly as many connections as there are
/// queued [`MockResponse`]s, recording each [`MockRequest`]. The owning test
/// points a [`NotionClient`] at [`base_url`](Self::base_url), drives it, then
/// calls [`MockServer::finish`] to join the thread and recover the captured
/// requests.
///
/// # Invariants
///
/// - The server handles a fixed, finite number of requests (one per queued
///   response) and then the accept loop ends, allowing the thread to exit.
///   A test that issues more requests than responses will hang on the
///   unanswered connection, so the response queue must match the expected
///   request count.
#[allow(dead_code)]
struct MockServer {
    /// Base URL (scheme + host + port) the client should target.
    base_url: String,
    /// Shared, mutex-guarded log of every request the server received.
    requests: Arc<Mutex<Vec<MockRequest>>>,
    /// Handle to the background accept/serve thread, joined in `finish`.
    handle: JoinHandle<()>,
}

impl MockServer {
    /// Starts the mock server with a fixed queue of responses to serve.
    ///
    /// `responses` are served in order, one per accepted connection. Binds
    /// `127.0.0.1:0` so the OS assigns a free port (avoiding collisions when
    /// tests run in parallel) and derives [`base_url`](Self::base_url) from
    /// the resolved local address. Returns the running [`MockServer`].
    ///
    /// # Panics
    ///
    /// Panics if the listener cannot bind or its local address cannot be
    /// resolved. (Per-connection failures panic inside the spawned thread and
    /// surface when [`MockServer::finish`] joins it.)
    #[allow(dead_code)]
    fn start(responses: Vec<MockResponse>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        let base_url = format!("http://{}", listener.local_addr().expect("local address"));
        let requests = Arc::new(Mutex::new(Vec::new()));
        // The accept loop owns its own Arc clone so the original handle can be
        // recovered by `finish` once the thread has dropped this one.
        let thread_requests = Arc::clone(&requests);

        let handle = thread::spawn(move || {
            // Drain the response queue front-to-back; the loop terminates
            // once every queued response has been served, which is what lets
            // the thread (and thus `finish`'s join) complete.
            let mut responses = VecDeque::from(responses);
            while let Some(response) = responses.pop_front() {
                let (mut stream, _) = listener.accept().expect("accept mock request");
                let request = read_mock_request(&mut stream);
                thread_requests
                    .lock()
                    .expect("lock mock requests")
                    .push(request);
                write_mock_response(&mut stream, response);
            }
        });

        Self {
            base_url,
            requests,
            handle,
        }
    }

    /// Joins the server thread and returns every request it captured.
    ///
    /// Consumes the server. Joining first guarantees the thread has finished
    /// writing to the shared request log, after which the `Arc`/`Mutex` are
    /// unwrapped to hand back the owned `Vec` without cloning. Returns the
    /// captured requests in arrival order.
    ///
    /// # Panics
    ///
    /// Panics if the server thread panicked, if the `Arc` is still referenced
    /// elsewhere (it should be sole owner once the thread exits), or if the
    /// mutex is poisoned.
    #[allow(dead_code)]
    fn finish(self) -> Vec<MockRequest> {
        self.handle.join().expect("mock server thread");
        // After the join, the spawned thread's Arc clone is dropped, so this
        // handle is the unique owner and `try_unwrap` can succeed.
        Arc::try_unwrap(self.requests)
            .expect("mock requests still referenced")
            .into_inner()
            .expect("mock requests mutex")
    }
}

/// Parses a single HTTP/1.1 request off `stream` into a [`MockRequest`].
///
/// Reads the request line (method + path, ignoring the version token), then
/// the header block up to the blank separator line, then exactly
/// `Content-Length` bytes of body. Header names are lower-cased so the
/// `content-length` lookup and later test assertions are case-insensitive.
/// Returns the captured request.
///
/// This is a deliberately tiny, assumption-heavy parser sufficient for the
/// well-formed requests `reqwest` emits; it is not a general HTTP parser
/// (e.g. it does not support chunked transfer encoding).
///
/// # Panics
///
/// Panics on any malformed input or I/O error: a request line missing the
/// method or path, a header line without a `:` separator, a body shorter than
/// the declared `Content-Length`, or a non-UTF-8 body.
#[allow(dead_code)]
fn read_mock_request(stream: &mut TcpStream) -> MockRequest {
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .expect("read request line");
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts.next().expect("request method").to_string();
    let path = request_parts.next().expect("request path").to_string();
    // The HTTP version token (third field) is intentionally ignored.
    let mut headers = HashMap::new();

    // Read header lines until the empty CRLF line that ends the header block.
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).expect("read header line");
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(':').expect("header separator");
        headers.insert(name.to_ascii_lowercase(), value.trim().to_string());
    }

    // Absent or unparseable Content-Length is treated as a zero-length body
    // rather than an error, since GET requests legitimately omit it.
    let content_length = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    // read_exact ensures we consume precisely the declared body length so the
    // stream is left at the correct position (and short bodies fail loudly).
    let mut body = vec![0; content_length];
    reader.read_exact(&mut body).expect("read request body");

    MockRequest {
        method,
        path,
        headers,
        body: String::from_utf8(body).expect("utf8 request body"),
    }
}

/// Serializes and writes a [`MockResponse`] to `stream` as HTTP/1.1.
///
/// `response` supplies the status, any extra headers, and the body. The
/// status line, `Content-Type: application/json`, an accurate
/// `Content-Length`, and `Connection: close` are always emitted; the
/// response's extra headers are appended before the blank separator line and
/// body. `Connection: close` keeps the exchange to one request per
/// connection, matching the one-response-per-accept model of [`MockServer`].
///
/// # Panics
///
/// Panics if writing the headers or body to the stream fails.
#[allow(dead_code)]
fn write_mock_response(stream: &mut TcpStream, response: MockResponse) {
    // Map only the status codes the tests use; everything else gets a
    // placeholder reason phrase, which clients ignore anyway.
    let reason = match response.status {
        200 => "OK",
        401 => "Unauthorized",
        429 => "Too Many Requests",
        _ => "Mock Status",
    };
    // Content-Length is the byte length of the body so the client knows
    // exactly how much to read; a wrong value would hang or truncate it.
    let mut headers = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        response.status,
        reason,
        response.body.len()
    );
    for (name, value) in response.headers {
        headers.push_str(&format!("{name}: {value}\r\n"));
    }
    // Blank line terminates the header block, per HTTP/1.1.
    headers.push_str("\r\n");
    stream.write_all(headers.as_bytes()).expect("write headers");
    stream
        .write_all(response.body.as_bytes())
        .expect("write body");
}

/// Builds a [`NotionClient`] wired to a mock server for testing.
///
/// `base_url` is the address of a running [`MockServer`]. Uses a dummy bearer
/// token and a short request timeout, and—crucially—installs a no-op retry
/// sleeper so retry paths run instantly instead of actually sleeping, keeping
/// the test suite fast and deterministic. Returns the constructed client.
///
/// # Panics
///
/// Panics if [`NotionClient::new_for_tests`] fails to build the client.
#[allow(dead_code)]
fn test_client(base_url: String) -> NotionClient {
    // No-op sleeper: retry backoff is exercised without real wall-clock waits.
    let retry_sleeper = Arc::new(|_| {});
    NotionClient::new_for_tests(
        "secret-token".to_string(),
        base_url,
        Duration::from_secs(2),
        retry_sleeper,
    )
    .expect("test client")
}

/// A 401 response must render with the `unauthorized` code, a clear
/// "token rejected" message, actionable token guidance, and the passed-through
/// request id.
///
/// Guards the operator experience for the most common misconfiguration (a bad
/// `NOTION_TOKEN`): the user should see *why* and *what to fix*, plus the
/// request id for support correlation.
#[test]
fn formats_unauthorized_error_with_token_guidance() {
    let message = NotionApiError::from_response(
        &Method::POST,
        "/v1/search",
        reqwest::StatusCode::UNAUTHORIZED,
        r#"{"object":"error","status":401,"code":"unauthorized","message":"API token is invalid.","request_id":"req-123"}"#,
    );
    let message = message.render_pretty();

    assert!(message.contains("notion-sql error"));
    assert!(message.contains("Code    : unauthorized"));
    assert!(message.contains("Token rejected by Notion."));
    assert!(message.contains("Request ID: req-123"));
}

/// A 403 `restricted_resource` response must explain the integration lacks
/// access and instruct the user to share the resource with the integration
/// behind `NOTION_TOKEN`.
///
/// This is the second most common failure (token valid, but the page/database
/// was never shared with the integration); the remediation differs entirely
/// from the 401 case, so it gets its own guidance text.
#[test]
fn formats_restricted_resource_error_with_sharing_guidance() {
    let message = NotionApiError::from_response(
        &Method::GET,
        "/v1/databases/database-id",
        reqwest::StatusCode::FORBIDDEN,
        r#"{"object":"error","status":403,"code":"restricted_resource","message":"Insufficient permissions.","request_id":"req-456"}"#,
    );
    let message = message.render_pretty();

    assert!(message.contains("The integration does not have access to this object."));
    assert!(message.contains("Share it with the integration connected to NOTION_TOKEN"));
}

/// When the error body is not valid Notion JSON, the rendered message must
/// fall back to `unknown_error` with a "no message" note and must NOT echo the
/// raw body.
///
/// The "not-json" assertion is the load-bearing one: dumping an arbitrary,
/// possibly large or sensitive upstream body to the user is undesirable, so
/// the formatter deliberately suppresses it. The 502 status here stands in for
/// any non-Notion-shaped failure (gateway/proxy error pages, etc.).
#[test]
fn formats_unparseable_error_body_without_raw_json_dump() {
    let message = NotionApiError::from_response(
        &Method::POST,
        "/v1/search",
        reqwest::StatusCode::BAD_GATEWAY,
        "not-json",
    );
    let message = message.render_pretty();

    assert!(message.contains("Code    : unknown_error"));
    assert!(message.contains("Notion did not return an error message"));
    assert!(!message.contains("not-json"));
}

/// A [`NotionApiError`] converted into an [`anyhow::Error`] must still be
/// discoverable via [`find_notion_api_error`].
///
/// Callers wrap errors in `anyhow` as they propagate; this verifies the
/// downcast-through-the-chain helper works so higher layers can detect a
/// Notion-origin failure (and, e.g., render the pretty guidance above) even
/// after the error has been boxed.
#[test]
fn finds_notion_api_error_in_anyhow_chain() {
    // `.into()` boxes the typed error into anyhow, mimicking real propagation.
    let error: anyhow::Error = NotionApiError::from_response(
        &Method::POST,
        "/v1/search",
        reqwest::StatusCode::UNAUTHORIZED,
        r#"{"code":"unauthorized","message":"API token is invalid."}"#,
    )
    .into();

    assert!(find_notion_api_error(&error).is_some());
}
