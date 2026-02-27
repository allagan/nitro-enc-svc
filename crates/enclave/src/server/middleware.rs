//! Axum middleware layers applied to the router.
//!
//! Includes request tracing, timeout enforcement, and response compression.

use std::time::Duration;

/// Default per-request timeout applied to all routes.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
