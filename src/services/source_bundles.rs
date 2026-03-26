use std::{
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use chrono::Utc;
use flate2::{write::GzEncoder, Compression};
use reqwest::Client;
use tar::Builder;
use tokio::fs;
use uuid::Uuid;

use crate::{
    error::AppError,
    models::{
        source_bundle::{SourceBundle, UploadedFile},
        state::BuilderConfig,
    },
};

#[derive(Debug, Clone)]
pub struct UploadedFileInput {
    pub relative_path: String,
    pub bytes: Vec<u8>,
}

#[derive(Clone)]
pub struct SourceBundlesService {
    config: Arc<BuilderConfig>,
    http_client: Client,
}

impl SourceBundlesService {
    pub fn new(config: BuilderConfig) -> Self {
        Self {
            config: Arc::new(config),
            http_client: Client::new(),
        }
    }

    pub async fn create_source_bundle(
        &self,
        lab_id: Option<String>,
        requested_by: Option<String>,
        files: Vec<UploadedFileInput>,
    ) -> Result<SourceBundle, AppError> {
        if files.is_empty() {
            return Err(AppError::BadRequest(
                "At least one uploaded file is required".into(),
            ));
        }

        let bundle_id = Uuid::new_v4();
        let bundle_root = Path::new(&self.config.bundle_root_dir).join(bundle_id.to_string());
        let workspace_dir = bundle_root.join("workspace");
        let artifacts_dir = bundle_root.join("artifacts");
        let archive_path = artifacts_dir.join("source.tar.gz");

        fs::create_dir_all(&workspace_dir).await.map_err(|error| {
            AppError::Internal(format!("Failed to create workspace dir: {error}"))
        })?;
        fs::create_dir_all(&artifacts_dir).await.map_err(|error| {
            AppError::Internal(format!("Failed to create artifacts dir: {error}"))
        })?;

        let mut stored_files = Vec::with_capacity(files.len());

        for file in files {
            let sanitized_path = sanitize_relative_path(&file.relative_path)?;
            let destination = workspace_dir.join(&sanitized_path);

            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent).await.map_err(|error| {
                    AppError::Internal(format!("Failed to create parent dir for upload: {error}"))
                })?;
            }

            fs::write(&destination, &file.bytes)
                .await
                .map_err(|error| {
                    AppError::Internal(format!("Failed to persist uploaded file: {error}"))
                })?;

            stored_files.push(UploadedFile {
                path: sanitized_path.display().to_string(),
                size_bytes: file.bytes.len() as u64,
            });
        }

        create_tar_gz_archive(workspace_dir.clone(), archive_path.clone()).await?;

        let archive_metadata = fs::metadata(&archive_path)
            .await
            .map_err(|error| AppError::Internal(format!("Failed to stat archive: {error}")))?;

        let bundle_key = lab_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("anonymous-lab");
        let suggested_gcs_path = format!(
            "gs://{}/builds/{}/{}/source.tar.gz",
            self.config.build_source_bucket, bundle_key, bundle_id
        );

        Ok(SourceBundle {
            bundle_id,
            lab_id,
            requested_by,
            workspace_dir: workspace_dir.display().to_string(),
            archive_path: archive_path.display().to_string(),
            suggested_gcs_path,
            archive_size_bytes: archive_metadata.len(),
            file_count: stored_files.len(),
            files: stored_files,
            created_at: Utc::now(),
        })
    }

    pub async fn upload_source_bundle_to_gcs(
        &self,
        bundle: &SourceBundle,
    ) -> Result<String, AppError> {
        let (bucket, object) = parse_gcs_uri(&bundle.suggested_gcs_path)?;
        let archive_bytes = fs::read(&bundle.archive_path).await.map_err(|error| {
            AppError::Internal(format!(
                "Failed to read local archive for GCS upload: {error}"
            ))
        })?;

        let access_token = gcp_auth::provider()
            .await
            .map_err(|error| AppError::Internal(format!("Failed to initialize GCP auth: {error}")))?
            .token(&["https://www.googleapis.com/auth/cloud-platform"])
            .await
            .map_err(|error| {
                AppError::Internal(format!("Failed to obtain GCS upload token: {error}"))
            })?;

        let endpoint = format!(
            "https://storage.googleapis.com/upload/storage/v1/b/{bucket}/o?uploadType=media&name={}",
            urlencoding::encode(&object)
        );

        let response = self
            .http_client
            .post(endpoint)
            .bearer_auth(access_token.as_str())
            .header("Content-Type", "application/gzip")
            .body(archive_bytes)
            .send()
            .await
            .map_err(|error| {
                AppError::Internal(format!("Failed to call GCS upload API: {error}"))
            })?;

        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "<unavailable body>".to_string());
            return Err(AppError::Internal(format!(
                "GCS upload failed for {}: HTTP {} - {}",
                bundle.suggested_gcs_path, status, body
            )));
        }

        Ok(bundle.suggested_gcs_path.clone())
    }
}

