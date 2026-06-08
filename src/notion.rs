//! Blocking Notion API client and Notion response adapters.
//!
//! The CLI issues one statement at a time, so a blocking `reqwest` client keeps
//! the implementation simple while still handling pagination and rate limits.
//!
//! # Responsibilities
//!
//! - Owns [`NotionClient`], the single entry point the rest of the crate uses to
//!   talk to Notion: resolving database names, listing databases, retrieving
//!   schemas, querying rows, and creating/updating/trashing pages.
//! - Centralizes the cross-cutting transport concerns every endpoint shares:
//!   header construction, cursor-based pagination, HTTP 429 retry-with-backoff,
//!   and translation of non-success responses into a structured error.
//! - Adapts loosely typed Notion JSON into the small, ergonomic types the rest
//!   of the crate consumes ([`PageRow`], [`DatabaseInfo`]) so that JSON poking is
//!   confined to this module.
//! - Produces [`NotionApiError`], a presentation-ready error carrying remediation
//!   guidance, which the CLI renders directly to the terminal.
//!
//! # Design notes
//!
//! The client is deliberately transport-pluggable via `NotionClient::with_options`
//! so tests can inject a mock base URL and a fast, non-blocking sleeper instead of
//! actually pausing on rate-limit retries. Schema parsing is delegated to
//! [`crate::schema::DatabaseSchema`]; this module only fetches the raw JSON.

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
///
/// Used as the default host in [`NotionClient::new`]; tests override it via
/// [`NotionClient::new_for_tests`] to point at a local mock server.
const NOTION_BASE_URL: &str = "https://api.notion.com";
/// Notion API version pinned for request and response compatibility.
///
/// Notion requires every request to declare the API version it was written
/// against via the `Notion-Version` header. Pinning it here keeps request and
/// response shapes stable so the JSON adapters in this module stay valid; bump
/// it deliberately alongside any adapter changes.
const NOTION_VERSION: &str = "2022-06-28";
/// Maximum number of retries for rate-limited (HTTP 429) requests.
///
/// Bounds the retry loop in [`NotionClient::request_json`] so a persistently
/// throttled endpoint eventually surfaces an error instead of hanging forever.
const MAX_RETRIES: usize = 5;
/// Maximum time a single HTTP request may spend before failing.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Maximum time to wait for one Notion rate-limit retry hint.
///
/// Notion's `Retry-After` value is attacker-/server-controlled, so it is clamped
/// to this ceiling to prevent a hostile or misbehaving response from stalling
/// the CLI for an unbounded period.
const MAX_RETRY_AFTER_SLEEP: Duration = Duration::from_secs(30);

