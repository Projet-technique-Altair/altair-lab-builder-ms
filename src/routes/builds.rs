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
