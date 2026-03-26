# Altair Lab Builder MS

> Stateless microservice dedicated to the lab creation pipeline.

## Role

This service is responsible for the build side of lab creation:

- validate a lab build request
- compute the target Artifact Registry image URIs
- track build jobs for the PoC
- provide a clean boundary before integrating GCS upload and Cloud Build

For the first PoC, the service is intentionally small:

- no database
- in-memory job store only
- no file upload endpoint yet

The idea is to stabilize the API contract first, then plug the real build chain behind it.

## Planned Responsibilities

Target chain for the next iterations:

1. receive a build request after file upload
2. point to a prepared `source.tar.gz` in GCS
3. call Cloud Build
4. push the runtime image to Artifact Registry
5. expose build status back to the platform

## Current API

### `GET /health`
Returns a basic health payload.

### `POST /builds`
Creates a build job in memory and returns the computed image URIs.

Behavior:

- if `LAB_BUILDER_LOCAL_MODE=true`, the job is stored as a local stub
- otherwise, the service submits a real Cloud Build job

Example payload:

```json
{
  "lab_id": "lab-poc-1",
  "requested_by": "creator-123",
  "image_name": "lab-poc-1",
  "image_tag": "v1",
  "source_archive_gcs_path": "gs://altair-lab-builds/builds/lab-poc-1/v1/source.tar.gz",
  "dockerfile_path": "Dockerfile"
}
```

### `GET /builds/:build_id`
Returns the in-memory representation of a previously created job.

## Environment Variables

```bash
PORT=8086
RUST_LOG=info
LAB_BUILDER_LOCAL_MODE=true
GCP_PROJECT_ID=altair-isen
GCP_REGION=europe-west9
ARTIFACT_REGISTRY_HOST=europe-west9-docker.pkg.dev
ARTIFACT_REGISTRY_REPO=altair-repo
LAB_BUILD_SOURCE_BUCKET=altair-lab-builds
CLOUD_BUILD_TIMEOUT_SECONDS=1200
# Optional:
# CLOUD_BUILD_SERVICE_ACCOUNT=projects/altair-isen/serviceAccounts/build-sa@altair-isen.iam.gserviceaccount.com
# CLOUD_BUILD_LOGS_BUCKET=gs://altair-cloudbuild-logs
```

## Why a Separate Microservice

Keeping this chain outside `altair-labs-ms` and `altair-lab-api-service` is cleaner:

- `altair-labs-ms` stays focused on catalog and pedagogy
- `altair-lab-api-service` stays focused on runtime orchestration
- this service owns build orchestration only

That separation matches the architecture discussed for the lab creation PoC.
