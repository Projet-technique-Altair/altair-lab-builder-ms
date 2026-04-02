use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use chrono::Utc;
use flate2::read::GzDecoder;
use reqwest::{Client, Url};
use serde::Deserialize;
use serde_json::{json, Value};
use tar::Archive;
use tokio::{fs, process::Command, sync::RwLock};
use uuid::Uuid;

use crate::{
    error::AppError,
    models::{
        build::{BuildDispatchMode, BuildJob, BuildStatus, CreateBuildRequest},
        state::BuilderConfig,
    },
};

const CLOUD_BUILD_API_BASE_URL: &str = "https://cloudbuild.googleapis.com/";

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
        self.validate_source_archive_path(&payload.source_archive_path)
            .await?;
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
        let versioned_image_uri = format!("{image_base}:{image_tag}");
        let latest_image_uri = format!("{image_base}:latest");
        let template_path = if self.config.local_mode {
            format!("{}:{}", payload.image_name, image_tag)
        } else {
            versioned_image_uri.clone()
        };

        let job = BuildJob {
            build_id,
            lab_id: payload.lab_id,
            requested_by: payload.requested_by,
            status: BuildStatus::Queued,
            dispatch_mode: if self.config.local_mode {
                BuildDispatchMode::LocalDockerKind
            } else {
                BuildDispatchMode::CloudBuild
            },
            image_name: payload.image_name,
            image_tag,
            template_path,
            source_archive_path: payload.source_archive_path,
            dockerfile_path,
            gcp_region: self.config.gcp_region.clone(),
            build_source_bucket: self.config.build_source_bucket.clone(),
            local_kind_cluster_name: if self.config.local_mode
                && self.config.local_kind_load_enabled
            {
                Some(self.config.local_kind_cluster_name.clone())
            } else {
                None
            },
            loaded_to_kind: false,
            cloud_build_id: None,
            cloud_build_name: None,
            cloud_build_operation_name: None,
            cloud_build_log_url: None,
            versioned_image_uri,
            latest_image_uri,
            created_at: Utc::now(),
        };

        let job = if self.config.local_mode {
            self.build_and_load_locally(job).await?
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

    async fn build_and_load_locally(&self, mut job: BuildJob) -> Result<BuildJob, AppError> {
        if !self.config.local_execution_enabled {
            job.status = BuildStatus::Ready;
            return Ok(job);
        }

        let build_context_dir = self
            .extract_archive_for_local_build(&job.source_archive_path, job.build_id)
            .await?;

        self.run_command(
            &self.config.local_docker_binary,
            &[
                "build",
                "-f",
                job.dockerfile_path.as_str(),
                "-t",
                job.template_path.as_str(),
                ".",
            ],
            Some(&build_context_dir),
        )
        .await?;

        if self.config.local_kind_load_enabled {
            self.run_command(
                &self.config.local_kind_binary,
                &[
                    "load",
                    "docker-image",
                    job.template_path.as_str(),
                    "--name",
                    self.config.local_kind_cluster_name.as_str(),
                ],
                None,
            )
            .await?;
            job.loaded_to_kind = true;
        }

        job.status = BuildStatus::Ready;
        Ok(job)
    }

    async fn extract_archive_for_local_build(
        &self,
        archive_path: &str,
        build_id: Uuid,
    ) -> Result<PathBuf, AppError> {
        let destination = Path::new(&self.config.bundle_root_dir)
            .join("local-build-contexts")
            .join(build_id.to_string());

        if fs::try_exists(&destination).await.map_err(|error| {
            AppError::Internal(format!("Failed to check local build context dir: {error}"))
        })? {
            fs::remove_dir_all(&destination).await.map_err(|error| {
                AppError::Internal(format!(
                    "Failed to clean previous local build context: {error}"
                ))
            })?;
        }

        fs::create_dir_all(&destination).await.map_err(|error| {
            AppError::Internal(format!("Failed to create local build context dir: {error}"))
        })?;

        let archive_path = archive_path.to_string();
        let destination_clone = destination.clone();
        tokio::task::spawn_blocking(move || -> Result<(), AppError> {
            let archive_file = std::fs::File::open(&archive_path).map_err(|error| {
                AppError::Internal(format!("Failed to open local source archive: {error}"))
            })?;
            let decoder = GzDecoder::new(archive_file);
            let mut archive = Archive::new(decoder);
            archive.unpack(&destination_clone).map_err(|error| {
                AppError::Internal(format!("Failed to extract local source archive: {error}"))
            })?;
            Ok(())
        })
        .await
        .map_err(|error| {
            AppError::Internal(format!("Local archive extraction task join error: {error}"))
        })??;

        Ok(destination)
    }

    async fn run_command(
        &self,
        program: &str,
        args: &[&str],
        current_dir: Option<&Path>,
    ) -> Result<(), AppError> {
        let mut command = Command::new(program);
        command.args(args);

        if let Some(current_dir) = current_dir {
            command.current_dir(current_dir);
        }

        let output = command.output().await.map_err(|error| {
            AppError::Internal(format!("Failed to start command `{program}`: {error}"))
        })?;

        if output.status.success() {
            return Ok(());
        }

        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let details = if !stderr.is_empty() {
            stderr
        } else if !stdout.is_empty() {
            stdout
        } else {
            format!("exit status {}", output.status)
        };

        Err(AppError::Internal(format!(
            "Command `{program} {}` failed: {details}",
            args.join(" ")
        )))
    }

    async fn submit_to_cloud_build(&self, mut job: BuildJob) -> Result<BuildJob, AppError> {
        let source = parse_gcs_uri(&job.source_archive_path)?;
        let build_request = self.build_cloud_build_request(&job, &source);
        let access_token = gcp_auth::provider()
            .await
            .map_err(|error| AppError::Internal(format!("Failed to initialize GCP auth: {error}")))?
            .token(&["https://www.googleapis.com/auth/cloud-platform"])
            .await
            .map_err(|error| {
                AppError::Internal(format!("Failed to obtain Cloud Build token: {error}"))
            })?;

        let endpoint = build_cloud_build_endpoint(
            &self.config.gcp_project_id,
            &self.config.gcp_region,
        )?;

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

    async fn validate_source_archive_path(&self, value: &str) -> Result<(), AppError> {
        let trimmed = value.trim();

        if trimmed.is_empty() {
            return Err(AppError::BadRequest(
                "source_archive_path must not be empty".into(),
            ));
        }

        if self.config.local_mode {
            if trimmed.starts_with("gs://") {
                return Err(AppError::BadRequest(
                    "source_archive_path must point to a local .tar.gz archive when LAB_BUILDER_LOCAL_MODE=true".into(),
                ));
            }

            let metadata = fs::metadata(trimmed).await.map_err(|error| {
                AppError::BadRequest(format!(
                    "source_archive_path must point to an existing local archive: {error}"
                ))
            })?;

            if !metadata.is_file() {
                return Err(AppError::BadRequest(
                    "source_archive_path must point to a file".into(),
                ));
            }

            if Path::new(trimmed)
                .extension()
                .and_then(|extension| extension.to_str())
                != Some("gz")
                || !trimmed.ends_with(".tar.gz")
            {
                return Err(AppError::BadRequest(
                    "source_archive_path must point to a .tar.gz archive".into(),
                ));
            }

            return Ok(());
        }

        validate_gcs_path(trimmed)
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
            "source_archive_path must start with gs://".into(),
        ))
    }
}

