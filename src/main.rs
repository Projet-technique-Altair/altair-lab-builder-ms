/**
 * @file main — application entry point.
 *
 * @remarks
 * Bootstraps the Lab Builder service by initializing configuration,
 * logging, middleware, and HTTP routes.
 *
 * Responsibilities:
 *
 *  - Load environment variables (`dotenv`)
 *  - Initialize structured logging (`tracing`)
 *  - Build application state from environment (`State::from_env`)
 *  - Configure middleware:
 *    - CORS (allow all origins, methods, headers)
 *    - HTTP request tracing
 *  - Register routes and attach shared state
 *  - Start the HTTP server on the configured port
 *
 * Key characteristics:
 *
 *  - Async runtime powered by Tokio
 *  - Modular architecture (routes, services, models)
 *  - Observability via tracing logs
 *  - Environment-driven configuration
 *
 * This module is the entry point of the service,
 * responsible for wiring all components together and starting the server.
 *
 * @packageDocumentation
 */
use axum::http::HeaderValue;
use tower_http::cors::{AllowOrigin, Any, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing::info;
use tracing_subscriber::EnvFilter;

mod error;
mod models;
mod routes;
mod services;

#[cfg(test)]
mod tests;

const DEFAULT_PORT: &str = "8086";
const DEFAULT_ALLOWED_ORIGINS: &str = "http://localhost:5173,http://localhost:3000";

fn parse_allowed_origins() -> Vec<HeaderValue> {
    std::env::var("ALLOWED_ORIGINS")
        .unwrap_or_else(|_| DEFAULT_ALLOWED_ORIGINS.to_string())
        .split(',')
        .filter_map(|origin| HeaderValue::from_str(origin.trim()).ok())
        .collect()
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let state = models::state::State::from_env();

    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list(parse_allowed_origins()))
        .allow_methods(Any)
        .allow_headers(Any);

    let app = routes::init_routes()
        .layer(TraceLayer::new_for_http())
        .layer(cors)
        .with_state(state);

    let port = std::env::var("PORT").unwrap_or_else(|_| DEFAULT_PORT.to_string());
    let addr = format!("0.0.0.0:{port}");

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("Failed to bind lab-builder-ms port");

    info!(address = %addr, "Lab builder service started");
    axum::serve(listener, app)
        .await
        .expect("Lab builder service failed");
}
