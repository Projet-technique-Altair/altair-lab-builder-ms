/**
 * @file builds — HTTP routes for build management.
 *
 * @remarks
 * Exposes endpoints for creating and retrieving build jobs
 * within the Lab Builder service.
 *
 * Endpoints:
 *
 *  - `POST /builds` → create a new build job
 *  - `GET /builds/:build_id` → retrieve an existing build job
 *
 * Key characteristics:
 *
 *  - Delegates business logic to `BuildsService`
 *  - Uses unified API response format (`ApiResponse<T>`)
 *  - Handles request validation via typed payloads
 *  - Returns structured errors through `AppError`
 *
 * This module acts as the HTTP interface for the build system,
 * connecting external clients (frontend, gateway) to the build pipeline.
 *
 * @packageDocumentation
 */

use axum::{
    extract::{Path, State},
    Json,
};
use uuid::Uuid;

use crate::{
    error::AppError,
    models::{
        api::ApiResponse,
        build::{BuildJob, CreateBuildRequest},
        state::State as AppState,
    },
};

pub async fn create_build(
    State(state): State<AppState>,
    Json(payload): Json<CreateBuildRequest>,
) -> Result<Json<ApiResponse<BuildJob>>, AppError> {
    let job = state.builds_service.create_build(payload).await?;
    Ok(Json(ApiResponse::success(job)))
}

pub async fn get_build(
    State(state): State<AppState>,
    Path(build_id): Path<Uuid>,
) -> Result<Json<ApiResponse<BuildJob>>, AppError> {
    let job = state.builds_service.get_build(build_id).await?;
    Ok(Json(ApiResponse::success(job)))
}
