/**
 * @file path_safety — secure filesystem path handling utilities.
 *
 * @remarks
 * Provides helper functions to safely manipulate and validate filesystem paths
 * within the Lab Builder workspace, preventing path traversal and unauthorized access.
 *
 * Includes:
 *
 *  - Root directory initialization and validation (`ensure_builder_root_dir`)
 *  - Safe path joining within a restricted root (`join_relative_to_root`)
 *  - Resolution and validation of existing paths (`resolve_existing_path_within_root`)
 *
 * Key characteristics:
 *
 *  - Enforces strict confinement to a predefined root directory
 *  - Prevents directory traversal attacks (e.g. `../`)
 *  - Requires absolute and canonicalized paths for critical operations
 *  - Ensures all accessed files exist and are within the allowed workspace
 *
 * This module is critical for securing file operations in the build pipeline,
 * especially when handling user-provided paths and uploaded archives.
 *
 * @packageDocumentation
 */

use std::path::{Path, PathBuf};

use tokio::fs;

use crate::error::AppError;

pub async fn ensure_builder_root_dir(value: &str) -> Result<PathBuf, AppError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(AppError::Internal(
            "LAB_BUNDLE_ROOT_DIR must not be empty".into(),
        ));
    }

    let candidate = PathBuf::from(trimmed);
    if !candidate.is_absolute() {
        return Err(AppError::Internal(
            "LAB_BUNDLE_ROOT_DIR must be an absolute path".into(),
        ));
    }

    fs::create_dir_all(&candidate)
        .await
        .map_err(|error| AppError::Internal(format!("Failed to prepare builder root dir: {error}")))?;

    fs::canonicalize(&candidate)
        .await
        .map_err(|error| AppError::Internal(format!("Failed to resolve builder root dir: {error}")))
}

pub fn join_relative_to_root(root: &Path, relative: &Path) -> Result<PathBuf, AppError> {
    if relative.is_absolute() {
        return Err(AppError::BadRequest(
            "Path must stay relative to the builder workspace".into(),
        ));
    }

    let candidate = root.join(relative);
    if candidate.starts_with(root) {
        Ok(candidate)
    } else {
        Err(AppError::BadRequest(
            "Path escapes the builder workspace".into(),
        ))
    }
}

pub async fn resolve_existing_path_within_root(
    root: &Path,
    value: &str,
) -> Result<PathBuf, AppError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(AppError::BadRequest("Path must not be empty".into()));
    }

    let canonical = fs::canonicalize(trimmed)
        .await
        .map_err(|error| AppError::BadRequest(format!("Path must exist: {error}")))?;

    if canonical.starts_with(root) {
        Ok(canonical)
    } else {
        Err(AppError::BadRequest(
            "Path must stay within the builder root directory".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::{
        path::Path,
        time::{SystemTime, UNIX_EPOCH},
    };

    use tokio::fs;

    use super::{ensure_builder_root_dir, join_relative_to_root, resolve_existing_path_within_root};

    #[test]
    fn join_relative_to_root_rejects_absolute_paths() {
        let root = Path::new("/tmp/altair-builder");
        let result = join_relative_to_root(root, Path::new("/etc/passwd"));
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn ensure_builder_root_dir_rejects_relative_root() {
        let result = ensure_builder_root_dir("relative/path").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn resolve_existing_path_within_root_rejects_outside_file() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be valid")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("lab-builder-root-{unique}"));
        let outside = std::env::temp_dir().join(format!("lab-builder-outside-{unique}.txt"));

        fs::create_dir_all(&root).await.expect("root should be created");
        fs::write(&outside, b"outside").await.expect("outside file should be created");

        let canonical_root = ensure_builder_root_dir(root.to_str().expect("root path")).await
            .expect("root should resolve");
        let result = resolve_existing_path_within_root(
            &canonical_root,
            outside.to_str().expect("outside path"),
        )
        .await;

        assert!(result.is_err());

        let _ = fs::remove_dir_all(root).await;
        let _ = fs::remove_file(outside).await;
    }

    #[tokio::test]
    async fn resolve_existing_path_within_root_accepts_inside_file() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be valid")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("lab-builder-root-{unique}"));
        let inside = root.join("artifacts").join("source.tar.gz");

        fs::create_dir_all(inside.parent().expect("parent should exist"))
            .await
            .expect("parent should be created");
        fs::write(&inside, b"archive").await.expect("inside file should be created");

        let canonical_root = ensure_builder_root_dir(root.to_str().expect("root path")).await
            .expect("root should resolve");
        let resolved = resolve_existing_path_within_root(
            &canonical_root,
            inside.to_str().expect("inside path"),
        )
        .await
        .expect("inside path should resolve");

        assert!(resolved.starts_with(&canonical_root));

        let _ = fs::remove_dir_all(root).await;
    }
}
