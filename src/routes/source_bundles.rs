use axum::{
    extract::{Multipart, State},
    Json,
};
use tracing::info;

use crate::{
    error::AppError,
    models::{
        api::ApiResponse,
        build::CreateBuildRequest,
        source_bundle::{BuildFromUploadResponse, SourceBundle},
        state::State as AppState,
    },
    services::source_bundles::UploadedFileInput,
};

struct SourceBundleMultipartPayload {
    lab_id: Option<String>,
    lab_name: Option<String>,
    requested_by: Option<String>,
    image_name: Option<String>,
    image_tag: Option<String>,
    dockerfile_path: Option<String>,
    files: Vec<UploadedFileInput>,
}

pub async fn create_source_bundle(
    State(state): State<AppState>,
    multipart: Multipart,
) -> Result<Json<ApiResponse<SourceBundle>>, AppError> {
    let payload = parse_source_bundle_payload(multipart).await?;
    info!(
        lab_id = ?payload.lab_id,
        requested_by = ?payload.requested_by,
        file_count = payload.files.len(),
        "Creating source bundle from multipart upload"
    );

    let bundle = state
        .source_bundles_service
        .create_source_bundle(payload.lab_id, payload.requested_by, payload.files)
        .await?;

    Ok(Json(ApiResponse::success(bundle)))
}

pub async fn create_build_from_upload(
    State(state): State<AppState>,
    multipart: Multipart,
) -> Result<Json<ApiResponse<BuildFromUploadResponse>>, AppError> {
    let payload = parse_source_bundle_payload(multipart).await?;
    info!(
        lab_id = ?payload.lab_id,
        lab_name = ?payload.lab_name,
        requested_by = ?payload.requested_by,
        image_name = ?payload.image_name,
        image_tag = ?payload.image_tag,
        dockerfile_path = ?payload.dockerfile_path,
        file_count = payload.files.len(),
        "Received build-from-upload request"
    );

    let image_name = payload
        .image_name
        .or_else(|| {
            payload
                .lab_name
                .clone()
                .map(|value| normalize_image_name(&value))
        })
        .or_else(|| {
            payload
                .lab_id
                .clone()
                .map(|value| normalize_image_name(&value))
        })
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            AppError::BadRequest(
                "build upload requires image_name, lab_name or lab_id to derive template_path"
                    .into(),
            )
        })?;
    info!(image_name = %image_name, "Derived image name for build-from-upload request");

    let bundle = state
        .source_bundles_service
        .create_source_bundle(
            payload.lab_id.clone(),
            payload.requested_by.clone(),
            payload.files,
        )
        .await?;
    info!(
        bundle_id = %bundle.bundle_id,
        archive_path = %bundle.archive_path,
        archive_size_bytes = bundle.archive_size_bytes,
        workspace_dir = %bundle.workspace_dir,
        "Source bundle created for build-from-upload request"
    );

    let source_archive_path = if state.builds_service.is_local_mode() {
        bundle.archive_path.clone()
    } else {
        state
            .source_bundles_service
            .upload_source_bundle_to_gcs(&bundle)
            .await?
    };
    info!(
        source_archive_path = %source_archive_path,
        local_mode = state.builds_service.is_local_mode(),
        "Resolved source archive path for build"
    );

    let build_job = state
        .builds_service
        .create_build(CreateBuildRequest {
            lab_id: payload.lab_id,
            requested_by: payload.requested_by,
            image_name,
            image_tag: payload.image_tag,
            source_archive_path,
            dockerfile_path: payload.dockerfile_path,
        })
        .await?;
    info!(
        build_id = %build_job.build_id,
        template_path = %build_job.template_path,
        status = ?build_job.status,
        dispatch_mode = ?build_job.dispatch_mode,
        "Build-from-upload request accepted"
    );

    Ok(Json(ApiResponse::success(BuildFromUploadResponse {
        source_bundle: bundle,
        build_job,
    })))
}

async fn parse_source_bundle_payload(
    mut multipart: Multipart,
) -> Result<SourceBundleMultipartPayload, AppError> {
    let mut lab_id = None;
    let mut lab_name = None;
    let mut requested_by = None;
    let mut image_name = None;
    let mut image_tag = None;
    let mut dockerfile_path = None;
    let mut uploaded_files = Vec::new();

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|error| AppError::BadRequest(format!("Invalid multipart payload: {error}")))?
    {
        let field_name = field.name().unwrap_or("").to_string();
        let file_name = field.file_name().map(ToString::to_string);

        match (field_name.as_str(), file_name) {
            ("lab_id", None) => assign_text_field(&mut lab_id, field, "lab_id").await?,
            ("lab_name", None) => assign_text_field(&mut lab_name, field, "lab_name").await?,
            ("requested_by", None) => {
                assign_text_field(&mut requested_by, field, "requested_by").await?
            }
            ("image_name", None) => assign_text_field(&mut image_name, field, "image_name").await?,
            ("image_tag", None) => assign_text_field(&mut image_tag, field, "image_tag").await?,
            ("dockerfile_path", None) => {
                assign_text_field(&mut dockerfile_path, field, "dockerfile_path").await?
            }
            (_, Some(file_name)) => {
                let bytes = field.bytes().await.map_err(|error| {
                    AppError::BadRequest(format!("Failed to read uploaded file bytes: {error}"))
                })?;

                uploaded_files.push(UploadedFileInput {
                    relative_path: file_name,
                    bytes: bytes.to_vec(),
                });
            }
            _ => {}
        }
    }

    Ok(SourceBundleMultipartPayload {
        lab_id,
        lab_name,
        requested_by,
        image_name,
        image_tag,
        dockerfile_path,
        files: uploaded_files,
    })
}

async fn assign_text_field(
    target: &mut Option<String>,
    field: axum::extract::multipart::Field<'_>,
    field_name: &str,
) -> Result<(), AppError> {
    let value = field.text().await.map_err(|error| {
        AppError::BadRequest(format!("Failed to read {field_name} field: {error}"))
    })?;

    if !value.trim().is_empty() {
        *target = Some(value);
    }

    Ok(())
}

fn normalize_image_name(value: &str) -> String {
    let mut normalized = String::new();
    let mut previous_was_separator = false;

    for ch in value.trim().chars() {
        let mapped = if ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '.' {
            Some(ch)
        } else if ch.is_ascii_uppercase() {
            Some(ch.to_ascii_lowercase())
        } else {
            None
        };

        match mapped {
            Some(ch) => {
                normalized.push(ch);
                previous_was_separator = false;
            }
            None if !previous_was_separator => {
                normalized.push('-');
                previous_was_separator = true;
            }
            None => {}
        }
    }

    normalized.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::normalize_image_name;

    #[test]
    fn normalize_image_name_slugifies_lab_name() {
        assert_eq!(normalize_image_name("Lab SQLi Guided"), "lab-sqli-guided");
        assert_eq!(normalize_image_name("  CTF.Web 101  "), "ctf.web-101");
    }
}
