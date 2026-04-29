#!/usr/bin/env bash
set -euo pipefail

BASE_URL="${LAB_BUILDER_BASE_URL:-http://localhost:8086}"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

response="$(curl -sS -X POST "${BASE_URL}/builds/from-upload" \
  -F "lab_id=lab-poc-1" \
  -F "lab_name=Lab POC 1" \
  -F "requested_by=creator-1" \
  -F "file=@${ROOT_DIR}/examples/basic-terminal-lab/Dockerfile;filename=Dockerfile" \
  -F "file=@${ROOT_DIR}/examples/basic-terminal-lab/app/start.sh;filename=app/start.sh")"

template_path="$(printf '%s' "${response}" | sed -n 's/.*"template_path":"\([^"]*\)".*/\1/p')"

if [[ -z "${template_path}" ]]; then
  echo "Failed to extract template_path from /builds/from-upload response"
  echo "${response}"
  exit 1
fi

echo "Local build response:"
printf '%s\n' "${response}"
echo
echo "Derived template_path: ${template_path}"
