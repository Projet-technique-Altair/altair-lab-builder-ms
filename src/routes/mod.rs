use axum::{
    routing::{get, post},
    Router,
};

use crate::{
    models::state::State,
    routes::{
        builds::{create_build, get_build},
        health::health,
    },
};

pub mod builds;
pub mod health;

pub fn init_routes() -> Router<State> {
    Router::new()
        .route("/health", get(health))
        .route("/builds", post(create_build))
        .route("/builds/:build_id", get(get_build))
}
