/**
 * @file source_bundles — HTTP routes for source upload and build triggering.
 *
 * @remarks
 * Exposes endpoints for handling multipart uploads of lab sources
 * and optionally triggering build jobs from those uploads.
 *
 * Endpoints:
 *
 *  - `POST /source-bundles` → create a source bundle from uploaded files
 *  - `POST /builds/from-upload` → upload files and immediately trigger a build
 *
 * Key characteristics:
 *
 *  - Parses multipart form-data (files + metadata fields)
 *  - Supports flexible input (lab_id, lab_name, or image_name)
 *  - Automatically derives a valid image name when not provided
 *  - Integrates with both `SourceBundlesService` and `BuildsService`
 *  - Adapts behavior depending on execution mode (local vs cloud)
 *
 * Features:
 *
 *  - Secure handling of uploaded files (delegated to service layer)
 *  - Automatic packaging into build-ready archives
 *  - Optional upload to GCS when running in cloud mode
 *  - Unified API responses (`ApiResponse<T>`)
 *
 * This module acts as the entry point for user-provided lab content,
 * bridging file uploads with the build pipeline in a single flow.
 *
 * @packageDocumentation
 */
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
    services::{file_policy::is_allowed_upload_name, source_bundles::UploadedFileInput},
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
    let payload = parse_source_bundle_payload(&state, multipart).await?;
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
    let payload = parse_source_bundle_payload(&state, multipart).await?;
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
    state: &AppState,
    mut multipart: Multipart,
) -> Result<SourceBundleMultipartPayload, AppError> {
    let mut lab_id = None;
    let mut lab_name = None;
    let mut requested_by = None;
    let mut image_name = None;
    let mut image_tag = None;
    let mut dockerfile_path = None;
    let mut uploaded_files = Vec::new();
    let mut total_upload_bytes = 0_usize;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|error| AppError::BadRequest(format!("Invalid multipart payload: {error}")))?
    {
        let field_name = field.name().unwrap_or("").to_string();
        let file_name = field.file_name().map(ToString::to_string);

        match (field_name.as_str(), file_name) {
            ("lab_id", None) => {
                assign_text_field(
                    &mut lab_id,
                    field,
                    "lab_id",
                    state.source_bundles_service.max_text_field_bytes(),
                )
                .await?
            }
            ("lab_name", None) => {
                assign_text_field(
                    &mut lab_name,
                    field,
                    "lab_name",
                    state.source_bundles_service.max_text_field_bytes(),
                )
                .await?
            }
            ("requested_by", None) => {
                assign_text_field(
                    &mut requested_by,
                    field,
                    "requested_by",
                    state.source_bundles_service.max_text_field_bytes(),
                )
                .await?
            }
            ("image_name", None) => {
                assign_text_field(
                    &mut image_name,
                    field,
                    "image_name",
                    state.source_bundles_service.max_text_field_bytes(),
                )
                .await?
            }
            ("image_tag", None) => {
                assign_text_field(
                    &mut image_tag,
                    field,
                    "image_tag",
                    state.source_bundles_service.max_text_field_bytes(),
                )
                .await?
            }
            ("dockerfile_path", None) => {
                assign_text_field(
                    &mut dockerfile_path,
                    field,
                    "dockerfile_path",
                    state.source_bundles_service.max_text_field_bytes(),
                )
                .await?
            }
            (_, Some(file_name)) => {
                if !is_allowed_upload_name(&file_name) {
                    return Err(AppError::BadRequest(format!(
                        "Unsupported uploaded file type: {file_name}"
                    )));
                }

                if uploaded_files.len() >= state.source_bundles_service.max_upload_files() {
                    return Err(AppError::BadRequest(
                        "Upload contains too many files".into(),
                    ));
                }

                let bytes = read_limited_field_bytes(
                    field,
                    state.source_bundles_service.max_upload_file_bytes(),
                    state.source_bundles_service.max_upload_total_bytes(),
                    &mut total_upload_bytes,
                )
                .await?;

                uploaded_files.push(UploadedFileInput {
                    relative_path: file_name,
                    bytes,
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
    max_bytes: usize,
) -> Result<(), AppError> {
    let bytes = read_limited_text_field(field, field_name, max_bytes).await?;
    let value = String::from_utf8(bytes)
        .map_err(|_| AppError::BadRequest(format!("{field_name} must be valid UTF-8")))?;

    if !value.trim().is_empty() {
        *target = Some(value);
    }

    Ok(())
}

async fn read_limited_text_field(
    mut field: axum::extract::multipart::Field<'_>,
    field_name: &str,
    max_bytes: usize,
) -> Result<Vec<u8>, AppError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = field
        .chunk()
        .await
        .map_err(|error| AppError::BadRequest(format!("Failed to read {field_name}: {error}")))?
    {
        if bytes.len().saturating_add(chunk.len()) > max_bytes {
            return Err(AppError::BadRequest(format!("{field_name} is too large")));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn read_limited_field_bytes(
    mut field: axum::extract::multipart::Field<'_>,
    max_file_bytes: usize,
    max_total_bytes: usize,
    total_upload_bytes: &mut usize,
) -> Result<Vec<u8>, AppError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = field.chunk().await.map_err(|error| {
        AppError::BadRequest(format!("Failed to read uploaded file bytes: {error}"))
    })? {
        if bytes.len().saturating_add(chunk.len()) > max_file_bytes {
            return Err(AppError::BadRequest(
                "Uploaded file exceeds the configured size limit".into(),
            ));
        }

        if (*total_upload_bytes).saturating_add(chunk.len()) > max_total_bytes {
            return Err(AppError::BadRequest(
                "Upload exceeds the configured total size limit".into(),
            ));
        }

        *total_upload_bytes = (*total_upload_bytes).saturating_add(chunk.len());
        bytes.extend_from_slice(&chunk);
    }

    Ok(bytes)
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
