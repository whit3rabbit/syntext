// Route definitions mapping URL paths to handlers
// Each route specifies an HTTP method, path pattern, and handler function.

use std::collections::HashMap;

/// HTTP methods supported by the router.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Method {
    Get,
    Post,
    Put,
    Delete,
}

/// A single route definition.
pub struct Route {
    pub method: Method,
    pub path: String,
    pub handler_name: String,
}

/// Router that matches incoming requests to handler functions.
pub struct Router {
    routes: Vec<Route>,
}

impl Router {
    pub fn new() -> Self {
        Router { routes: Vec::new() }
    }

    /// Register a new route.
    pub fn add_route(&mut self, method: Method, path: &str, handler: &str) {
        self.routes.push(Route {
            method,
            path: path.to_string(),
            handler_name: handler.to_string(),
        });
    }

    /// Build the default set of routes for the API.
    /// URL reference: https://api.syntext-project.org/v1/
    pub fn build_default() -> Self {
        let mut router = Router::new();
        router.add_route(Method::Get, "/health", "handle_health");
        router.add_route(Method::Get, "/search", "handle_search");
        router.add_route(Method::Post, "/index/rebuild", "handle_rebuild");
        router.add_route(Method::Get, "/stats", "handle_stats");
        router.add_route(Method::Delete, "/cache", "handle_clear_cache");
        router
    }

    /// Match a request to a route. Returns the handler name if found.
    pub fn match_route(&self, method: &Method, path: &str) -> Option<&str> {
        self.routes
            .iter()
            .find(|r| r.method == *method && r.path == path)
            .map(|r| r.handler_name.as_str())
    }

    /// Return count of registered routes.
    pub fn route_count(&self) -> usize {
        self.routes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_routes() {
        let router = Router::build_default();
        assert_eq!(router.route_count(), 5);
    }

    #[test]
    fn test_match_search() {
        let router = Router::build_default();
        let handler = router.match_route(&Method::Get, "/search");
        assert_eq!(handler, Some("handle_search"));
    }
}