async fn create_tar_gz_archive(
    workspace_dir: PathBuf,
    archive_path: PathBuf,
) -> Result<(), AppError> {
    tokio::task::spawn_blocking(move || -> Result<(), AppError> {
        let archive_file = std::fs::File::create(&archive_path).map_err(|error| {
            AppError::Internal(format!("Failed to create archive file: {error}"))
        })?;
        let encoder = GzEncoder::new(archive_file, Compression::default());
        let mut builder = Builder::new(encoder);

        builder
            .append_dir_all(".", &workspace_dir)
            .map_err(|error| {
                AppError::Internal(format!("Failed to append files to archive: {error}"))
            })?;

        let encoder = builder.into_inner().map_err(|error| {
            AppError::Internal(format!("Failed to finalize tar archive: {error}"))
        })?;

        encoder.finish().map_err(|error| {
            AppError::Internal(format!("Failed to finish gzip archive: {error}"))
        })?;

        Ok(())
    })
    .await
    .map_err(|error| AppError::Internal(format!("Archive task join error: {error}")))?
}

fn sanitize_relative_path(value: &str) -> Result<PathBuf, AppError> {
    let normalized = value.trim().replace('\\', "/");
    if normalized.is_empty() {
        return Err(AppError::BadRequest(
            "Uploaded file name must not be empty".into(),
        ));
    }

    let candidate = Path::new(&normalized);
    if candidate.is_absolute() {
        return Err(AppError::BadRequest(
            "Uploaded file path must be relative".into(),
        ));
    }

    let mut sanitized = PathBuf::new();

    for component in candidate.components() {
        match component {
            Component::Normal(segment) => sanitized.push(segment),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(AppError::BadRequest(
                    "Uploaded file path contains forbidden path traversal".into(),
                ))
            }
        }
    }

    if sanitized.as_os_str().is_empty() {
        return Err(AppError::BadRequest(
            "Uploaded file path resolved to an empty value".into(),
        ));
    }

    Ok(sanitized)
}

fn parse_gcs_uri(value: &str) -> Result<(String, String), AppError> {
    let trimmed = value.trim();
    let without_prefix = trimmed
        .strip_prefix("gs://")
        .ok_or_else(|| AppError::Internal(format!("Invalid suggested GCS path: {trimmed}")))?;
    let (bucket, object) = without_prefix
        .split_once('/')
        .ok_or_else(|| AppError::Internal(format!("Invalid suggested GCS path: {trimmed}")))?;

    if bucket.is_empty() || object.is_empty() {
        return Err(AppError::Internal(format!(
            "Invalid suggested GCS path: {trimmed}"
        )));
    }

    Ok((bucket.to_string(), object.to_string()))
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::models::state::BuilderConfig;

    use super::{sanitize_relative_path, SourceBundlesService, UploadedFileInput};

    fn test_config(bundle_root_dir: String) -> BuilderConfig {
        BuilderConfig {
            gcp_project_id: "altair-isen".into(),
            gcp_region: "europe-west9".into(),
            artifact_registry_host: "europe-west9-docker.pkg.dev".into(),
            artifact_registry_repo: "altair-labs".into(),
            build_source_bucket: "altair-lab-builds".into(),
            bundle_root_dir,
            cloud_build_timeout_seconds: 1200,
            cloud_build_service_account: None,
            cloud_build_logs_bucket: None,
            local_execution_enabled: false,
            local_docker_binary: "docker".into(),
            local_kind_binary: "kind".into(),
            local_kind_cluster_name: "kind".into(),
            local_kind_load_enabled: true,
            local_mode: true,
        }
    }

    #[test]
    fn sanitize_relative_path_rejects_parent_traversal() {
        let result = sanitize_relative_path("../etc/passwd");
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn create_source_bundle_writes_archive() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be valid")
            .as_nanos();
        let bundle_root = std::env::temp_dir().join(format!("lab-builder-test-{unique}"));
        let service = SourceBundlesService::new(test_config(bundle_root.display().to_string()));

        let bundle = service
            .create_source_bundle(
                Some("lab-1".into()),
                Some("creator-1".into()),
                vec![
                    UploadedFileInput {
                        relative_path: "Dockerfile".into(),
                        bytes: b"FROM debian:bookworm-slim\n".to_vec(),
                    },
                    UploadedFileInput {
                        relative_path: "app/start.sh".into(),
                        bytes: b"#!/bin/sh\necho hello\n".to_vec(),
                    },
                ],
            )
            .await
            .expect("bundle creation should succeed");

        assert_eq!(bundle.file_count, 2);
        assert!(Path::new(&bundle.archive_path).exists());
        assert_eq!(
            bundle.suggested_gcs_path,
            format!(
                "gs://altair-lab-builds/builds/lab-1/{}/source.tar.gz",
                bundle.bundle_id
            )
        );

        let _ = std::fs::remove_dir_all(bundle_root);
    }
}
