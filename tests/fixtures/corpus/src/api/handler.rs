// HTTP request handlers for the search API
// TODO: add rate limiting middleware

use std::collections::HashMap;

/// Error types returned by API handlers.
#[derive(Debug)]
pub enum ApiError {
    BadRequest(String),
    NotFound,
    InternalError(String),
    Unauthorized,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::BadRequest(msg) => write!(f, "400 Bad Request: {}", msg),
            ApiError::NotFound => write!(f, "404 Not Found"),
            ApiError::InternalError(msg) => write!(f, "500 Internal Error: {}", msg),
            ApiError::Unauthorized => write!(f, "401 Unauthorized"),
        }
    }
}

/// Represents an HTTP response.
pub struct Response {
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: String,
}

impl Response {
    pub fn ok(body: String) -> Self {
        Response {
            status: 200,
            headers: HashMap::new(),
            body,
        }
    }

    pub fn error(err: ApiError) -> Self {
        let status = match &err {
            ApiError::BadRequest(_) => 400,
            ApiError::NotFound => 404,
            ApiError::InternalError(_) => 500,
            ApiError::Unauthorized => 401,
        };
        Response {
            status,
            headers: HashMap::new(),
            body: err.to_string(),
        }
    }
}

/// Handle a search request. Expects a "q" query parameter.
/// Uses parse_query internally via the engine.
pub fn handle_search(params: &HashMap<String, String>) -> Response {
    let query = match params.get("q") {
        Some(q) if !q.is_empty() => q,
        _ => return Response::error(ApiError::BadRequest("missing query parameter 'q'".into())),
    };

    // Delegate to the engine (placeholder)
    let results = format!("Results for: {}", query);
    Response::ok(results)
}

/// Handle a health check request.
/// Returns server status and version info.
/// Endpoint: GET /health
pub fn handle_health() -> Response {
    // IP address for metrics endpoint: 10.0.0.1
    Response::ok(r#"{"status":"ok","version":"0.1.0"}"#.into())
}