/// Injectable sleep callback used to keep retry tests fast.
///
/// Production code wires this to [`std::thread::sleep`]; tests substitute a
/// no-op (or recording) closure so the 429 backoff path can be exercised without
/// real wall-clock delay. `Arc<dyn Fn ..>` is used (rather than a generic) so the
/// concrete [`NotionClient`] type stays simple and object-safe across both paths.
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
    /// Creates a production Notion client from an integration token.
    ///
    /// Uses the real Notion host, the standard rate-limit sleep cap, and the
    /// blocking [`std::thread::sleep`] as the retry sleeper.
    ///
    /// `token` is the Notion internal integration secret; it is sent verbatim in
    /// the `Authorization: Bearer` header.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying `reqwest` client fails to build (for
    /// example, on TLS backend initialization failure).
    pub fn new(token: String) -> Result<Self> {
        Self::with_options(
            token,
            NOTION_BASE_URL.to_string(),
            MAX_RETRY_AFTER_SLEEP,
            Arc::new(thread::sleep),
        )
    }

    /// Creates a Notion client with test-controlled transport options.
    ///
    /// Exposes the otherwise-private `Self::with_options` so integration tests
    /// can point the client at a mock server and inject a fast sleeper.
    ///
    /// - `token`: integration secret placed in the authorization header.
    /// - `base_url`: host to send requests to (e.g. a local mock server).
    /// - `retry_after_sleep_cap`: ceiling applied to each 429 backoff sleep.
    /// - `retry_sleeper`: callback invoked instead of really sleeping between
    ///   retries, allowing tests to advance without wall-clock delay.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying `reqwest` client fails to build.
    pub fn new_for_tests(
        token: String,
        base_url: String,
        retry_after_sleep_cap: Duration,
        retry_sleeper: RetrySleeper,
    ) -> Result<Self> {
        Self::with_options(token, base_url, retry_after_sleep_cap, retry_sleeper)
    }

    /// Creates a Notion client from explicit transport options.
    ///
    /// The shared constructor behind both [`Self::new`] and
    /// [`Self::new_for_tests`]. Builds the HTTP client with the crate's user
    /// agent and request timeout, normalizes `base_url` by stripping any trailing
    /// slash (so later `format!("{base_url}{path}")` joins never double up the
    /// `/`), and starts with an empty name cache.
    ///
    /// # Errors
    ///
    /// Returns an error if `reqwest` fails to build the HTTP client.
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
    ///
    /// `requested` is whatever the user named as a table: it may already be a
    /// Notion UUID (returned as-is) or a human-friendly database title that must
    /// be looked up via search. Lookups are cached case-insensitively for the
    /// life of the client so repeated references in one CLI run hit Notion once.
    ///
    /// Matching is intentionally strict: Notion's search is fuzzy, so only
    /// candidates whose title matches case-insensitively *exactly* are accepted.
    /// This avoids silently operating on the wrong database.
    ///
    /// # Errors
    ///
    /// Returns an error if the search request fails, if no database title matches
    /// `requested` exactly, or if more than one does (ambiguous name). The error
    /// message lists the candidate databases to help the user disambiguate.
    pub fn resolve_database(&mut self, requested: &str) -> Result<String> {
        // A value already shaped like a UUID is taken to be an ID and returned
        // verbatim, skipping the search round-trip entirely.
        if looks_like_notion_id(requested) {
            return Ok(requested.to_string());
        }

        // Cache keyed on the lowercased name so case variations of the same
        // table reference share a single resolved ID.
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
            // Exactly one exact-title match is the only unambiguous success case;
            // cache it before returning so the next reference is free.
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
    ///
    /// Performs an unfiltered search and projects each candidate to the public
    /// [`DatabaseInfo`] shape. Results are sorted for stable, human-friendly
    /// output (see below). Drives the CLI's `--list-databases` mode.
    ///
    /// # Errors
    ///
    /// Returns an error if any underlying search request fails.
    pub fn list_databases(&self) -> Result<Vec<DatabaseInfo>> {
        let mut databases = self
            .search_database_candidates(None)?
            .into_iter()
            .map(|candidate| DatabaseInfo {
                id: candidate.id,
                name: candidate.name,
            })
            .collect::<Vec<_>>();

        // Sort case-insensitively by name for readable output, then by ID as a
        // tiebreaker so identically-named databases (and the overall ordering)
        // are deterministic across runs.
        databases.sort_by(|left, right| {
            left.name
                .to_ascii_lowercase()
                .cmp(&right.name.to_ascii_lowercase())
                .then_with(|| left.id.cmp(&right.id))
        });

        Ok(databases)
    }

    /// Searches visible databases, following every Notion search page.
    ///
    /// `query` is an optional title filter passed to Notion's search; `None`
    /// returns all visible databases. The `object == database` filter ensures
    /// pages are excluded. Pagination is handled internally so the caller always
    /// receives the complete result set.
    ///
    /// # Errors
    ///
    /// Returns an error if a search request fails or if a response is missing the
    /// expected `results` array.
    fn search_database_candidates(&self, query: Option<&str>) -> Result<Vec<DatabaseCandidate>> {
        let mut databases = Vec::new();
        let mut start_cursor: Option<String> = None;

        // Loop over Notion's cursor-paginated search until `has_more` is false
        // (or a missing cursor forces a stop), accumulating every page.
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
            // Defensive guard: `has_more` was true but no cursor was supplied.
            // Stopping here avoids an infinite loop re-fetching the first page.
            if start_cursor.is_none() {
                break;
            }
        }

        Ok(databases)
    }

    /// Retrieves and parses a database schema from Notion.
    ///
    /// `database_id` must be a resolved Notion database ID. Fetches the raw
    /// database object and delegates parsing to
    /// [`DatabaseSchema::from_notion_database`].
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP request fails, Notion returns a non-success
    /// status, or the schema cannot be parsed from the response.
    pub fn retrieve_schema(&self, database_id: &str) -> Result<DatabaseSchema> {
        let database =
            self.request_json(Method::GET, &format!("/v1/databases/{database_id}"), None)?;
        DatabaseSchema::from_notion_database(&database)
    }

    /// Queries a database with optional filter, sort list, and row limit.
    ///
    /// Convenience wrapper over [`Self::query_database_with_progress`] with a
    /// no-op progress callback; see that method for parameter and pagination
    /// semantics.
    ///
    /// # Errors
    ///
    /// Propagates any error from [`Self::query_database_with_progress`].
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
    ///
    /// Walks Notion's cursor-paginated query endpoint, collecting [`PageRow`]s.
    ///
    /// - `database_id`: resolved Notion database ID to query.
    /// - `filter`: optional Notion filter object, included only when present.
    /// - `sorts`: ordered list of Notion sort objects; omitted when empty.
    /// - `limit`: optional cap on returned rows. Notion has no server-side row
    ///   limit, so it is enforced by shrinking the final page request and
    ///   stopping once enough rows are collected.
    /// - `progress`: invoked after each fetched page with the running page count
    ///   and row count; returning an error from it aborts the query (used by the
    ///   CLI to surface progress and to honor cancellation).
    ///
    /// # Errors
    ///
    /// Returns an error if any query request fails, if a response is missing its
    /// `results` array, if a page row cannot be parsed, or if the `progress`
    /// callback returns an error.
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
            // How many rows are still wanted under the caller's limit;
            // `saturating_sub` keeps this at 0 rather than underflowing once the
            // limit has already been reached.
            let remaining = limit.map(|value| value.saturating_sub(rows.len()));
            // A limit that is exactly satisfied means there is nothing left to
            // request, so stop before issuing a needless page-size-0 query.
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

            // Stop as soon as the limit is met even if Notion reports more
            // pages, since the extra rows would be discarded anyway.
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
            // Same defensive guard as the search loop: `has_more` without a
            // cursor would otherwise spin forever.
            if start_cursor.is_none() {
                break;
            }
        }

        Ok(rows)
    }

    /// Moves a page to Notion trash.
    ///
    /// `page_id` is the page to soft-delete. Notion has no hard-delete endpoint;
    /// setting `in_trash: true` is the supported way to remove a row, so this is
    /// what backs SQL `DELETE`. The parsed response body is intentionally
    /// discarded.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or Notion returns a non-success
    /// status (for example, the page is not shared with the integration).
    pub fn trash_page(&self, page_id: &str) -> Result<()> {
        self.request_json(
            Method::PATCH,
            &format!("/v1/pages/{page_id}"),
            Some(json!({ "in_trash": true })),
        )?;
        Ok(())
    }

    /// Replaces one or more page properties with a prepared Notion payload.
    ///
    /// `page_id` identifies the target page; `properties` must already be a
    /// Notion-shaped `properties` object (built by the writer layer, not raw SQL
    /// values). Backs SQL `UPDATE`. The parsed response body is discarded.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or Notion rejects the payload (for
    /// example, a property type mismatch yields a validation error).
    pub fn update_page_properties(&self, page_id: &str, properties: Value) -> Result<()> {
        self.request_json(
            Method::PATCH,
            &format!("/v1/pages/{page_id}"),
            Some(json!({ "properties": properties })),
        )?;
        Ok(())
    }

    /// Creates a page under a database parent with prepared Notion properties.
    ///
    /// `database_id` is the parent database for the new row; `properties` must be
    /// a Notion-shaped `properties` object. Backs SQL `INSERT`. The parsed
    /// response body is discarded.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or Notion rejects the payload.
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
    ///
    /// The single choke point for all HTTP traffic, centralizing headers,
    /// rate-limit retries, error translation, and JSON parsing so the endpoint
    /// methods stay thin.
    ///
    /// - `method`: HTTP method; cloned per attempt since a retry re-sends it.
    /// - `path`: API path appended to the client's base URL (e.g. `/v1/search`).
    /// - `body`: optional JSON body serialized onto the request when present.
    ///
    /// On HTTP 429 it sleeps (honoring `Retry-After`, clamped) and retries up to
    /// [`MAX_RETRIES`] before giving up and returning the error response.
    ///
    /// # Errors
    ///
    /// Returns an error if the request cannot be sent, the response body cannot
    /// be read, Notion returns a non-success status (wrapped as
    /// [`NotionApiError`]), or the success body is not valid JSON.
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

            // Retry only on 429 and only while under the attempt budget; any
            // other status (including success) falls through to be handled below.
            if response.status().as_u16() == 429 && attempt < MAX_RETRIES {
                // Honor Notion's retry hint when present and fall back to a
                // short pause for rate-limit responses without the header. The
                // cap bounds how long any single backoff can stall the CLI.
                let retry_after = retry_after_duration(response.headers())
                    .unwrap_or_else(|| Duration::from_secs(1))
                    .min(self.retry_after_sleep_cap);
                attempt += 1;
                // Indirected through the injected sleeper so tests avoid real delay.
                (self.retry_sleeper)(retry_after);
                continue;
            }

            let status = response.status();
            let text = response
                .text()
                .with_context(|| format!("Failed reading response body for {method} {path}"))?;

            // Non-success (and exhausted-retry 429) responses become a
            // structured, presentation-ready error rather than a raw status.
            if !status.is_success() {
                return Err(NotionApiError::from_response(&method, path, status, &text).into());
            }

            return serde_json::from_str(&text)
                .with_context(|| format!("Failed to parse JSON response for {method} {path}"));
        }
    }

    /// Builds the headers required by every Notion API request.
    ///
    /// Produces the `Authorization`, `Notion-Version`, and `Content-Type` headers
    /// sent on every request. Rebuilt per request because [`Client`] is shared and
    /// immutable.
    ///
    /// # Errors
    ///
    /// Returns an error if the token cannot be encoded as a valid HTTP header
    /// value (for example, it contains control characters), which surfaces as an
    /// invalid `NOTION_TOKEN`.
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
    ///
    /// Captures the request `method`/`path` and `status` for context, parses what
    /// it can from the error `body`, and precomputes remediation guidance keyed on
    /// the (status, code) pair so rendering later is purely mechanical. Missing
    /// fields fall back to sensible placeholders rather than failing.
    pub fn from_response(method: &Method, path: &str, status: StatusCode, body: &str) -> Self {
        let body = parse_api_error_body(body);
        // Notion always sends a code in practice, but default defensively so the
        // guidance lookup and rendering never hit an empty string.
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
    ///
    /// Lays out the request/status/code/message, the optional request ID, and the
    /// remediation title with its bullet steps as a single newline-joined string
    /// ready to print. This is also reused by the [`fmt::Display`] impl so both
    /// paths render identically.
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

/// Renders the error via [`NotionApiError::render_pretty`] so the `Display`
/// output (used by `anyhow` and generic error formatting) matches the CLI block.
impl fmt::Display for NotionApiError {
    /// Formats the error in the same polished shape used by the CLI.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.render_pretty())
    }
}

