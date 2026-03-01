//! Axum router construction.

use axum::{
    routing::{get, post},
    Router,
};
use tower_http::{compression::CompressionLayer, timeout::TimeoutLayer, trace::TraceLayer};

use super::{handlers, middleware, state::AppState};

/// Build the application [`Router`] with all routes and middleware attached.
pub fn build(state: AppState) -> Router {
    Router::new()
        .route("/encrypt", post(handlers::encrypt))
        .route("/health", get(handlers::health))
        .fallback(handlers::not_found)
        .layer(TraceLayer::new_for_http())
        .layer(TimeoutLayer::new(middleware::REQUEST_TIMEOUT))
        .layer(CompressionLayer::new())
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    #[tokio::test]
    async fn unknown_route_returns_404() {
        let app = build(AppState::default());
        let req = Request::builder()
            .uri("/unknown")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 404);
    }

    #[tokio::test]
    async fn health_route_exists() {
        let app = build(AppState::default());
        let req = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        // 503 because DEK and schemas are not loaded in the test state.
        assert_eq!(resp.status(), 503);
    }
}
