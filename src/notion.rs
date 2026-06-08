//! Blocking Notion API client and Notion response adapters.
//!
//! The CLI issues one statement at a time, so a blocking `reqwest` client keeps
//! the implementation simple while still handling pagination and rate limits.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::{Method, StatusCode};
use serde_json::{json, Map, Value};

use crate::schema::DatabaseSchema;

/// Base URL for all Notion REST API requests.
const NOTION_BASE_URL: &str = "https://api.notion.com";
/// Notion API version pinned for request and response compatibility.
const NOTION_VERSION: &str = "2022-06-28";
/// Maximum number of retries for rate-limited requests.
const MAX_RETRIES: usize = 5;
/// Maximum time a single HTTP request may spend before failing.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Maximum time to wait for one Notion rate-limit retry hint.
const MAX_RETRY_AFTER_SLEEP: Duration = Duration::from_secs(30);

/// Injectable sleep callback used to keep retry tests fast.
type RetrySleeper = Arc<dyn Fn(Duration) + Send + Sync>;

/// A Notion page row returned by a database query.
#[derive(Debug, Clone)]
pub struct PageRow {
    /// Stable page ID used for update and trash operations.
    pub id: String,
    /// Best-effort display title extracted from the page title property.
    pub title: String,
    /// Raw Notion property values keyed by canonical property name.
    pub properties: Map<String, Value>,
}

/// Minimal database metadata for list and name resolution output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseInfo {
    /// Stable Notion database ID.
    pub id: String,
    /// Plain-text database title, or `<untitled>` when Notion provides none.
    pub name: String,
}

/// Blocking client wrapper for the Notion REST API.
pub struct NotionClient {
    /// Shared HTTP client with application user agent.
    http: Client,
    /// Base URL for Notion-compatible API requests.
    base_url: String,
    /// Internal integration token used in the authorization header.
    token: String,
    /// Case-insensitive database name lookup cache for a single CLI run.
    database_name_cache: HashMap<String, String>,
    /// Cap applied to a single Retry-After sleep.
    retry_after_sleep_cap: Duration,
    /// Sleep hook used for retry delays.
    retry_sleeper: RetrySleeper,
}

impl NotionClient {
    /// Creates a Notion client from an integration token.
    pub fn new(token: String) -> Result<Self> {
        Self::with_options(
            token,
            NOTION_BASE_URL.to_string(),
            MAX_RETRY_AFTER_SLEEP,
            Arc::new(thread::sleep),
        )
    }

    /// Creates a Notion client with test-controlled transport options.
    pub fn new_for_tests(
        token: String,
        base_url: String,
        retry_after_sleep_cap: Duration,
        retry_sleeper: RetrySleeper,
    ) -> Result<Self> {
        Self::with_options(token, base_url, retry_after_sleep_cap, retry_sleeper)
    }

    /// Creates a Notion client from explicit transport options.
    fn with_options(
        token: String,
        base_url: String,
        retry_after_sleep_cap: Duration,
        retry_sleeper: RetrySleeper,
    ) -> Result<Self> {
        Ok(Self {
            http: Client::builder()
                .user_agent(concat!("notion-sql/", env!("CARGO_PKG_VERSION")))
                .timeout(REQUEST_TIMEOUT)
                .build()
                .context("Failed to build HTTP client")?,
            base_url: base_url.trim_end_matches('/').to_string(),
            token,
            database_name_cache: HashMap::new(),
            retry_after_sleep_cap,
            retry_sleeper,
        })
    }

