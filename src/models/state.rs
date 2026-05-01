/**
 * @file state — application state and configuration.
 *
 * @remarks
 * Defines the global application state for the Lab Builder service,
 * including configuration and service initialization.
 *
 * Includes:
 *
 *  - Runtime configuration (`BuilderConfig`)
 *  - Shared application state (`State`)
 *  - Environment-based configuration loading
 *
 * Key characteristics:
 *
 *  - Centralizes all infrastructure and build-related configuration
 *  - Supports both local and cloud execution modes
 *  - Initializes core services (builds, source bundles)
 *  - Uses thread-safe shared state for build job tracking
 *
 * Configuration is primarily sourced from environment variables,
 * with sensible defaults to allow local development and testing.
 *
 * This module acts as the entry point for dependency wiring
 * across the Lab Builder service.
 *
 * @packageDocumentation
 */
use std::{collections::HashMap, sync::Arc};

use tokio::sync::{RwLock, Semaphore};

use crate::services::{builds::BuildsService, source_bundles::SourceBundlesService};

#[derive(Debug, Clone)]
pub struct BuilderConfig {
    pub gcp_project_id: String,
    pub gcp_region: String,
    pub artifact_registry_host: String,
    pub artifact_registry_repo: String,
    pub build_source_bucket: String,
    pub bundle_root_dir: String,
    pub cloud_build_timeout_seconds: u64,
    pub cloud_build_poll_interval_seconds: u64,
    pub cloud_build_service_account: Option<String>,
    pub cloud_build_logs_bucket: Option<String>,
    pub local_execution_enabled: bool,
    pub local_docker_binary: String,
    pub local_kind_binary: String,
    pub local_kind_cluster_name: String,
    pub local_kind_load_enabled: bool,
    pub local_mode: bool,
    pub max_upload_files: usize,
    pub max_upload_file_bytes: usize,
    pub max_upload_total_bytes: usize,
    pub max_text_field_bytes: usize,
    pub max_archive_entries: usize,
    pub max_archive_uncompressed_bytes: u64,
    pub max_concurrent_builds: usize,
}

#[derive(Clone)]
pub struct State {
    pub builds_service: BuildsService,
    pub source_bundles_service: SourceBundlesService,
}

impl State {
    pub fn from_env() -> Self {
        let config = BuilderConfig {
            gcp_project_id: env_or_default("GCP_PROJECT_ID", "altair-isen"),
            gcp_region: env_or_default("GCP_REGION", "europe-west9"),
            artifact_registry_host: env_or_default(
                "ARTIFACT_REGISTRY_HOST",
                "europe-west9-docker.pkg.dev",
            ),
            artifact_registry_repo: env_or_default("ARTIFACT_REGISTRY_REPO", "altair-labs"),
            build_source_bucket: env_or_default("LAB_BUILD_SOURCE_BUCKET", "altair-lab-builds"),
            bundle_root_dir: env_or_default("LAB_BUNDLE_ROOT_DIR", "/tmp/altair-lab-builder"),
            cloud_build_timeout_seconds: env_u64_or_default("CLOUD_BUILD_TIMEOUT_SECONDS", 1200),
            cloud_build_poll_interval_seconds: env_u64_or_default(
                "CLOUD_BUILD_POLL_INTERVAL_SECONDS",
                5,
            ),
            cloud_build_service_account: optional_env("CLOUD_BUILD_SERVICE_ACCOUNT"),
            cloud_build_logs_bucket: optional_env("CLOUD_BUILD_LOGS_BUCKET"),
            local_execution_enabled: parse_bool_env("LAB_BUILDER_LOCAL_EXECUTION_ENABLED", true),
            local_docker_binary: env_or_default("LAB_BUILDER_LOCAL_DOCKER_BINARY", "docker"),
            local_kind_binary: env_or_default("LAB_BUILDER_LOCAL_KIND_BINARY", "kind"),
            local_kind_cluster_name: env_or_default(
                "LAB_BUILDER_LOCAL_KIND_CLUSTER_NAME",
                "altair",
            ),
            local_kind_load_enabled: parse_bool_env("LAB_BUILDER_LOCAL_KIND_LOAD_ENABLED", true),
            local_mode: parse_bool_env("LAB_BUILDER_LOCAL_MODE", true),
            max_upload_files: env_usize_or_default("LAB_BUILDER_MAX_UPLOAD_FILES", 200),
            max_upload_file_bytes: env_usize_or_default(
                "LAB_BUILDER_MAX_UPLOAD_FILE_BYTES",
                10 * 1024 * 1024,
            ),
            max_upload_total_bytes: env_usize_or_default(
                "LAB_BUILDER_MAX_UPLOAD_TOTAL_BYTES",
                50 * 1024 * 1024,
            ),
            max_text_field_bytes: env_usize_or_default("LAB_BUILDER_MAX_TEXT_FIELD_BYTES", 4096),
            max_archive_entries: env_usize_or_default("LAB_BUILDER_MAX_ARCHIVE_ENTRIES", 2000),
            max_archive_uncompressed_bytes: env_u64_or_default(
                "LAB_BUILDER_MAX_ARCHIVE_UNCOMPRESSED_BYTES",
                250 * 1024 * 1024,
            ),
            max_concurrent_builds: env_usize_or_default("LAB_BUILDER_MAX_CONCURRENT_BUILDS", 2),
        };

        let jobs = Arc::new(RwLock::new(HashMap::new()));
        let build_slots = Arc::new(Semaphore::new(config.max_concurrent_builds.max(1)));
        let builds_service = BuildsService::new(config.clone(), jobs.clone(), build_slots);
        let source_bundles_service = SourceBundlesService::new(config);

        Self {
            builds_service,
            source_bundles_service,
        }
    }
}

fn env_or_default(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_u64_or_default(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default)
}

fn env_usize_or_default(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn optional_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn parse_bool_env(key: &str, default: bool) -> bool {
    std::env::var(key)
        .ok()
        .map(|value| {
            let normalized = value.trim().to_ascii_lowercase();
            matches!(normalized.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(default)
}
