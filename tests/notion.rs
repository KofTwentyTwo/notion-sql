//! Tests for blocking Notion API client and Notion response adapters.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use notion_sql::notion::{find_notion_api_error, NotionApiError, NotionClient};
use reqwest::Method;
use serde_json::Value;

#[allow(dead_code)]
const NOTION_VERSION: &str = "2022-06-28";

#[allow(dead_code)]
#[derive(Debug, Clone)]
struct MockRequest {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: String,
}

#[allow(dead_code)]
struct MockResponse {
    status: u16,
    headers: Vec<(&'static str, &'static str)>,
    body: String,
}

impl MockResponse {
    #[allow(dead_code)]
    fn json(status: u16, body: Value) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body.to_string(),
        }
    }

    #[allow(dead_code)]
    fn with_header(mut self, name: &'static str, value: &'static str) -> Self {
        self.headers.push((name, value));
        self
    }
}

#[allow(dead_code)]
struct MockServer {
    base_url: String,
    requests: Arc<Mutex<Vec<MockRequest>>>,
    handle: JoinHandle<()>,
}

impl MockServer {
    #[allow(dead_code)]
    fn start(responses: Vec<MockResponse>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        let base_url = format!("http://{}", listener.local_addr().expect("local address"));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let thread_requests = Arc::clone(&requests);

        let handle = thread::spawn(move || {
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

    #[allow(dead_code)]
    fn finish(self) -> Vec<MockRequest> {
        self.handle.join().expect("mock server thread");
        Arc::try_unwrap(self.requests)
            .expect("mock requests still referenced")
            .into_inner()
            .expect("mock requests mutex")
    }
}

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
    let mut headers = HashMap::new();

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

    let content_length = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0; content_length];
    reader.read_exact(&mut body).expect("read request body");

    MockRequest {
        method,
        path,
        headers,
        body: String::from_utf8(body).expect("utf8 request body"),
    }
}

#[allow(dead_code)]
fn write_mock_response(stream: &mut TcpStream, response: MockResponse) {
    let reason = match response.status {
        200 => "OK",
        401 => "Unauthorized",
        429 => "Too Many Requests",
        _ => "Mock Status",
    };
    let mut headers = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        response.status,
        reason,
        response.body.len()
    );
    for (name, value) in response.headers {
        headers.push_str(&format!("{name}: {value}\r\n"));
    }
    headers.push_str("\r\n");
    stream.write_all(headers.as_bytes()).expect("write headers");
    stream
        .write_all(response.body.as_bytes())
        .expect("write body");
}

#[allow(dead_code)]
fn test_client(base_url: String) -> NotionClient {
    let retry_sleeper = Arc::new(|_| {});
    NotionClient::new_for_tests(
        "secret-token".to_string(),
        base_url,
        Duration::from_secs(2),
        retry_sleeper,
    )
    .expect("test client")
}

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

#[test]
fn finds_notion_api_error_in_anyhow_chain() {
    let error: anyhow::Error = NotionApiError::from_response(
        &Method::POST,
        "/v1/search",
        reqwest::StatusCode::UNAUTHORIZED,
        r#"{"code":"unauthorized","message":"API token is invalid."}"#,
    )
    .into();

    assert!(find_notion_api_error(&error).is_some());
}
