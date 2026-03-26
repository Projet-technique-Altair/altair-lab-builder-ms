use std::{collections::HashMap, sync::Arc};

use chrono::Utc;
use reqwest::Client;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::{
    error::AppError,
    models::{
        build::{BuildDispatchMode, BuildJob, BuildStatus, CreateBuildRequest},
        state::BuilderConfig,
    },
};

#[derive(Clone)]
pub struct BuildsService {
    config: BuilderConfig,
    jobs: Arc<RwLock<HashMap<Uuid, BuildJob>>>,
    http_client: Client,
}

impl BuildsService {
    pub fn new(config: BuilderConfig, jobs: Arc<RwLock<HashMap<Uuid, BuildJob>>>) -> Self {
        Self {
            config,
            jobs,
            http_client: Client::new(),
        }
    }

    pub fn is_local_mode(&self) -> bool {
        self.config.local_mode
    }

    pub async fn create_build(&self, payload: CreateBuildRequest) -> Result<BuildJob, AppError> {
        validate_gcs_path(&payload.source_archive_gcs_path)?;
        validate_image_name(&payload.image_name)?;

        let build_id = Uuid::new_v4();
        let image_tag = payload
            .image_tag
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("v1")
            .to_string();
        let dockerfile_path = payload
            .dockerfile_path
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("Dockerfile")
            .to_string();

        let image_base = format!(
            "{}/{}/{}/{}",
            self.config.artifact_registry_host,
            self.config.gcp_project_id,
            self.config.artifact_registry_repo,
            payload.image_name
        );

        let job = BuildJob {
            build_id,
            lab_id: payload.lab_id,
            requested_by: payload.requested_by,
            status: BuildStatus::Queued,
            dispatch_mode: if self.config.local_mode {
                BuildDispatchMode::Stub
            } else {
                BuildDispatchMode::CloudBuild
            },
            image_name: payload.image_name,
            image_tag: image_tag.clone(),
            source_archive_gcs_path: payload.source_archive_gcs_path,
            dockerfile_path,
            gcp_region: self.config.gcp_region.clone(),
            build_source_bucket: self.config.build_source_bucket.clone(),
            cloud_build_id: None,
            cloud_build_name: None,
            cloud_build_operation_name: None,
            cloud_build_log_url: None,
            versioned_image_uri: format!("{image_base}:{image_tag}"),
            latest_image_uri: format!("{image_base}:latest"),
            created_at: Utc::now(),
        };

        let job = if self.config.local_mode {
            job
        } else {
            self.submit_to_cloud_build(job).await?
        };

        self.jobs.write().await.insert(build_id, job.clone());
        Ok(job)
    }

    pub async fn get_build(&self, build_id: Uuid) -> Result<BuildJob, AppError> {
        self.jobs
            .read()
            .await
            .get(&build_id)
            .cloned()
            .ok_or_else(|| AppError::NotFound(format!("Build job {build_id} not found")))
    }

    async fn submit_to_cloud_build(&self, mut job: BuildJob) -> Result<BuildJob, AppError> {
        let source = parse_gcs_uri(&job.source_archive_gcs_path)?;
        let build_request = self.build_cloud_build_request(&job, &source);
        let access_token = gcp_auth::provider()
            .await
            .map_err(|error| AppError::Internal(format!("Failed to initialize GCP auth: {error}")))?
            .token(&["https://www.googleapis.com/auth/cloud-platform"])
            .await
            .map_err(|error| {
                AppError::Internal(format!("Failed to obtain Cloud Build token: {error}"))
            })?;

        let endpoint = format!(
            "https://cloudbuild.googleapis.com/v1/projects/{}/locations/{}/builds?projectId={}",
            self.config.gcp_project_id, self.config.gcp_region, self.config.gcp_project_id
        );

        let response = self
            .http_client
            .post(endpoint)
            .bearer_auth(access_token.as_str())
            .json(&build_request)
            .send()
            .await
            .map_err(|error| {
                AppError::Internal(format!("Failed to call Cloud Build API: {error}"))
            })?;

        let status = response.status();
        let payload = response
            .json::<CloudBuildOperation>()
            .await
            .map_err(|error| {
                AppError::Internal(format!("Failed to decode Cloud Build response: {error}"))
            })?;

        if !status.is_success() {
            let details = payload.error_message();
            return Err(AppError::Internal(format!(
                "Cloud Build API rejected the build request: {}",
                details.unwrap_or_else(|| format!("HTTP {status}"))
            )));
        }

        job.status = BuildStatus::Submitted;
        job.cloud_build_operation_name = payload.name.clone();
        job.cloud_build_id = payload.build_id();
        job.cloud_build_name = payload.build_name();
        job.cloud_build_log_url = payload.build_log_url();

        Ok(job)
    }

