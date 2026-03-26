use axum::{
    routing::{get, post},
    Router,
};

use crate::{
    models::state::State,
    routes::{
        builds::{create_build, get_build},
        health::health,
        source_bundles::create_source_bundle,
    },
};

pub mod builds;
pub mod health;
pub mod source_bundles;

pub fn init_routes() -> Router<State> {
    Router::new()
        .route("/health", get(health))
        .route("/builds", post(create_build))
        .route("/builds/:build_id", get(get_build))
        .route("/source-bundles", post(create_source_bundle))
}