fn validate_gcp_project_id(value: &str) -> Result<(), AppError> {
    let trimmed = value.trim();

    if trimmed.is_empty() {
        return Err(AppError::Internal("GCP project id must not be empty".into()));
    }

    if !(6..=30).contains(&trimmed.len()) {
        return Err(AppError::Internal(
            "GCP project id must be between 6 and 30 characters".into(),
        ));
    }

    if !trimmed
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
    {
        return Err(AppError::Internal(
            "GCP project id contains forbidden characters".into(),
        ));
    }

    if !trimmed
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_lowercase())
    {
        return Err(AppError::Internal(
            "GCP project id must start with a lowercase letter".into(),
        ));
    }

    if !trimmed
        .chars()
        .last()
        .is_some_and(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit())
    {
        return Err(AppError::Internal(
            "GCP project id must end with a lowercase letter or digit".into(),
        ));
    }

    Ok(())
}

fn validate_gcp_region(value: &str) -> Result<(), AppError> {
    let trimmed = value.trim();

    if trimmed.is_empty() {
        return Err(AppError::Internal("GCP region must not be empty".into()));
    }

    if trimmed.len() > 32 {
        return Err(AppError::Internal("GCP region is too long".into()));
    }

    if !trimmed
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
    {
        return Err(AppError::Internal("GCP region contains forbidden characters".into()));
    }

    if trimmed.starts_with('-') || trimmed.ends_with('-') || !trimmed.contains('-') {
        return Err(AppError::Internal("GCP region has an invalid format".into()));
    }

    Ok(())
}

