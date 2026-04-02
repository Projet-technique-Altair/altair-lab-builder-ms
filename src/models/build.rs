use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BuildStatus {
    Queued,
    Submitted,
    Ready,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BuildDispatchMode {
    LocalDockerKind,
    CloudBuild,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateBuildRequest {
    pub lab_id: Option<String>,
    pub requested_by: Option<String>,
    pub image_name: String,
    pub image_tag: Option<String>,
    #[serde(alias = "source_archive_gcs_path")]
    pub source_archive_path: String,
    pub dockerfile_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildJob {
    pub build_id: Uuid,
    pub lab_id: Option<String>,
    pub requested_by: Option<String>,
    pub status: BuildStatus,
    pub failure_message: Option<String>,
    pub dispatch_mode: BuildDispatchMode,
    pub image_name: String,
    pub image_tag: String,
    pub template_path: String,
    pub source_archive_path: String,
    pub dockerfile_path: String,
    pub gcp_region: String,
    pub build_source_bucket: String,
    pub local_kind_cluster_name: Option<String>,
    pub loaded_to_kind: bool,
    pub cloud_build_id: Option<String>,
    pub cloud_build_name: Option<String>,
    pub cloud_build_operation_name: Option<String>,
    pub cloud_build_log_url: Option<String>,
    pub versioned_image_uri: String,
    pub latest_image_uri: String,
    pub created_at: DateTime<Utc>,
}