    /// Resolves either a raw Notion database ID or an exact database title to an ID.
    pub fn resolve_database(&mut self, requested: &str) -> Result<String> {
        if looks_like_notion_id(requested) {
            return Ok(requested.to_string());
        }

        let cache_key = requested.to_ascii_lowercase();
        if let Some(database_id) = self.database_name_cache.get(&cache_key) {
            return Ok(database_id.clone());
        }

        // Search may return fuzzy title matches, so only exact case-insensitive
        // database titles are accepted for SQL table names.
        let candidates = self.search_database_candidates(Some(requested))?;

        let exact_matches = candidates
            .iter()
            .filter(|candidate| candidate.name.eq_ignore_ascii_case(requested))
            .collect::<Vec<_>>();

        match exact_matches.as_slice() {
            [candidate] => {
                self.database_name_cache
                    .insert(cache_key, candidate.id.clone());
                Ok(candidate.id.clone())
            }
            [] => bail!(
                "No Notion database matched '{requested}'. Candidates: {}",
                format_candidates(&candidates)
            ),
            matches => bail!(
                "Multiple Notion databases matched '{requested}': {}",
                format_candidates(
                    &matches
                        .iter()
                        .map(|candidate| (*candidate).clone())
                        .collect::<Vec<_>>()
                )
            ),
        }
    }

    /// Lists all databases visible to the configured integration token.
    pub fn list_databases(&self) -> Result<Vec<DatabaseInfo>> {
        let mut databases = self
            .search_database_candidates(None)?
            .into_iter()
            .map(|candidate| DatabaseInfo {
                id: candidate.id,
                name: candidate.name,
            })
            .collect::<Vec<_>>();

        databases.sort_by(|left, right| {
            left.name
                .to_ascii_lowercase()
                .cmp(&right.name.to_ascii_lowercase())
                .then_with(|| left.id.cmp(&right.id))
        });

        Ok(databases)
    }

    /// Searches visible databases, following every Notion search page.
    fn search_database_candidates(&self, query: Option<&str>) -> Result<Vec<DatabaseCandidate>> {
        let mut databases = Vec::new();
        let mut start_cursor: Option<String> = None;

        loop {
            let mut body = json!({
                "page_size": 100,
                "filter": {
                    "property": "object",
                    "value": "database"
                }
            });
            if let Some(query) = query {
                body["query"] = Value::String(query.to_string());
            }
            if let Some(cursor) = &start_cursor {
                body["start_cursor"] = Value::String(cursor.clone());
            }

            // Notion search is paginated even for filtered database searches.
            let response = self.request_json(Method::POST, "/v1/search", Some(body))?;
            let results = response
                .get("results")
                .and_then(Value::as_array)
                .context("Search response did not include results")?;

            databases.extend(results.iter().filter_map(database_candidate));

            let has_more = response
                .get("has_more")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if !has_more {
                break;
            }

            start_cursor = response
                .get("next_cursor")
                .and_then(Value::as_str)
                .map(str::to_string);
            if start_cursor.is_none() {
                break;
            }
        }

        Ok(databases)
    }

    /// Retrieves and parses a database schema from Notion.
    pub fn retrieve_schema(&self, database_id: &str) -> Result<DatabaseSchema> {
        let database =
            self.request_json(Method::GET, &format!("/v1/databases/{database_id}"), None)?;
        DatabaseSchema::from_notion_database(&database)
    }

    /// Queries a database with optional filter, sort list, and row limit.
    pub fn query_database(
        &self,
        database_id: &str,
        filter: Option<Value>,
        sorts: Vec<Value>,
        limit: Option<usize>,
    ) -> Result<Vec<PageRow>> {
        self.query_database_with_progress(database_id, filter, sorts, limit, |_, _| Ok(()))
    }