/// Marks [`NotionApiError`] as a standard error so it can participate in
/// `anyhow` chains and be recovered later via [`find_notion_api_error`]. No
/// custom behavior is needed beyond the default `Error` methods.
impl std::error::Error for NotionApiError {}

/// Attempts to recover a structured Notion API error from an `anyhow` chain.
///
/// Errors bubble up wrapped in `anyhow::Error` with added context, which erases
/// the concrete type. This walks the cause chain and downcasts each link,
/// returning the first [`NotionApiError`] found so the CLI can render its rich
/// guidance instead of a flat message. Returns `None` if the chain holds no such
/// error.
pub fn find_notion_api_error(error: &anyhow::Error) -> Option<&NotionApiError> {
    error.chain().find_map(|cause| cause.downcast_ref())
}

/// Parses the subset of Notion's error JSON needed for diagnostics.
///
/// `body` is the raw response text. Best-effort: if the body is not valid JSON
/// (or omits any field) the corresponding fields stay `None` rather than
/// erroring, since this runs while already handling a failure and must not fail
/// itself.
fn parse_api_error_body(body: &str) -> NotionApiErrorBody {
    // `.ok()` discards parse failures; a non-JSON body simply yields all-None.
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
///
/// Maps a (`status`, `code`) pair to a remediation title and concrete next-step
/// bullets. Specific known cases are matched first; server-error statuses match
/// on status alone (any code), and the final arm is a generic catch-all so every
/// failure yields *some* guidance. All strings are `'static` because the advice
/// is fixed at compile time.
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
///
/// `value` is one element of a search `results` array. Returns `None` (filtered
/// out by the caller's `filter_map`) when the result lacks a string `id`, since
/// an ID-less candidate is unusable. A missing title is tolerated via the
/// placeholder from [`database_title`].
fn database_candidate(value: &Value) -> Option<DatabaseCandidate> {
    Some(DatabaseCandidate {
        id: value.get("id")?.as_str()?.to_string(),
        name: database_title(value),
    })
}

/// Extracts the display title from a Notion database object.
///
/// `value` is a database (or search result) object. Prefers the rich-text
/// `title` array; falls back to a plain string `name` field (some object shapes
/// use it) and finally to `<untitled>` so the result is always non-empty.
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
///
/// `value` is one page object from a query `results` array.
///
/// # Errors
///
/// Returns an error if the page is missing a string `id` or a `properties`
/// object; both are required to display the row and to target later mutations.
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
    // The title isn't a fixed key: Notion stores it under whichever property has
    // type "title", so find that property and join its rich-text. A page with no
    // title property degrades gracefully to the placeholder.
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
///
/// Notion represents text as an array of rich-text fragments, each carrying a
/// `plain_text` field; this concatenates them into one string. `value` is the
/// optional array node. Returns `None` when the node is absent or not an array
/// (letting callers apply their own fallback); an empty array yields
/// `Some("")`. Fragments without `plain_text` are skipped.
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
///
/// `value` is the raw table reference. Used by [`NotionClient::resolve_database`]
/// to decide between treating input as an ID versus a name to search for. The
/// check is structural only: hyphens are stripped, then the remainder must be
/// exactly 32 hex digits. It does not validate that the UUID actually exists.
fn looks_like_notion_id(value: &str) -> bool {
    let stripped = value.replace('-', "");
    stripped.len() == 32 && stripped.chars().all(|value| value.is_ascii_hexdigit())
}

/// Parses Notion's integer seconds Retry-After header.
///
/// `headers` is the rate-limited response's header map. Notion sends
/// `Retry-After` as whole seconds; returns `None` if the header is absent,
/// non-ASCII, or not a valid `u64`, in which case the caller falls back to a
/// default pause.
fn retry_after_duration(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get("Retry-After")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs)
}

/// Formats database candidates for ambiguity and no-match error messages.
///
/// `candidates` is the candidate set to describe. Renders each as
/// `name (id)`, comma-joined, or the literal `none` for an empty set so error
/// messages always read sensibly.
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
