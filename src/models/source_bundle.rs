/**
 * @file source_bundle — source upload and packaging models.
 *
 * @remarks
 * Defines the structures used to represent uploaded lab sources,
 * their packaging into bundles, and their linkage to build jobs.
 *
 * Includes:
 *
 *  - Individual uploaded files (`UploadedFile`)
 *  - Aggregated source bundle metadata (`SourceBundle`)
 *  - Combined response for upload + build trigger (`BuildFromUploadResponse`)
 *
 * Key characteristics:
 *
 *  - Tracks all files included in a bundle (path, size)
 *  - Provides archive metadata (size, count, storage paths)
 *  - Bridges upload step with build pipeline execution
 *
 * This module enables the transition from raw user uploads
 * to reproducible build inputs within the Lab Builder system.
 *
 * @packageDocumentation
 */
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
