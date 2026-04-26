/**
 * @file builds — build orchestration service.
 *
 * @remarks
 * Implements the core logic for creating, executing, and tracking
 * lab build jobs across different execution environments.
 *
 * Responsibilities:
 *
 *  - Build job creation and validation (`create_build`)
 *  - Local build execution (Docker + Kind)
 *  - Cloud build submission (Google Cloud Build)
 *  - Asynchronous job tracking and state updates
 *
 * Key features:
 *
 *  - Dual execution mode:
 *      • Local (Docker + Kind cluster)
 *      • Cloud (GCP Cloud Build)
 *  - Secure handling of source archives (path validation, sandboxing)
 *  - Full lifecycle tracking (queued → submitted → ready/failed)
 *  - Background processing using async tasks
 *  - Integration with external services (GCP APIs, local CLI tools)
 *
 * Also includes:
 *
 *  - Helpers for GCS URI parsing and validation
 *  - Cloud Build API request/response handling
 *  - Command execution wrappers for local builds
 *  - Polling logic for long-running cloud operations
 *
 * This service is the backbone of the Lab Builder system,
 * transforming uploaded lab sources into runnable container images.
 *
 * @packageDocumentation
 */

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
use tokio::{fs, process::Command, sync::RwLock, time::{sleep, Duration}};
use tracing::info;
use uuid::Uuid;

