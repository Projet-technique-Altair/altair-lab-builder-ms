/**
 * @file routes — application route registration.
 *
 * @remarks
 * Defines and registers all HTTP routes exposed by the Lab Builder service,
 * mapping endpoints to their corresponding handlers.
 *
 * Registered routes:
 *
 *  - `GET /health` → service health check
 *  - `POST /builds` → create a build job
 *  - `POST /builds/from-upload` → upload sources and trigger a build
 *  - `GET /builds/{build_id}` → retrieve a build job
 *  - `POST /source-bundles` → create a source bundle from uploaded files
 *
 * Key characteristics:
 *
 *  - Centralized routing configuration
 *  - Uses shared application state (`State`)
 *  - Connects HTTP layer to route handlers
 *
 * This module acts as the entry point for all API endpoints,
 * assembling the router used by the application server.
 *
 * @packageDocumentation
 */

use axum::{
    routing::{get, post},
    Router,
};

use crate::{
    models::state::State,
    routes::{
        builds::{create_build, get_build},
        health::health,
        source_bundles::{create_build_from_upload, create_source_bundle},
    },
};

pub mod builds;
pub mod health;
pub mod source_bundles;

pub fn init_routes() -> Router<State> {
    Router::new()
        .route("/health", get(health))
        .route("/builds", post(create_build))
        .route("/builds/from-upload", post(create_build_from_upload))
        .route("/builds/{build_id}", get(get_build))
        .route("/source-bundles", post(create_source_bundle))
}
