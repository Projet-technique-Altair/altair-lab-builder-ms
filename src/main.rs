use tower_http::cors::{Any, CorsLayer};
use tracing::info;
use tracing_subscriber::EnvFilter;

mod error;
mod models;
mod routes;
mod services;

#[cfg(test)]
mod tests;

const DEFAULT_PORT: &str = "8086";

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let state = models::state::State::from_env();

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = routes::init_routes().layer(cors).with_state(state);

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
