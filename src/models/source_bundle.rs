use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::models::build::BuildJob;

#[derive(Debug, Clone, Serialize)]
pub struct UploadedFile {
    pub path: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceBundle {
    pub bundle_id: Uuid,
    pub lab_id: Option<String>,
    pub requested_by: Option<String>,
    pub workspace_dir: String,
    pub archive_path: String,
    pub suggested_gcs_path: String,
    pub archive_size_bytes: u64,
    pub file_count: usize,
    pub files: Vec<UploadedFile>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BuildFromUploadResponse {
    pub source_bundle: SourceBundle,
    pub build_job: BuildJob,
}
