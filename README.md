# Altair Lab Builder MS

Stateless microservice dedicated to the build side of lab creation.

## Purpose

`altair-lab-builder-ms` turns uploaded lab files into a runnable container image
and returns the resulting `template_path`.

It exists to keep build orchestration separate from the rest of the platform:

- `altair-labs-ms` manages lab metadata and pedagogy
- `altair-lab-api-service` manages runtime orchestration
- `altair-lab-builder-ms` prepares sources and produces images

## What The Service Does

The builder:

- accepts uploaded lab files
- writes them into a temporary workspace under `LAB_BUNDLE_ROOT_DIR`
- creates a `source.tar.gz`
- builds an image locally in development mode
- optionally loads that image into a local Kind cluster
- uploads the archive to GCS in remote mode
- submits a Cloud Build job in remote mode
- computes and returns the final `template_path`
- exposes the current in-memory state of build jobs

## What The Service Does Not Do

The builder does not:

- store labs in the main database
- manage the lab catalog
- decide publication rules
- start or orchestrate learner runtime sessions
- persist build jobs beyond the current process lifetime

## High-Level Workflow

### Local mode

Local mode is intended for development and PoC validation.

1. receive uploaded files
2. write them into a temporary workspace
3. generate `source.tar.gz`
4. extract the archive into a local build context
5. run `docker build`
6. tag the image as `<image_name>:<image_tag>`
7. optionally load the image into the configured Kind cluster
8. return a local `template_path`

In local mode, the returned `template_path` is:

```text
<image_name>:<image_tag>
```

Example:

```text
lab-poc-1:v1
```

### Remote mode

Remote mode is intended for the real Cloud Build flow.

1. receive uploaded files
2. write them into a temporary workspace
3. generate `source.tar.gz`
4. upload the archive to GCS
5. submit a Cloud Build job using that archive as the source
6. build and push the image to Artifact Registry
7. return the versioned image URI as `template_path`

In remote mode, the returned `template_path` follows this format:

```text
REGION-docker.pkg.dev/PROJECT/REPOSITORY/IMAGE:TAG
```

Example:

```text
europe-west9-docker.pkg.dev/altair-isen/altair-labs/lab-poc-1:v1
```

## Recommended Entry Point

For platform integration, the main entry point is:

```text
POST /builds/from-upload
```

This endpoint combines:

- source bundle creation
- archive upload if needed
- build execution

It returns:

- source bundle metadata
- build job metadata
- the computed `template_path`

This is the cleanest endpoint for the creator flow.

## Current Platform Flow

In cloud mode, the builder can reuse a previous ready image when the uploaded
build context has the same `source_context_hash`, `dockerfile_path`, and
`image_tag`. Reuse is first checked in memory, then optionally in the Labs
database when `LAB_BUILD_ARTIFACTS_DATABASE_URL`, `LABS_MS_DATABASE_URL`, or
`LABS_DATABASE_URL` is configured.

The persistent cache stores image build artifacts only. The canonical lab
metadata and active `template_path` still belong to `labs-ms`.

The current flow is:

```text
frontend -> gateway -> lab-builder
frontend -> gateway -> labs-ms
```

More concretely:

1. the creator frontend calls the builder
2. the builder returns a `template_path`
3. the frontend includes that `template_path` in the lab creation payload
4. `altair-labs-ms` stores it in the labs database

## API

### `GET /health`

Returns:

- `status`
- `local_mode`

### `POST /source-bundles`

Accepts `multipart/form-data`, writes uploaded files into a temporary
workspace, and creates a `source.tar.gz`.

Returned data includes:

- `bundle_id`
- `lab_id`
- `requested_by`
- `workspace_dir`
- `archive_path`
- `suggested_gcs_path`
- `archive_size_bytes`
- `file_count`
- `files`
- `created_at`

This endpoint is useful when the caller wants to split:

- source preparation
- build submission

### `POST /builds`

Creates a build job from an existing archive path.

Request body:

```json
{
  "lab_id": "optional-lab-id",
  "requested_by": "optional-user-id",
  "image_name": "lab-poc-1",
  "image_tag": "v1",
  "source_archive_path": "gs://bucket/path/source.tar.gz",
  "dockerfile_path": "Dockerfile"
}
```

Behavior:

- in local mode, `source_archive_path` must be a local `.tar.gz` archive
- in remote mode, `source_archive_path` must be a `gs://` path
- `image_tag` defaults to `v1`
- `dockerfile_path` defaults to `Dockerfile`

### `POST /builds/from-upload`

One-step endpoint that combines:

- file upload
- source bundle generation
- optional GCS upload
- build submission

Accepted multipart text fields:

- `lab_id`
- `lab_name`
- `requested_by`
- `image_name`
- `image_tag`
- `dockerfile_path`

Accepted file fields:

- any multipart part with a filename is treated as an uploaded lab file

Image name resolution priority:

1. `image_name`
2. `lab_name`
3. `lab_id`

### `GET /builds/{build_id}`

Returns the current in-memory representation of a previously created build job.

Build jobs are stored only in process memory. If the service restarts, previous
jobs are no longer available from this endpoint. Ready build artifacts may still
be reused through the optional persistent cache.

## Build Job Model