    fn build_cloud_build_request(&self, job: &BuildJob, source: &StorageSource) -> Value {
        let mut build = json!({
            "source": {
                "storageSource": {
                    "bucket": source.bucket,
                    "object": source.object
                }
            },
            "steps": [{
                "name": "gcr.io/cloud-builders/docker",
                "args": [
                    "build",
                    "-f",
                    job.dockerfile_path,
                    "-t",
                    job.versioned_image_uri,
                    "-t",
                    job.latest_image_uri,
                    "."
                ]
            }],
            "images": [
                job.versioned_image_uri,
                job.latest_image_uri
            ],
            "timeout": format!("{}s", self.config.cloud_build_timeout_seconds),
            "options": {
                "logging": "CLOUD_LOGGING_ONLY"
            },
            "tags": [
                "altair",
                "lab-builder",
                job.image_name
            ]
        });

        if let Some(service_account) = normalized_service_account(
            &self.config.gcp_project_id,
            self.config.cloud_build_service_account.as_deref(),
        ) {
            build["serviceAccount"] = Value::String(service_account);
        }

        if let Some(logs_bucket) = &self.config.cloud_build_logs_bucket {
            build["logsBucket"] = Value::String(logs_bucket.clone());
        }

        build
    }
}

#[derive(Debug)]
struct StorageSource {
    bucket: String,
    object: String,
}

#[derive(Debug, Deserialize)]
struct CloudBuildOperation {
    name: Option<String>,
    metadata: Option<Value>,
    error: Option<CloudBuildApiError>,
}

#[derive(Debug, Deserialize)]
struct CloudBuildApiError {
    message: Option<String>,
}

impl CloudBuildOperation {
    fn error_message(&self) -> Option<String> {
        self.error.as_ref().and_then(|error| error.message.clone())
    }

    fn build_id(&self) -> Option<String> {
        metadata_string(self.metadata.as_ref(), &["build", "id"])
    }

    fn build_name(&self) -> Option<String> {
        metadata_string(self.metadata.as_ref(), &["build", "name"])
    }

    fn build_log_url(&self) -> Option<String> {
        metadata_string(self.metadata.as_ref(), &["build", "logUrl"])
    }
}

fn validate_gcs_path(value: &str) -> Result<(), AppError> {
    if value.trim().starts_with("gs://") {
        Ok(())
    } else {
        Err(AppError::BadRequest(
            "source_archive_gcs_path must start with gs://".into(),
        ))
    }
}

fn validate_image_name(value: &str) -> Result<(), AppError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(AppError::BadRequest("image_name must not be empty".into()));
    }

    let valid = trimmed
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '-' | '.'));

    if valid {
        Ok(())
    } else {
        Err(AppError::BadRequest(
            "image_name must contain only lowercase letters, digits, '-' or '.'".into(),
        ))
    }
}

fn parse_gcs_uri(value: &str) -> Result<StorageSource, AppError> {
    let trimmed = value.trim();
    let without_prefix = trimmed.strip_prefix("gs://").ok_or_else(|| {
        AppError::BadRequest("source_archive_gcs_path must start with gs://".into())
    })?;
    let (bucket, object) = without_prefix.split_once('/').ok_or_else(|| {
        AppError::BadRequest(
            "source_archive_gcs_path must include both a bucket and an object path".into(),
        )
    })?;

    if bucket.is_empty() || object.is_empty() {
        return Err(AppError::BadRequest(
            "source_archive_gcs_path must include both a bucket and an object path".into(),
        ));
    }

    Ok(StorageSource {
        bucket: bucket.to_string(),
        object: object.to_string(),
    })
}