fn build_cloud_build_endpoint(project_id: &str, region: &str) -> Result<Url, AppError> {
    validate_gcp_project_id(project_id)?;
    validate_gcp_region(region)?;

    let mut endpoint = Url::parse(CLOUD_BUILD_API_BASE_URL)
        .map_err(|error| AppError::Internal(format!("Invalid Cloud Build base URL: {error}")))?;

    {
        let mut segments = endpoint.path_segments_mut().map_err(|_| {
            AppError::Internal("Cloud Build base URL cannot accept path segments".into())
        })?;
        segments.extend(["v1", "projects", project_id, "locations", region, "builds"]);
    }

    endpoint
        .query_pairs_mut()
        .append_pair("projectId", project_id);

    Ok(endpoint)
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
    let without_prefix = trimmed
        .strip_prefix("gs://")
        .ok_or_else(|| AppError::BadRequest("source_archive_path must start with gs://".into()))?;
    let (bucket, object) = without_prefix.split_once('/').ok_or_else(|| {
        AppError::BadRequest(
            "source_archive_path must include both a bucket and an object path".into(),
        )
    })?;

    if bucket.is_empty() || object.is_empty() {
        return Err(AppError::BadRequest(
            "source_archive_path must include both a bucket and an object path".into(),
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
    use std::{
        collections::HashMap,
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    };

    use tokio::sync::RwLock;

    use crate::models::{
        build::{BuildDispatchMode, BuildStatus, CreateBuildRequest},
        state::BuilderConfig,
    };

    use super::{
        build_cloud_build_endpoint, normalized_service_account, parse_gcs_uri,
        validate_gcp_project_id, validate_gcp_region, BuildsService,
    };

    fn test_config() -> BuilderConfig {
        BuilderConfig {
            gcp_project_id: "altair-isen".into(),
            gcp_region: "europe-west9".into(),
            artifact_registry_host: "europe-west9-docker.pkg.dev".into(),
            artifact_registry_repo: "altair-labs".into(),
            build_source_bucket: "altair-lab-builds".into(),
            bundle_root_dir: "/tmp/altair-lab-builder".into(),
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

    #[tokio::test]
    async fn create_build_accepts_local_archive_in_local_mode() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be valid")
            .as_nanos();
        let archive_path = std::env::temp_dir().join(format!("lab-builder-build-{unique}.tar.gz"));
        std::fs::write(&archive_path, b"fake tar gz payload").expect("archive should be written");

        let service = BuildsService::new(test_config(), Arc::new(RwLock::new(HashMap::new())));

        let job = service
            .create_build(CreateBuildRequest {
                lab_id: Some("lab-local".into()),
                requested_by: Some("creator-local".into()),
                image_name: "lab-local".into(),
                image_tag: None,
                source_archive_path: archive_path.display().to_string(),
                dockerfile_path: Some("Dockerfile".into()),
            })
            .await
            .expect("local archive should be accepted");

        assert_eq!(job.dispatch_mode, BuildDispatchMode::LocalDockerKind);
        assert_eq!(job.status, BuildStatus::Ready);
        assert_eq!(job.template_path, "lab-local:v1");
        assert_eq!(
            job.versioned_image_uri,
            "europe-west9-docker.pkg.dev/altair-isen/altair-labs/lab-local:v1"
        );
        assert_eq!(job.source_archive_path, archive_path.display().to_string());
        assert!(!job.loaded_to_kind);

        let _ = std::fs::remove_file(archive_path);
    }

    #[tokio::test]
    async fn create_build_rejects_gcs_source_in_local_mode() {
        let service = BuildsService::new(test_config(), Arc::new(RwLock::new(HashMap::new())));

        let result = service
            .create_build(CreateBuildRequest {
                lab_id: None,
                requested_by: None,
                image_name: "lab-poc-1".into(),
                image_tag: None,
                source_archive_path: "gs://bucket/lab/source.tar.gz".into(),
                dockerfile_path: None,
            })
            .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn create_build_uses_cloud_build_in_non_local_mode() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be valid")
            .as_nanos();
        let archive_path = std::env::temp_dir().join(format!("lab-builder-cloud-{unique}.tar.gz"));
        std::fs::write(&archive_path, b"fake tar gz payload").expect("archive should be written");

        let mut config = test_config();
        config.local_mode = false;
        let service = BuildsService::new(config, Arc::new(RwLock::new(HashMap::new())));

        let result = service
            .create_build(CreateBuildRequest {
                lab_id: None,
                requested_by: None,
                image_name: "lab-poc-1".into(),
                image_tag: None,
                source_archive_path: archive_path.display().to_string(),
                dockerfile_path: None,
            })
            .await;

        assert!(result.is_err());
        let _ = std::fs::remove_file(archive_path);
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

    #[test]
    fn validate_gcp_project_id_accepts_standard_project_id() {
        assert!(validate_gcp_project_id("altair-isen").is_ok());
    }

    #[test]
    fn validate_gcp_project_id_rejects_forbidden_characters() {
        assert!(validate_gcp_project_id("altair-isen/evil").is_err());
    }

    #[test]
    fn validate_gcp_region_accepts_standard_region() {
        assert!(validate_gcp_region("europe-west9").is_ok());
    }

    #[test]
    fn validate_gcp_region_rejects_invalid_format() {
        assert!(validate_gcp_region("https://evil.example").is_err());
    }

    #[test]
    fn build_cloud_build_endpoint_uses_fixed_google_host() {
        let endpoint = build_cloud_build_endpoint("altair-isen", "europe-west9")
            .expect("endpoint should be valid");

        assert_eq!(endpoint.scheme(), "https");
        assert_eq!(endpoint.host_str(), Some("cloudbuild.googleapis.com"));
        assert_eq!(
            endpoint.as_str(),
            "https://cloudbuild.googleapis.com/v1/projects/altair-isen/locations/europe-west9/builds?projectId=altair-isen"
        );
    }
}
