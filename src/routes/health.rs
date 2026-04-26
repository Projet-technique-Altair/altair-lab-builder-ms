/**
 * @file health — service health check endpoint.
 *
 * @remarks
 * Provides a lightweight endpoint to verify that the Lab Builder service
 * is running and accessible.
 *
 * Response includes:
 *
 *  - Service status indicator
 *  - Current execution mode (local or cloud)
 *
 * Key characteristics:
 *
 *  - No dependency on external services
 *  - Fast and always available
 *  - Useful for monitoring, orchestration, and readiness checks
 *
 * This endpoint is typically used by load balancers, deployment platforms,
 * and observability tools to ensure service availability.
 *
 * @packageDocumentation
 */

use axum::{extract::State, Json};
use serde_json::json;

use crate::models::state::State as AppState;

pub async fn health(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(json!({
        "status": "lab-builder ok",
        "local_mode": state.builds_service.is_local_mode()
    }))
}