    /// Queries a database while reporting `(pages_fetched, rows_matched)` after each page.
    pub fn query_database_with_progress(
        &self,
        database_id: &str,
        filter: Option<Value>,
        sorts: Vec<Value>,
        limit: Option<usize>,
        mut progress: impl FnMut(usize, usize) -> Result<()>,
    ) -> Result<Vec<PageRow>> {
        let mut rows = Vec::new();
        let mut start_cursor: Option<String> = None;
        let mut pages_fetched = 0usize;

        loop {
            let remaining = limit.map(|value| value.saturating_sub(rows.len()));
            if matches!(remaining, Some(0)) {
                break;
            }

            // The Notion API caps page size at 100, so requested SQL limits are
            // applied by shrinking the final request and stopping locally.
            let page_size = remaining.unwrap_or(100).min(100);
            let mut body = json!({ "page_size": page_size });
            if let Some(filter) = &filter {
                body["filter"] = filter.clone();
            }
            if !sorts.is_empty() {
                body["sorts"] = Value::Array(sorts.clone());
            }
            if let Some(cursor) = &start_cursor {
                body["start_cursor"] = Value::String(cursor.clone());
            }

            let response = self.request_json(
                Method::POST,
                &format!("/v1/databases/{database_id}/query"),
                Some(body),
            )?;

            let results = response
                .get("results")
                .and_then(Value::as_array)
                .context("Database query response did not include results")?;

            for page in results {
                rows.push(page_row_from_value(page)?);
            }
            pages_fetched += 1;
            progress(pages_fetched, rows.len())?;

            if limit.is_some_and(|value| rows.len() >= value) {
                break;
            }

            let has_more = response
                .get("has_more")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if !has_more {
                break;
            }

            start_cursor = response
                .get("next_cursor")
                .and_then(Value::as_str)
                .map(str::to_string);
            if start_cursor.is_none() {
                break;
            }
        }

        Ok(rows)
    }

    /// Moves a page to Notion trash.
    pub fn trash_page(&self, page_id: &str) -> Result<()> {
        self.request_json(
            Method::PATCH,
            &format!("/v1/pages/{page_id}"),
            Some(json!({ "in_trash": true })),
        )?;
        Ok(())
    }

    /// Replaces one or more page properties with a prepared Notion payload.
    pub fn update_page_properties(&self, page_id: &str, properties: Value) -> Result<()> {
        self.request_json(
            Method::PATCH,
            &format!("/v1/pages/{page_id}"),
            Some(json!({ "properties": properties })),
        )?;
        Ok(())
    }

    /// Creates a page under a database parent with prepared Notion properties.
    pub fn create_page(&self, database_id: &str, properties: Value) -> Result<()> {
        self.request_json(
            Method::POST,
            "/v1/pages",
            Some(json!({
                "parent": { "database_id": database_id },
                "properties": properties
            })),
        )?;
        Ok(())
    }

    /// Sends a JSON request to Notion and returns the parsed JSON response body.
    fn request_json(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
        let url = format!("{}{path}", self.base_url);
        let mut attempt = 0usize;

        loop {
            let mut request = self
                .http
                .request(method.clone(), &url)
                .headers(self.headers()?);
            if let Some(body) = &body {
                request = request.json(body);
            }

            let response = request
                .send()
                .with_context(|| format!("HTTP request failed for {method} {path}"))?;

            if response.status().as_u16() == 429 && attempt < MAX_RETRIES {
                // Honor Notion's retry hint when present and fall back to a
                // short pause for rate-limit responses without the header.
                let retry_after = retry_after_duration(response.headers())
                    .unwrap_or_else(|| Duration::from_secs(1))
                    .min(self.retry_after_sleep_cap);
                attempt += 1;
                (self.retry_sleeper)(retry_after);
                continue;
            }

            let status = response.status();
            let text = response
                .text()
                .with_context(|| format!("Failed reading response body for {method} {path}"))?;

            if !status.is_success() {
                return Err(NotionApiError::from_response(&method, path, status, &text).into());
            }

            return serde_json::from_str(&text)
                .with_context(|| format!("Failed to parse JSON response for {method} {path}"));
        }
    }

    /// Builds the headers required by every Notion API request.
    fn headers(&self) -> Result<HeaderMap> {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", self.token))
                .context("Invalid NOTION_TOKEN header value")?,
        );
        headers.insert("Notion-Version", HeaderValue::from_static(NOTION_VERSION));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        Ok(headers)
    }
}

/// Parsed Notion error body returned for non-success HTTP statuses.
#[derive(Debug, Clone, PartialEq, Eq)]
struct NotionApiErrorBody {
    /// Machine-readable Notion error code such as `unauthorized`.
    code: Option<String>,
    /// Human-readable Notion error message.
    message: Option<String>,
    /// Request identifier useful when debugging with Notion support or logs.
    request_id: Option<String>,
}

