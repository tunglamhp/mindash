//! Web layer for MinDash.
//!
//! The Python original served everything through one `BaseHTTPRequestHandler`
//! with a hand-written router and a hand-rolled session check on every path.
//! Here routing is typed, the auth check is middleware, and the state is shared
//! behind an `Arc<RwLock<_>>` instead of a module-level global.

pub mod api;
pub mod auth;
pub mod control;
pub mod exec;
pub mod net;
pub mod state;
pub mod widgets;
pub mod win_stats;

pub use state::AppState;

use axum::{
    body::Body,
    http::{header, Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use std::sync::Arc;

/// Paths served without a session. Everything else needs one when a password is
/// configured.
///
/// The icon and manifest are here because the browser fetches them while parsing
/// the login page itself: behind auth they would 401 and the tab would show a
/// placeholder. Neither reveals anything, so there is nothing to protect.
const PUBLIC_PREFIXES: [&str; 6] = [
    // The auth endpoints must be public: login is the route that issues a
    // session, and logout only clears a cookie.
    "/api/auth",
    "/assets",
    "/login",
    "/icon.svg",
    "/manifest.webmanifest",
    "/favicon.ico",
];

pub fn router(state: Arc<AppState>) -> Router {
    let api = Router::new()
        .route("/api/version", get(api::version))
        .route("/api/config", get(api::get_config).post(api::put_config))
        .route("/api/health", get(api::health))
        .route("/api/widget-types", get(api::widget_types))
        .route("/api/widgets", post(api::create_widget))
        .route(
            "/api/widgets/{id}",
            post(api::update_widget).delete(api::delete_widget),
        )
        .route("/api/widget/{id}", get(api::widget_data))
        // Server-sent events instead of the 5 s polling loop the Python
        // dashboard used: one long-lived connection, pushed only on change.
        .route("/api/stream", get(api::stream))
        .route("/api/auth/login", post(auth::login))
        .route("/api/auth/logout", post(auth::logout))
        // -- control plane ---------------------------------------------------
        // Devices are always addressed by id; the host, user and port come from
        // stored config, never from the request.
        .route("/api/devices/status", get(control::devices_status))
        .route("/api/devices/{id}/stats", get(control::device_stats))
        .route("/api/devices/{id}/power", post(control::device_power))
        .route("/api/devices/{id}/wake", post(control::device_wake))
        .route("/api/devices/{id}/files", get(control::files_list))
        .route("/api/devices/{id}/file", get(control::files_read))
        .route("/api/containers", get(control::containers_list))
        .route(
            "/api/containers/available",
            get(control::containers_available),
        )
        .route("/api/containers/action", post(control::container_control));

    let static_files = Router::new()
        .route("/", get(api::index))
        .route("/login", get(api::login_page))
        .fallback_service(tower_http::services::ServeDir::new(
            state.static_dir.clone(),
        ));

    Router::new()
        .merge(api)
        .merge(static_files)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            require_session,
        ))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state)
}

/// Auth as middleware rather than a per-handler check.
///
/// Behaviour differs by request kind on purpose: an API caller gets a 401 with a
/// JSON body it can act on, a browser gets the login page. The Python version
/// returned the login HTML with status 200 for both, which made an expired
/// session surface as a JSON parse error in the dashboard.
async fn require_session(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    req: Request<Body>,
    next: Next,
) -> Response {
    if !state.auth_enabled() {
        return next.run(req).await;
    }

    let path = req.uri().path().to_string();
    if PUBLIC_PREFIXES.iter().any(|p| path_is_public(&path, p)) {
        return next.run(req).await;
    }

    let token = req
        .headers()
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(auth::session_from_cookie);

    if token.as_deref().is_some_and(|t| state.verify_session(t)) {
        return next.run(req).await;
    }

    if path.starts_with("/api/") {
        (
            StatusCode::UNAUTHORIZED,
            axum::Json(serde_json::json!({ "success": false, "error": "Unauthorized" })),
        )
            .into_response()
    } else {
        // Send the browser somewhere it can actually sign in.
        (StatusCode::FOUND, [(header::LOCATION, "/login")]).into_response()
    }
}

/// Does `path` fall under the public prefix `prefix`?
///
/// Matched on whole path segments, not on raw substrings. A substring match made
/// the `/login` prefix swallow `/api/auth/login`, so once a password was set the
/// login endpoint itself answered 401 and there was no way back in.
fn path_is_public(path: &str, prefix: &str) -> bool {
    if path == prefix {
        return true;
    }
    match path.strip_prefix(prefix) {
        Some(rest) => rest.starts_with('/'),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_prefixes_match_whole_segments() {
        for (path, prefix) in [
            ("/login", "/login"),
            ("/login/", "/login"),
            ("/assets/mindash-web.js", "/assets"),
            ("/icon.svg", "/icon.svg"),
            ("/manifest.webmanifest", "/manifest.webmanifest"),
            ("/favicon.ico", "/favicon.ico"),
        ] {
            assert!(path_is_public(path, prefix), "{path} should match {prefix}");
        }
    }

    #[test]
    fn the_login_endpoint_is_not_treated_as_the_login_page() {
        // The bug this guards: a substring match made the `/login` prefix swallow
        // `/api/auth/login`, so once a password was set the login endpoint itself
        // answered 401 and there was no way back in.
        assert!(!path_is_public("/api/auth/login", "/login"));
        assert!(!path_is_public("/api/auth/logout", "/login"));
        // Nor should a sibling path with a shared prefix sneak through.
        assert!(!path_is_public("/assets-evil/x", "/assets"));
        assert!(!path_is_public("/loginfoo", "/login"));
    }

    #[test]
    fn the_login_endpoint_is_reachable_without_a_session() {
        // It has to be: it is the route that issues the session.
        assert!(
            PUBLIC_PREFIXES
                .iter()
                .any(|p| path_is_public("/api/auth/login", p)),
            "/api/auth/login must be public or nobody can sign in"
        );
    }
}