Returned build jobs include:

- `build_id`
- `lab_id`
- `requested_by`
- `status`
- `dispatch_mode`
- `image_name`
- `image_tag`
- `template_path`
- `source_archive_path`
- `dockerfile_path`
- `source_context_hash`
- `gcp_region`
- `build_source_bucket`
- `local_kind_cluster_name`
- `loaded_to_kind`
- `cloud_build_id`
- `cloud_build_name`
- `cloud_build_operation_name`
- `cloud_build_log_url`
- `versioned_image_uri`
- `latest_image_uri`
- `created_at`

Supported statuses:

- `QUEUED`
- `SUBMITTED`
- `READY`

Supported dispatch modes:

- `LOCAL_DOCKER_KIND`
- `CLOUD_BUILD`

## Naming Rules

The service normalizes the image name so it can be used as a container image
name.

Example:

- `Lab SQLi Guided` becomes `lab-sqli-guided`
- `CTF.Web 101` becomes `ctf.web-101`

## Configuration

The service reads its configuration from environment variables.

### Core GCP and registry configuration

- `GCP_PROJECT_ID`
- `GCP_REGION`
- `ARTIFACT_REGISTRY_HOST`
- `ARTIFACT_REGISTRY_REPO`
- `LAB_BUILD_SOURCE_BUCKET`
- `LAB_BUILD_ARTIFACTS_DATABASE_URL`
- `LABS_MS_DATABASE_URL` / `LABS_DATABASE_URL` (fallbacks for the same cache)
- `CLOUD_BUILD_TIMEOUT_SECONDS`
- `CLOUD_BUILD_SERVICE_ACCOUNT`
- `CLOUD_BUILD_LOGS_BUCKET`

### Local workspace and execution configuration

- `LAB_BUNDLE_ROOT_DIR`
- `LAB_BUILDER_LOCAL_MODE`
- `LAB_BUILDER_LOCAL_EXECUTION_ENABLED`
- `LAB_BUILDER_LOCAL_DOCKER_BINARY`
- `LAB_BUILDER_LOCAL_KIND_BINARY`
- `LAB_BUILDER_LOCAL_KIND_CLUSTER_NAME`
- `LAB_BUILDER_LOCAL_KIND_LOAD_ENABLED`

### Network configuration

- `PORT`

Default port:

```text
8086
```

## Current Local Defaults

For local development, the current defaults are:

- `LAB_BUILDER_LOCAL_MODE=true`
- `LAB_BUILDER_LOCAL_EXECUTION_ENABLED=true`
- `LAB_BUILDER_LOCAL_KIND_LOAD_ENABLED=true`
- `LAB_BUILDER_LOCAL_KIND_CLUSTER_NAME=altair`
- `LAB_BUNDLE_ROOT_DIR=/tmp/altair-lab-builder`
- `PORT=8086`

Local development expects:

- Docker available on the host
- Kind available on the host
- a local Kind cluster named `altair`

## Returned `template_path`

This is the most important output of the service.

The builder exists to produce a `template_path` that the rest of the platform
can store and reuse.

### Local mode

```text
lab-poc-1:v1
```

### Remote mode

```text
europe-west9-docker.pkg.dev/altair-isen/altair-labs/lab-poc-1:v1
```
## May 2026 Security And Platform Updates

- Runtime Docker image now installs only required packages with `--no-install-recommends` and runs as non-root UID `10001`.
- The sample `examples/basic-terminal-lab` image also runs as non-root UID `10001`.
- CORS origin handling is now allowlist-based through `ALLOWED_ORIGINS`; local defaults are `http://localhost:5173,http://localhost:3000`.
- Cloud Build and GCS access tokens are obtained from runtime identity. Do not commit service account keys or access tokens.
- Latest Trivy scan status for this repo: no HIGH or CRITICAL findings.
## Builder Guardrails Update

- Runtime container runs as non-root user `10001`.
- CORS is allowlisted through `ALLOWED_ORIGINS`.
- Trivy HIGH/CRITICAL scan is clean for this service image after the Dockerfile hardening pass.
- Uploads and builds have application-level guardrails:
  - `LAB_BUILDER_MAX_UPLOAD_FILES` limits files per multipart upload (`200` by default).
  - `LAB_BUILDER_MAX_UPLOAD_FILE_BYTES` limits one uploaded file (`10485760` by default).
  - `LAB_BUILDER_MAX_UPLOAD_TOTAL_BYTES` limits the whole upload (`52428800` by default).
  - `LAB_BUILDER_MAX_TEXT_FIELD_BYTES` limits multipart text fields (`4096` by default).
  - `LAB_BUILDER_MAX_CONCURRENT_BUILDS` limits local/cloud builds running at once (`2` by default).
  - `LAB_BUILDER_MAX_ARCHIVE_ENTRIES` and `LAB_BUILDER_MAX_ARCHIVE_UNCOMPRESSED_BYTES` protect local archive extraction.
- Local `.tar.gz` build archives are extracted entry by entry and reject unsupported archive entry types instead of blindly unpacking the archive.
- The service stays split into `routes/`, `services/`, and `models/`; `services/builds.rs` remains the orchestration owner for Cloud Build and local Docker lifecycle.
