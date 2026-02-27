//! Axum HTTPS server, routing, and middleware.
//!
//! # Responsibilities
//! - Build and bind the TLS listener (rustls + ACM for Nitro Enclaves cert).
//! - Define the Axum router with all routes and shared middleware.
//! - Inject shared application state (`AppState`) into handlers.

// Sub-modules added as the server layer is implemented.
pub mod handlers;
pub mod middleware;
pub mod router;
pub mod state;
pub mod tls;