use crate::{
    error::AppError,
    models::{
        build::{BuildDispatchMode, BuildJob, BuildStatus, CreateBuildRequest},
        state::BuilderConfig,
    },
    services::path_safety::{ensure_builder_root_dir, join_relative_to_root, resolve_existing_path_within_root},
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
        info!(
            lab_id = ?payload.lab_id,
            requested_by = ?payload.requested_by,
            image_name = %payload.image_name,
            image_tag = ?payload.image_tag,
            source_archive_path = %payload.source_archive_path,
            dockerfile_path = ?payload.dockerfile_path,
            local_mode = self.config.local_mode,
            "Creating build job"
        );
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
            status: if self.config.local_mode {
                BuildStatus::Submitted
            } else {
                BuildStatus::Queued
            },
            failure_message: None,
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

        if self.config.local_mode {
            self.store_job(job.clone()).await;
            self.spawn_local_build(job.clone());
            info!(
                build_id = %job.build_id,
                status = ?job.status,
                template_path = %job.template_path,
                "Build job accepted for asynchronous local execution"
            );
            return Ok(job);
        }

        let job = self.submit_to_cloud_build(job).await?;
        info!(
            build_id = %job.build_id,
            status = ?job.status,
            template_path = %job.template_path,
            cloud_build_id = ?job.cloud_build_id,
            cloud_build_name = ?job.cloud_build_name,
            cloud_build_operation_name = ?job.cloud_build_operation_name,
            loaded_to_kind = job.loaded_to_kind,
            "Build job finished"
        );

        self.store_job(job.clone()).await;
        self.spawn_cloud_build_tracking(job.clone());
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

    async fn store_job(&self, job: BuildJob) {
        self.jobs.write().await.insert(job.build_id, job);
    }

    fn spawn_local_build(&self, job: BuildJob) {
        let service = self.clone();

        tokio::spawn(async move {
            let result = service.build_and_load_locally(job.clone()).await;

            match result {
                Ok(completed_job) => {
                    info!(
                        build_id = %completed_job.build_id,
                        status = ?completed_job.status,
                        template_path = %completed_job.template_path,
                        loaded_to_kind = completed_job.loaded_to_kind,
                        "Background local build job completed"
                    );
                    service.store_job(completed_job).await;
                }
                Err(error) => {
                    let mut failed_job = job;
                    failed_job.status = BuildStatus::Failed;
                    failed_job.failure_message = Some(app_error_message(&error));
                    info!(
                        build_id = %failed_job.build_id,
                        error = %error,
                        "Background local build job failed"
                    );
                    service.store_job(failed_job).await;
                }
            }
        });
    }

    fn spawn_cloud_build_tracking(&self, job: BuildJob) {
        let service = self.clone();

        tokio::spawn(async move {
            let result = service.track_cloud_build(job.clone()).await;

            match result {
                Ok(updated_job) => {
                    info!(
                        build_id = %updated_job.build_id,
                        status = ?updated_job.status,
                        cloud_build_id = ?updated_job.cloud_build_id,
                        "Background cloud build tracking completed"
                    );
                    service.store_job(updated_job).await;
                }
                Err(error) => {
                    let mut failed_job = job;
                    failed_job.status = BuildStatus::Failed;
                    failed_job.failure_message = Some(app_error_message(&error));
                    info!(
                        build_id = %failed_job.build_id,
                        error = %error,
                        "Background cloud build tracking failed"
                    );
                    service.store_job(failed_job).await;
                }
            }
        });
    }

    async fn build_and_load_locally(&self, mut job: BuildJob) -> Result<BuildJob, AppError> {
        if !self.config.local_execution_enabled {
            info!(
                build_id = %job.build_id,
                "Local execution is disabled; marking build as ready without docker/kind commands"
            );
            job.status = BuildStatus::Ready;
            job.failure_message = None;
            return Ok(job);
        }

        let archive_path = self
            .resolve_local_archive_path(&job.source_archive_path)
            .await?;
        info!(
            build_id = %job.build_id,
            archive_path = %archive_path.display(),
            "Resolved local source archive path"
        );
        let build_context_dir = self
            .extract_archive_for_local_build(&archive_path, job.build_id)
            .await?;
        info!(
            build_id = %job.build_id,
            build_context_dir = %build_context_dir.display(),
            dockerfile_path = %job.dockerfile_path,
            template_path = %job.template_path,
            "Starting local docker build"
        );

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
        info!(
            build_id = %job.build_id,
            template_path = %job.template_path,
            "Local docker build completed successfully"
        );

        if self.config.local_kind_load_enabled {
            info!(
                build_id = %job.build_id,
                cluster_name = %self.config.local_kind_cluster_name,
                template_path = %job.template_path,
                "Starting kind image load"
            );
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
            info!(
                build_id = %job.build_id,
                cluster_name = %self.config.local_kind_cluster_name,
                template_path = %job.template_path,
                "Kind image load completed successfully"
            );
        }

        job.status = BuildStatus::Ready;
        job.failure_message = None;
        Ok(job)
    }

    async fn extract_archive_for_local_build(
        &self,
        archive_path: &Path,
        build_id: Uuid,
    ) -> Result<PathBuf, AppError> {
        let root_dir = ensure_builder_root_dir(&self.config.bundle_root_dir).await?;
        let local_contexts_dir = join_relative_to_root(&root_dir, Path::new("local-build-contexts"))?;
        let build_id_string = build_id.to_string();
        let destination = join_relative_to_root(&local_contexts_dir, Path::new(&build_id_string))?;

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

        let archive_path = archive_path.to_path_buf();
        let destination_clone = destination.clone();
        tokio::task::spawn_blocking(move || -> Result<(), AppError> {
            info!(
                archive_path = %archive_path.display(),
                destination = %destination_clone.display(),
                "Extracting source archive for local build"
            );
            let archive_file = std::fs::File::open(&archive_path).map_err(|error| {
                AppError::Internal(format!("Failed to open local source archive: {error}"))
            })?;
            let decoder = GzDecoder::new(archive_file);
            let mut archive = Archive::new(decoder);
            archive.unpack(&destination_clone).map_err(|error| {
                AppError::Internal(format!("Failed to extract local source archive: {error}"))
            })?;
            info!(
                archive_path = %archive_path.display(),
                destination = %destination_clone.display(),
                "Source archive extracted successfully"
            );
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
        info!(
            program = %program,
            args = %args.join(" "),
            current_dir = ?current_dir.map(|path| path.display().to_string()),
            "Executing local command"
        );
        let mut command = Command::new(program);
        command.args(args);

        if let Some(current_dir) = current_dir {
            command.current_dir(current_dir);
        }

        let output = command.output().await.map_err(|error| {
            AppError::Internal(format!("Failed to start command `{program}`: {error}"))
        })?;

        if output.status.success() {
            info!(
                program = %program,
                args = %args.join(" "),
                status = %output.status,
                "Local command completed successfully"
            );
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
        let access_token = self.cloud_build_access_token().await?;

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
        job.failure_message = None;
        job.cloud_build_operation_name = payload.name.clone();
        job.cloud_build_id = payload.build_id();
        job.cloud_build_name = payload.build_name();
        job.cloud_build_log_url = payload.build_log_url();

        Ok(job)
    }

    async fn track_cloud_build(&self, mut job: BuildJob) -> Result<BuildJob, AppError> {
        let poll_interval = Duration::from_secs(self.config.cloud_build_poll_interval_seconds.max(1));
        let mut consecutive_poll_errors = 0u8;

        loop {
            let poll_state = match self.fetch_cloud_build_state(&job).await {
                Ok(state) => {
                    consecutive_poll_errors = 0;
                    state
                }
                Err(error) => {
                    consecutive_poll_errors = consecutive_poll_errors.saturating_add(1);
                    info!(
                        build_id = %job.build_id,
                        cloud_build_id = ?job.cloud_build_id,
                        attempt = consecutive_poll_errors,
                        error = %error,
                        "Cloud build status poll failed; retrying"
                    );

                    if consecutive_poll_errors >= 5 {
                        return Err(error);
                    }

                    sleep(poll_interval).await;
                    continue;
                }
            };

            if let Some(snapshot) = poll_state.snapshot {
                job.cloud_build_id = snapshot.id.or(job.cloud_build_id.clone());
                job.cloud_build_name = snapshot.name.or(job.cloud_build_name.clone());
                job.cloud_build_log_url = snapshot.log_url.or(job.cloud_build_log_url.clone());

                if !poll_state.done {
                    let status = snapshot.status.unwrap_or_else(|| "UNKNOWN".into());
                    info!(
                        build_id = %job.build_id,
                        cloud_build_status = %status,
                        cloud_build_id = ?job.cloud_build_id,
                        cloud_build_operation_name = ?job.cloud_build_operation_name,
                        "Cloud build still running"
                    );
                    self.store_job(job.clone()).await;
                    sleep(poll_interval).await;
                    continue;
                }

                match snapshot.status.as_deref() {
                    Some("SUCCESS") => {
                        job.status = BuildStatus::Ready;
                        job.failure_message = None;
                        return Ok(job);
                    }
                    Some("FAILURE" | "INTERNAL_ERROR" | "TIMEOUT" | "CANCELLED" | "EXPIRED") => {
                        job.status = BuildStatus::Failed;
                        job.failure_message = Some(
                            snapshot
                                .status_detail
                                .unwrap_or_else(|| format!("Cloud Build reported status {}", snapshot.status.unwrap_or_else(|| "UNKNOWN".into()))),
                        );
                        return Ok(job);
                    }
                    Some(status) => {
                        job.status = BuildStatus::Failed;
                        job.failure_message = Some(format!(
                            "Cloud Build operation completed with unexpected status {status}"
                        ));
                        return Ok(job);
                    }
                    None => {
                        if let Some(error_message) = poll_state.error_message {
                            job.status = BuildStatus::Failed;
                            job.failure_message = Some(error_message);
                            return Ok(job);
                        }

                        job.status = BuildStatus::Failed;
                        job.failure_message = Some(
                            "Cloud Build operation completed without a build status".into(),
                        );
                        return Ok(job);
                    }
                }
            }

            if !poll_state.done {
                info!(
                    build_id = %job.build_id,
                    cloud_build_id = ?job.cloud_build_id,
                    cloud_build_operation_name = ?job.cloud_build_operation_name,
                    "Cloud build operation still running without build snapshot"
                );
                self.store_job(job.clone()).await;
                sleep(poll_interval).await;
                continue;
            }

            if let Some(error_message) = poll_state.error_message {
                job.status = BuildStatus::Failed;
                job.failure_message = Some(error_message);
                return Ok(job);
            }

            job.status = BuildStatus::Failed;
            job.failure_message = Some(
                "Cloud Build operation completed without a build payload".into(),
            );
            return Ok(job);
        }
    }

    async fn fetch_cloud_build_state(&self, job: &BuildJob) -> Result<CloudBuildPollState, AppError> {
        if job.cloud_build_name.is_some() {
            let snapshot = self.fetch_cloud_build_snapshot(job).await?;
            let done = snapshot
                .status
                .as_deref()
                .is_some_and(is_terminal_cloud_build_status);

            return Ok(CloudBuildPollState {
                snapshot: Some(snapshot),
                done,
                error_message: None,
            });
        }

        if let Some(operation_name) = job.cloud_build_operation_name.as_deref() {
            return self.fetch_cloud_build_operation_state(operation_name).await;
        }

        let snapshot = self.fetch_cloud_build_snapshot(job).await?;
        Ok(CloudBuildPollState {
            snapshot: Some(snapshot),
            done: false,
            error_message: None,
        })
    }

    async fn fetch_cloud_build_operation_state(
        &self,
        operation_name: &str,
    ) -> Result<CloudBuildPollState, AppError> {
        let endpoint = build_cloud_build_operation_get_endpoint(operation_name)?;
        let payload = self
            .cloud_build_get_json(endpoint, "Cloud Build operation status query")
            .await?;

        Ok(CloudBuildPollState {
            snapshot: CloudBuildSnapshot::from_value(&payload),
            done: payload
                .get("done")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            error_message: payload
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
                .map(ToString::to_string),
        })
    }

    async fn fetch_cloud_build_snapshot(&self, job: &BuildJob) -> Result<CloudBuildSnapshot, AppError> {
        let endpoint = build_cloud_build_get_endpoint(
            &self.config.gcp_project_id,
            &self.config.gcp_region,
            job.cloud_build_name.as_deref(),
            job.cloud_build_id.as_deref(),
        )?;
        let payload = self
            .cloud_build_get_json(endpoint, "Cloud Build status query")
            .await?;

        CloudBuildSnapshot::from_value(&payload).ok_or_else(|| {
            AppError::Internal(format!(
                "Cloud Build status response did not contain the expected fields. Body: {}",
                truncate_for_log(&payload.to_string())
            ))
        })
    }

    async fn cloud_build_access_token(&self) -> Result<String, AppError> {
        let token = gcp_auth::provider()
            .await
            .map_err(|error| AppError::Internal(format!("Failed to initialize GCP auth: {error}")))?
            .token(&["https://www.googleapis.com/auth/cloud-platform"])
            .await
            .map_err(|error| {
                AppError::Internal(format!("Failed to obtain Cloud Build token: {error}"))
            })?;

        Ok(token.as_str().to_string())
    }

    async fn cloud_build_get_json(
        &self,
        endpoint: Url,
        request_label: &str,
    ) -> Result<Value, AppError> {
        let access_token = self.cloud_build_access_token().await?;

        let response = self
            .http_client
            .get(endpoint)
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|error| AppError::Internal(format!("Failed to query {request_label}: {error}")))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| {
                AppError::Internal(format!("Failed to read {request_label} response body: {error}"))
            })?;

        if !status.is_success() {
            return Err(AppError::Internal(format!(
                "{request_label} failed with HTTP {status}: {}",
                truncate_for_log(&body)
            )));
        }

        serde_json::from_str(&body).map_err(|error| {
            AppError::Internal(format!(
                "Failed to decode {request_label} response: {error}. Body: {}",
                truncate_for_log(&body)
            ))
        })
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

            let resolved_path = self.resolve_local_archive_path(trimmed).await?;
            let metadata = fs::metadata(&resolved_path).await.map_err(|error| {
                AppError::BadRequest(format!(
                    "source_archive_path must point to an existing local archive: {error}"
                ))
            })?;

            if !metadata.is_file() {
                return Err(AppError::BadRequest(
                    "source_archive_path must point to a file".into(),
                ));
            }

            if resolved_path
                .extension()
                .and_then(|extension| extension.to_str())
                != Some("gz")
                || !resolved_path
                    .to_string_lossy()
                    .to_ascii_lowercase()
                    .ends_with(".tar.gz")
            {
                return Err(AppError::BadRequest(
                    "source_archive_path must point to a .tar.gz archive".into(),
                ));
            }

            return Ok(());
        }

        validate_gcs_path(trimmed)
    }

    async fn resolve_local_archive_path(&self, value: &str) -> Result<PathBuf, AppError> {
        let root_dir = ensure_builder_root_dir(&self.config.bundle_root_dir).await?;
        resolve_existing_path_within_root(&root_dir, value).await
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
struct CloudBuildSnapshot {
    id: Option<String>,
    name: Option<String>,
    status: Option<String>,
    #[serde(rename = "statusDetail")]
    status_detail: Option<String>,
    #[serde(rename = "logUrl")]
    log_url: Option<String>,
}

#[derive(Debug)]
struct CloudBuildPollState {
    snapshot: Option<CloudBuildSnapshot>,
    done: bool,
    error_message: Option<String>,
}

impl CloudBuildSnapshot {
    fn from_value(value: &Value) -> Option<Self> {
        let mut candidates = vec![value];
        if let Some(response) = value.get("response") {
            candidates.push(response);
            if let Some(build) = response.get("build") {
                candidates.push(build);
            }
        }
        if let Some(build) = value.get("build") {
            candidates.push(build);
        }
        if let Some(build) = value.get("metadata").and_then(|metadata| metadata.get("build")) {
            candidates.push(build);
        }

        for candidate in candidates {
            let snapshot = Self {
                id: json_string(candidate, &["id"]),
                name: json_string(candidate, &["name"]),
                status: json_string(candidate, &["status"]),
                status_detail: json_string(candidate, &["statusDetail"]),
                log_url: json_string(candidate, &["logUrl"]),
            };

            if snapshot.id.is_some() || snapshot.name.is_some() || snapshot.status.is_some() {
                return Some(snapshot);
            }
        }

        None
    }
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

fn build_cloud_build_get_endpoint(
    project_id: &str,
    region: &str,
    build_name: Option<&str>,
    build_id: Option<&str>,
) -> Result<Url, AppError> {
    validate_gcp_project_id(project_id)?;
    validate_gcp_region(region)?;

    let (path_segments, resolved_build_id): (Vec<String>, String) = if let Some(name) = build_name {
        let segments = normalize_cloud_build_resource_name(name, "Cloud Build name")?;
        let Some(last_segment) = segments.last().cloned() else {
            return Err(AppError::Internal(
                "Cloud Build name did not contain a build id".into(),
            ));
        };

        (segments, last_segment)
    } else if let Some(id) = build_id {
        let trimmed = id.trim();
        if trimmed.is_empty() {
            return Err(AppError::Internal(
                "Cloud Build id must not be empty when provided".into(),
            ));
        }

        (
            vec![
                "v1".into(),
                "projects".into(),
                project_id.into(),
                "locations".into(),
                region.into(),
                "builds".into(),
                trimmed.into(),
            ],
            trimmed.into(),
        )
    } else {
        return Err(AppError::Internal(
            "Cloud build tracking requires either a build name or a build id".into(),
        ));
    };

    let mut endpoint = Url::parse(CLOUD_BUILD_API_BASE_URL)
        .map_err(|error| AppError::Internal(format!("Invalid Cloud Build base URL: {error}")))?;

    {
        let mut segments = endpoint.path_segments_mut().map_err(|_| {
            AppError::Internal("Cloud Build base URL cannot accept path segments".into())
        })?;
        segments.extend(path_segments);
    }

    endpoint
        .query_pairs_mut()
        .append_pair("projectId", project_id)
        .append_pair("id", &resolved_build_id);

    Ok(endpoint)
}

fn build_cloud_build_operation_get_endpoint(operation_name: &str) -> Result<Url, AppError> {
    let mut endpoint = Url::parse(CLOUD_BUILD_API_BASE_URL)
        .map_err(|error| AppError::Internal(format!("Invalid Cloud Build base URL: {error}")))?;
    let path_segments = normalize_cloud_build_resource_name(
        operation_name,
        "Cloud Build operation name",
    )?;

    {
        let mut segments = endpoint.path_segments_mut().map_err(|_| {
            AppError::Internal("Cloud Build base URL cannot accept path segments".into())
        })?;
        segments.extend(path_segments);
    }

    Ok(endpoint)
}

fn normalize_cloud_build_resource_name(
    value: &str,
    label: &str,
) -> Result<Vec<String>, AppError> {
    let trimmed = value.trim().trim_matches('/');
    if trimmed.is_empty() {
        return Err(AppError::Internal(format!("{label} must not be empty")));
    }

    let mut segments: Vec<String> = trimmed.split('/').map(str::to_string).collect();
    if segments.first().map(String::as_str) != Some("v1") {
        segments.insert(0, "v1".into());
    }

    Ok(segments)
}

fn app_error_message(error: &AppError) -> String {
    match error {
        AppError::BadRequest(message)
        | AppError::NotFound(message)
        | AppError::Internal(message) => message.clone(),
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

fn json_string(value: &Value, path: &[&str]) -> Option<String> {
    let mut current = value;

    for segment in path {
        current = current.get(*segment)?;
    }

    current.as_str().map(ToString::to_string)
}

fn truncate_for_log(value: &str) -> String {
    const MAX_LEN: usize = 400;

    let trimmed = value.trim();
    if trimmed.len() <= MAX_LEN {
        trimmed.to_string()
    } else {
        format!("{}...", &trimmed[..MAX_LEN])
    }
}

fn is_terminal_cloud_build_status(status: &str) -> bool {
    matches!(
        status,
        "SUCCESS" | "FAILURE" | "INTERNAL_ERROR" | "TIMEOUT" | "CANCELLED" | "EXPIRED"
    )
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        path::Path,
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    };

    use tokio::time::{sleep, Duration, Instant};
    use tokio::sync::RwLock;

    use crate::models::{
        build::{BuildDispatchMode, BuildStatus, CreateBuildRequest},
        state::BuilderConfig,
    };

    use super::{
        build_cloud_build_endpoint, build_cloud_build_get_endpoint, build_cloud_build_operation_get_endpoint,
        normalized_service_account, parse_gcs_uri, validate_gcp_project_id, validate_gcp_region,
        BuildsService,
    };

    async fn wait_for_build_status(
        service: &BuildsService,
        build_id: uuid::Uuid,
        expected_status: BuildStatus,
    ) -> crate::models::build::BuildJob {
        let deadline = Instant::now() + Duration::from_secs(2);

        loop {
            let job = service
                .get_build(build_id)
                .await
                .expect("build job should exist while polling");

            if job.status == expected_status {
                return job;
            }

            assert!(
                Instant::now() < deadline,
                "timed out while waiting for build status {:?}, last seen {:?}",
                expected_status,
                job.status
            );

            sleep(Duration::from_millis(10)).await;
        }
    }

    fn test_config() -> BuilderConfig {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be valid")
            .as_nanos();
        let bundle_root_dir = std::env::temp_dir()
            .join(format!("altair-lab-builder-tests-{unique}"))
            .display()
            .to_string();

        BuilderConfig {
            gcp_project_id: "altair-isen".into(),
            gcp_region: "europe-west9".into(),
            artifact_registry_host: "europe-west9-docker.pkg.dev".into(),
            artifact_registry_repo: "altair-labs".into(),
            build_source_bucket: "altair-lab-builds".into(),
            bundle_root_dir,
            cloud_build_timeout_seconds: 1200,
            cloud_build_poll_interval_seconds: 1,
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
        let config = test_config();
        let archive_root = Path::new(&config.bundle_root_dir)
            .join("artifacts")
            .join(unique.to_string());
        std::fs::create_dir_all(&archive_root).expect("archive root should be created");
        let archive_path = archive_root.join("lab-builder-build.tar.gz");
        std::fs::write(&archive_path, b"fake tar gz payload").expect("archive should be written");

        let service = BuildsService::new(config.clone(), Arc::new(RwLock::new(HashMap::new())));

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
        assert_eq!(job.status, BuildStatus::Submitted);
        assert_eq!(job.template_path, "lab-local:v1");
        assert_eq!(
            job.versioned_image_uri,
            "europe-west9-docker.pkg.dev/altair-isen/altair-labs/lab-local:v1"
        );
        assert_eq!(job.source_archive_path, archive_path.display().to_string());
        assert!(!job.loaded_to_kind);

        let completed_job = wait_for_build_status(&service, job.build_id, BuildStatus::Ready).await;
        assert_eq!(completed_job.status, BuildStatus::Ready);
        assert_eq!(completed_job.failure_message, None);

        let _ = std::fs::remove_dir_all(Path::new(&config.bundle_root_dir));
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
        let config = test_config();
        let bundle_root_dir = config.bundle_root_dir.clone();
        let archive_root = Path::new(&config.bundle_root_dir)
            .join("artifacts")
            .join(unique.to_string());
        std::fs::create_dir_all(&archive_root).expect("archive root should be created");
        let archive_path = archive_root.join("lab-builder-cloud.tar.gz");
        std::fs::write(&archive_path, b"fake tar gz payload").expect("archive should be written");

        let mut config = config;
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
        let _ = std::fs::remove_dir_all(bundle_root_dir);
    }

    #[tokio::test]
    async fn create_build_rejects_local_archive_outside_bundle_root() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be valid")
            .as_nanos();
        let config = test_config();
        let outside_archive = std::env::temp_dir().join(format!("outside-builder-{unique}.tar.gz"));
        std::fs::write(&outside_archive, b"fake tar gz payload").expect("archive should be written");

        let service = BuildsService::new(config, Arc::new(RwLock::new(HashMap::new())));

        let result = service
            .create_build(CreateBuildRequest {
                lab_id: Some("lab-local".into()),
                requested_by: Some("creator-local".into()),
                image_name: "lab-local".into(),
                image_tag: None,
                source_archive_path: outside_archive.display().to_string(),
                dockerfile_path: Some("Dockerfile".into()),
            })
            .await;

        assert!(result.is_err());

        let _ = std::fs::remove_file(outside_archive);
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

    #[test]
    fn build_cloud_build_get_endpoint_accepts_build_name() {
        let endpoint = build_cloud_build_get_endpoint(
            "altair-isen",
            "europe-west9",
            Some("projects/390873516222/locations/europe-west9/builds/123"),
            None,
        )
        .expect("endpoint should be valid");

        assert_eq!(
            endpoint.as_str(),
            "https://cloudbuild.googleapis.com/v1/projects/390873516222/locations/europe-west9/builds/123?projectId=altair-isen&id=123"
        );
    }

    #[test]
    fn build_cloud_build_get_endpoint_falls_back_to_build_id() {
        let endpoint = build_cloud_build_get_endpoint(
            "altair-isen",
            "europe-west9",
            None,
            Some("123"),
        )
        .expect("endpoint should be valid");

        assert_eq!(
            endpoint.as_str(),
            "https://cloudbuild.googleapis.com/v1/projects/altair-isen/locations/europe-west9/builds/123?projectId=altair-isen&id=123"
        );
    }

    #[test]
    fn build_cloud_build_operation_get_endpoint_accepts_operation_name_without_v1() {
        let endpoint = build_cloud_build_operation_get_endpoint(
            "operations/build/altair-isen/NmMyNDEwYTItMzBhZC00ZmJmLWE2NzEtZTYwMGEzYjVlODE0",
        )
        .expect("operation endpoint should be valid");

        assert_eq!(
            endpoint.as_str(),
            "https://cloudbuild.googleapis.com/v1/operations/build/altair-isen/NmMyNDEwYTItMzBhZC00ZmJmLWE2NzEtZTYwMGEzYjVlODE0"
        );
    }
}
