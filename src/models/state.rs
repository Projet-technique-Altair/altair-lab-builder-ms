use std::{collections::HashMap, sync::Arc};

use tokio::sync::RwLock;

use crate::services::builds::BuildsService;

#[derive(Debug, Clone)]
pub struct BuilderConfig {
    pub gcp_project_id: String,
    pub gcp_region: String,
    pub artifact_registry_host: String,
    pub artifact_registry_repo: String,
    pub build_source_bucket: String,
    pub cloud_build_timeout_seconds: u64,
    pub cloud_build_service_account: Option<String>,
    pub cloud_build_logs_bucket: Option<String>,
    pub local_mode: bool,
}

#[derive(Clone)]
pub struct State {
    pub builds_service: BuildsService,
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
            artifact_registry_repo: env_or_default("ARTIFACT_REGISTRY_REPO", "altair-repo"),
            build_source_bucket: env_or_default("LAB_BUILD_SOURCE_BUCKET", "altair-lab-builds"),
            cloud_build_timeout_seconds: env_u64_or_default("CLOUD_BUILD_TIMEOUT_SECONDS", 1200),
            cloud_build_service_account: optional_env("CLOUD_BUILD_SERVICE_ACCOUNT"),
            cloud_build_logs_bucket: optional_env("CLOUD_BUILD_LOGS_BUCKET"),
            local_mode: parse_bool_env("LAB_BUILDER_LOCAL_MODE", true),
        };

        let jobs = Arc::new(RwLock::new(HashMap::new()));
        let builds_service = BuildsService::new(config.clone(), jobs.clone());

        Self { builds_service }
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
