use axum::{
    extract::{Multipart, State},
    Json,
};

use crate::{
    error::AppError,
    models::{api::ApiResponse, source_bundle::SourceBundle, state::State as AppState},
};

pub async fn create_source_bundle(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<Json<ApiResponse<SourceBundle>>, AppError> {
    let mut lab_id = None;
    let mut requested_by = None;
    let mut uploaded_files = Vec::new();

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|error| AppError::BadRequest(format!("Invalid multipart payload: {error}")))?
    {
        let field_name = field.name().unwrap_or("").to_string();
        let file_name = field.file_name().map(ToString::to_string);

        match (field_name.as_str(), file_name) {
            ("lab_id", None) => {
                let value = field.text().await.map_err(|error| {
                    AppError::BadRequest(format!("Failed to read lab_id field: {error}"))
                })?;
                if !value.trim().is_empty() {
                    lab_id = Some(value);
                }
            }
            ("requested_by", None) => {
                let value = field.text().await.map_err(|error| {
                    AppError::BadRequest(format!("Failed to read requested_by field: {error}"))
                })?;
                if !value.trim().is_empty() {
                    requested_by = Some(value);
                }
            }
            (_, Some(file_name)) => {
                let bytes = field.bytes().await.map_err(|error| {
                    AppError::BadRequest(format!("Failed to read uploaded file bytes: {error}"))
                })?;

                uploaded_files.push(crate::services::source_bundles::UploadedFileInput {
                    relative_path: file_name,
                    bytes: bytes.to_vec(),
                });
            }
            _ => {}
        }
    }

    let bundle = state
        .source_bundles_service
        .create_source_bundle(lab_id, requested_by, uploaded_files)
        .await?;

    Ok(Json(ApiResponse::success(bundle)))
}