/// Structured Notion API failure with enough context for polished CLI rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotionApiError {
    /// HTTP method used for the failed request.
    method: String,
    /// Notion API path used for the failed request.
    path: String,
    /// HTTP status returned by Notion.
    status: StatusCode,
    /// Machine-readable Notion error code.
    code: String,
    /// Human-readable Notion error message.
    message: String,
    /// Request identifier returned by Notion.
    request_id: Option<String>,
    /// Short remediation title.
    action_title: &'static str,
    /// Concrete next steps to display under the remediation title.
    action_steps: &'static [&'static str],
}

impl NotionApiError {
    /// Creates a structured error from a failed Notion response body.
    pub fn from_response(method: &Method, path: &str, status: StatusCode, body: &str) -> Self {
        let body = parse_api_error_body(body);
        let code = body.code.unwrap_or_else(|| "unknown_error".to_string());
        let (action_title, action_steps) = api_error_guidance(status, &code);

        Self {
            method: method.to_string(),
            path: path.to_string(),
            status,
            code,
            message: body
                .message
                .unwrap_or_else(|| "Notion did not return an error message.".to_string()),
            request_id: body.request_id,
            action_title,
            action_steps,
        }
    }

    /// Returns a polished, multi-line terminal error block.
    pub fn render_pretty(&self) -> String {
        let mut lines = vec![
            "notion-sql error".to_string(),
            String::new(),
            format!("  Request : {} {}", self.method, self.path),
            format!("  Status  : {}", self.status),
            format!("  Code    : {}", self.code),
            format!("  Message : {}", self.message),
        ];

        if let Some(request_id) = &self.request_id {
            lines.push(format!("  Request ID: {request_id}"));
        }

        lines.push(String::new());
        lines.push(format!("  {}", self.action_title));
        for step in self.action_steps {
            lines.push(format!("  - {step}"));
        }

        lines.join("\n")
    }
}

impl fmt::Display for NotionApiError {
    /// Formats the error in the same polished shape used by the CLI.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.render_pretty())
    }
}

impl std::error::Error for NotionApiError {}

/// Attempts to recover a structured Notion API error from an `anyhow` chain.
pub fn find_notion_api_error(error: &anyhow::Error) -> Option<&NotionApiError> {
    error.chain().find_map(|cause| cause.downcast_ref())
}

