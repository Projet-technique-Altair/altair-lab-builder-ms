use axum::{extract::State, Json};
use serde_json::json;

use crate::models::state::State as AppState;

pub async fn health(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(json!({
        "status": "lab-builder ok",
        "local_mode": state.builds_service.is_local_mode()
    }))
}