fn normalized_service_account(project_id: &str, value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            if value.starts_with("projects/") {
                value.to_string()
            } else {
                format!("projects/{project_id}/serviceAccounts/{value}")
            }
        })
}

fn metadata_string(metadata: Option<&Value>, path: &[&str]) -> Option<String> {
    let mut current = metadata?;

    for segment in path {
        current = current.get(*segment)?;
    }

    current.as_str().map(ToString::to_string)
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use tokio::sync::RwLock;

    use crate::models::{
        build::{BuildDispatchMode, CreateBuildRequest},
        state::BuilderConfig,
    };

    use super::{normalized_service_account, parse_gcs_uri, BuildsService};

    fn test_config() -> BuilderConfig {
        BuilderConfig {
            gcp_project_id: "altair-isen".into(),
            gcp_region: "europe-west9".into(),
            artifact_registry_host: "europe-west9-docker.pkg.dev".into(),
            artifact_registry_repo: "altair-repo".into(),
            build_source_bucket: "altair-lab-builds".into(),
            cloud_build_timeout_seconds: 1200,
            cloud_build_service_account: None,
            cloud_build_logs_bucket: None,
            local_mode: true,
        }
    }

    #[tokio::test]
    async fn create_build_computes_image_uris() {
        let service = BuildsService::new(test_config(), Arc::new(RwLock::new(HashMap::new())));

        let job = service
            .create_build(CreateBuildRequest {
                lab_id: Some("lab-1".into()),
                requested_by: Some("creator-1".into()),
                image_name: "lab-poc-1".into(),
                image_tag: Some("v7".into()),
                source_archive_gcs_path: "gs://bucket/lab/source.tar.gz".into(),
                dockerfile_path: None,
            })
            .await
            .expect("build creation should succeed");

        assert_eq!(
            job.versioned_image_uri,
            "europe-west9-docker.pkg.dev/altair-isen/altair-repo/lab-poc-1:v7"
        );
        assert_eq!(
            job.latest_image_uri,
            "europe-west9-docker.pkg.dev/altair-isen/altair-repo/lab-poc-1:latest"
        );
        assert_eq!(job.dispatch_mode, BuildDispatchMode::Stub);
        assert_eq!(job.gcp_region, "europe-west9");
        assert_eq!(job.build_source_bucket, "altair-lab-builds");
        assert_eq!(job.status, crate::models::build::BuildStatus::Queued);
    }

    #[tokio::test]
    async fn create_build_rejects_non_gcs_source() {
        let service = BuildsService::new(test_config(), Arc::new(RwLock::new(HashMap::new())));

        let result = service
            .create_build(CreateBuildRequest {
                lab_id: None,
                requested_by: None,
                image_name: "lab-poc-1".into(),
                image_tag: None,
                source_archive_gcs_path: "/tmp/source.tar.gz".into(),
                dockerfile_path: None,
            })
            .await;

        assert!(result.is_err());
    }

    #[test]
    fn parse_gcs_uri_extracts_bucket_and_object() {
        let parsed = parse_gcs_uri("gs://altair-lab-builds/builds/lab-1/v1/source.tar.gz")
            .expect("gcs uri should parse");

        assert_eq!(parsed.bucket, "altair-lab-builds");
        assert_eq!(parsed.object, "builds/lab-1/v1/source.tar.gz");
    }

    #[test]
    fn normalized_service_account_accepts_email() {
        let value = normalized_service_account(
            "altair-isen",
            Some("build-sa@altair-isen.iam.gserviceaccount.com"),
        )
        .expect("service account should be normalized");

        assert_eq!(
            value,
            "projects/altair-isen/serviceAccounts/build-sa@altair-isen.iam.gserviceaccount.com"
        );
    }
}