/// Parses the subset of Notion's error JSON needed for diagnostics.
fn parse_api_error_body(body: &str) -> NotionApiErrorBody {
    let parsed = serde_json::from_str::<Value>(body).ok();

    NotionApiErrorBody {
        code: parsed
            .as_ref()
            .and_then(|value| value.get("code"))
            .and_then(Value::as_str)
            .map(str::to_string),
        message: parsed
            .as_ref()
            .and_then(|value| value.get("message"))
            .and_then(Value::as_str)
            .map(str::to_string),
        request_id: parsed
            .as_ref()
            .and_then(|value| value.get("request_id"))
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

/// Returns actionable guidance for common Notion API failure modes.
fn api_error_guidance(status: StatusCode, code: &str) -> (&'static str, &'static [&'static str]) {
    match (status, code) {
        (StatusCode::UNAUTHORIZED, "unauthorized") => (
            "Token rejected by Notion.",
            &[
                "Create or copy a current Notion internal integration secret.",
                "Export it as NOTION_TOKEN in the same shell running notion-sql.",
                "If this token was pasted anywhere public, revoke it before retrying.",
            ],
        ),
        (StatusCode::FORBIDDEN, "restricted_resource") => (
            "The integration does not have access to this object.",
            &[
                "Open the target database in Notion.",
                "Share it with the integration connected to NOTION_TOKEN.",
                "Retry after Notion confirms access was granted.",
            ],
        ),
        (StatusCode::NOT_FOUND, "object_not_found") => (
            "The requested Notion object was not found.",
            &[
                "Check the database or page ID for typos.",
                "Confirm the object is shared with the integration.",
                "Run --list-databases to see what the token can access.",
            ],
        ),
        (StatusCode::TOO_MANY_REQUESTS, "rate_limited") => (
            "Notion is still rate limiting requests.",
            &[
                "Wait a minute and retry.",
                "Reduce query volume or narrow the SQL WHERE clause.",
            ],
        ),
        (StatusCode::BAD_REQUEST, "validation_error") => (
            "Notion rejected the generated request payload.",
            &[
                "Check the SQL statement syntax.",
                "Verify property names match the Notion database schema.",
                "Verify written values match the Notion property types.",
            ],
        ),
        (StatusCode::CONFLICT, "conflict_error") => (
            "Notion reported an edit conflict.",
            &["Retry the command after refreshing the target data."],
        ),
        (StatusCode::INTERNAL_SERVER_ERROR, _) | (StatusCode::BAD_GATEWAY, _) => (
            "Notion returned a server error.",
            &[
                "Retry later.",
                "If it persists, keep the request ID for Notion support.",
            ],
        ),
        (StatusCode::SERVICE_UNAVAILABLE, _) | (StatusCode::GATEWAY_TIMEOUT, _) => {
            ("Notion is temporarily unavailable.", &["Retry later."])
        }
        _ => (
            "The request failed.",
            &[
                "Check the SQL statement.",
                "Check Notion database permissions.",
                "Check the integration token.",
                "Use the request ID if you need to contact Notion support.",
            ],
        ),
    }
}

/// Internal search candidate used while resolving a friendly database name.
#[derive(Debug, Clone)]
struct DatabaseCandidate {
    /// Stable Notion database ID.
    id: String,
    /// Plain-text title used for exact matching and diagnostics.
    name: String,
}

/// Converts a Notion search result into a candidate for exact title matching.
fn database_candidate(value: &Value) -> Option<DatabaseCandidate> {
    Some(DatabaseCandidate {
        id: value.get("id")?.as_str()?.to_string(),
        name: database_title(value),
    })
}

/// Extracts the display title from a Notion database object.
fn database_title(value: &Value) -> String {
    plain_text_array(value.get("title"))
        .or_else(|| {
            value
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| "<untitled>".to_string())
}

/// Converts a Notion page object into the row shape used by renderers and mutators.
fn page_row_from_value(value: &Value) -> Result<PageRow> {
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Page result did not include an id"))?
        .to_string();
    let properties = value
        .get("properties")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("Page {id} did not include properties"))?
        .clone();
    let title = properties
        .values()
        .find(|property| property.get("type").and_then(Value::as_str) == Some("title"))
        .and_then(|property| plain_text_array(property.get("title")))
        .unwrap_or_else(|| "<untitled>".to_string());

    Ok(PageRow {
        id,
        title,
        properties,
    })
}

/// Joins Notion rich text fragments into their plain-text representation.
fn plain_text_array(value: Option<&Value>) -> Option<String> {
    let text = value?
        .as_array()?
        .iter()
        .filter_map(|item| item.get("plain_text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("");

    Some(text)
}

/// Checks whether a string is formatted like a Notion UUID with or without hyphens.
fn looks_like_notion_id(value: &str) -> bool {
    let stripped = value.replace('-', "");
    stripped.len() == 32 && stripped.chars().all(|value| value.is_ascii_hexdigit())
}

/// Parses Notion's integer seconds Retry-After header.
fn retry_after_duration(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get("Retry-After")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs)
}

/// Formats database candidates for ambiguity and no-match error messages.
fn format_candidates(candidates: &[DatabaseCandidate]) -> String {
    if candidates.is_empty() {
        return "none".to_string();
    }
    candidates
        .iter()
        .map(|candidate| format!("{} ({})", candidate.name, candidate.id))
        .collect::<Vec<_>>()
        .join(", ")
}
