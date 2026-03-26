# Altair Lab Builder MS

Stateless microservice dedicated to the build side of lab creation.

## Purpose

This service is responsible for turning user-provided lab files into a runnable
container image.

It exists to isolate build orchestration from the rest of the platform:

- `altair-labs-ms` manages lab metadata and pedagogy
- `altair-lab-api-service` manages lab runtime orchestration
- `altair-lab-builder-ms` manages build preparation and image production

## What The Service Does

The builder receives lab files, prepares a clean build context, and produces a
`template_path` that can be stored by the platform and used later to run the lab.

Core responsibilities:

- accept uploaded lab files
- create a temporary workspace
- generate a `source.tar.gz`
- build an image locally for development and PoC flows
- load the image into `kind` in local mode
- submit a remote Cloud Build job in non-local mode
- compute and return the final `template_path`
- expose build job state

## What The Service Does Not Do

The builder is intentionally narrow in scope.

It does not:

- store labs in the main database
- manage the lab catalog
- decide business publication rules
- launch labs on the runtime platform
- manage long-term persistence of build jobs

## High-Level Workflow

### Local mode

Local mode is meant for development and fast PoC validation.

1. receive uploaded files
2. write them into a temporary workspace
3. generate `source.tar.gz`
4. extract the archive into a local build context
5. run `docker build`
6. tag the image as `<image_name>:<image_tag>`
7. optionally load the image into the configured `kind` cluster
8. return a local `template_path`

In local mode, the returned `template_path` is a simple local image reference:

```text
<image_name>:<image_tag>
```

Example:

```text
lab-poc-1:v1
```

### Non-local mode

Non-local mode is meant for the real remote build flow.

1. receive uploaded files
2. write them into a temporary workspace
3. generate `source.tar.gz`
4. upload the archive to object storage
5. submit a Cloud Build job using that archive as source
6. build and push the image to the labs registry
7. return the versioned image URI as `template_path`

In non-local mode, the returned `template_path` must follow this format:

```text
REGION-docker.pkg.dev/PROJECT/LABS_REPO/LAB_NAME:TAG
```

Example:

```text
europe-west9-docker.pkg.dev/PROJECT/altair-labs/lab-poc-1:v1
```

## Recommended Entry Point

For frontend integration, the main entry point is:

```text
POST /builds/from-upload
```

This endpoint works for both local and non-local execution.

It lets the caller send the lab files once and receive:

- source bundle metadata
- build job metadata
- the computed `template_path`

That makes it the cleanest endpoint for a "Create lab" workflow.

## API

### `GET /health`

Returns a lightweight health payload and the current execution mode.

### `POST /source-bundles`

Accepts a `multipart/form-data` upload, writes the received files to a temporary
workspace, and generates a `source.tar.gz`.

Returned data includes:

- workspace location
- archive location
- generated file list
- suggested remote archive path

This endpoint is useful when the caller wants to separate:

- source preparation
- build submission

### `POST /builds`

Creates a build job from an existing archive path.

Behavior:

- in local mode, the archive path must point to a local `.tar.gz`
- in non-local mode, the archive path must point to a remote object path
- the service computes the image URIs
- the service returns a build job with the resolved `template_path`

### `POST /builds/from-upload`

One-step endpoint that combines:

- file upload
- source bundle generation
- build execution

This is the endpoint intended for end-to-end builder integration.

### `GET /builds/{build_id}`

Returns the current in-memory representation of a previously created build job.

## Naming Rules

The service can derive the image name automatically.

Priority order:

1. `image_name`
2. `lab_name`
3. `lab_id`

The resolved image name is normalized so it can be used safely as a container
image name.

Example:

- `Lab POC 1` becomes `lab-poc-1`

## Returned `template_path`

This is the most important output of the service.

The builder exists to produce a `template_path` that the rest of the platform
can store and reuse.

### Local mode

```text
lab-poc-1:v1
```

